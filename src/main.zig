//! Read readsb `--net-json-port` and write one UART frame per position.

const std = @import("std");
const frame = @import("frame.zig");
const json = @import("json.zig");

const c = std.c;
const posix = std.posix;
const linux = std.os.linux;
const print = std.debug.print;
const ns_per_ms = std.time.ns_per_ms;
const ns_per_s = std.time.ns_per_s;

const default_uart = "/dev/serial0";
const default_baud: u32 = 115_200;
const default_interval_s: f64 = 1.0;
const min_interval_s: f64 = 0.2;
const poll_slice_ns = 200 * ns_per_ms;
const connect_slice_ns = 50 * ns_per_ms;
const sleep_step_ns = 50 * ns_per_ms;
/// Finished frames waiting on the UART. The frame being written sits beside this.
const queue_cap = 64;
const connect_timeout_ns = 5 * ns_per_s;
const retry_min_ns = 1 * ns_per_s;
const retry_max_ns = 30 * ns_per_s;
const keepalive_idle_s = 30;
const keepalive_interval_s = 10;
const keepalive_probes = 3;

/// Set by SIGINT. Every wait below is sliced, so a stop is seen within one slice.
var stop: std.atomic.Value(bool) = .init(false);

fn stopping() bool {
    return stop.load(.monotonic);
}

pub fn main(init: std.process.Init.Minimal) u8 {
    ignoreSigpipe();
    var usage: Usage = undefined;
    const options = parseArgs(init.args.vector, &usage) catch |err| switch (err) {
        error.Help => {
            writeAll(posix.STDOUT_FILENO, help);
            return 0;
        },
        error.Usage => {
            printUsage(usage);
            return 2;
        },
    };
    var writer: Writer = .{ .sink = if (std.mem.eql(u8, options.uart, "-"))
        .stdout
    else
        .{ .uart = openUart(options.uart, options.baud) orelse return 1 } };
    catchSigint();

    switch (options.mode) {
        .check => {
            print("check uart={s} baud={d}\n", .{ options.uart, options.baud });
            return runCheck(checkIntervalNs(options.interval_s), &writer);
        },
        .net_json => |text| {
            const addresses = resolve(text) orelse {
                print("net-json {s}: expected host:port\n", .{text});
                return 1;
            };
            print("net-json={s} uart={s} baud={d}\n", .{ text, options.uart, options.baud });
            return runPort(addresses, text, &writer);
        },
    }
}

const help =
    \\Send each readsb position to a UART as it arrives.
    \\
    \\Usage: trck [OPTIONS]
    \\
    \\Options:
    \\      --net-json <HOST:PORT>  Host and port of readsb `--net-json-port`. Required unless `--check`
    \\      --uart <UART>           UART device, or - for stdout [default: /dev/serial0]
    \\      --baud <BAUD>           [default: 115200]
    \\      --interval <INTERVAL>   Seconds between passes of `--check`. The JSON port does not wait [default: 1]
    \\      --check                 Send the known frames instead of reading readsb, to check the UART
    \\  -h, --help                  Print help
    \\
;

/// One run reads readsb or checks the UART, never both.
const Mode = union(enum) {
    check,
    net_json: []const u8,
};

const Options = struct {
    mode: Mode,
    uart: [:0]const u8,
    baud: u32,
    interval_s: f64,
};

const check_arg = "--check";
const net_json_arg = "--net-json <HOST:PORT>";
const uart_arg = "--uart <UART>";
const baud_arg = "--baud <BAUD>";
const interval_arg = "--interval <INTERVAL>";

/// Why the command line was refused. The message text is part of the CLI. The exit code is 2.
const Usage = union(enum) {
    unexpected_argument: []const u8,
    unexpected_value: struct { flag: []const u8, value: []const u8 },
    missing_value: []const u8,
    invalid_value: struct { option: []const u8, value: []const u8, reason: ValueError },
    repeated: []const u8,
    conflict: struct { first: []const u8, second: []const u8 },
    missing_mode,
};

const ValueError = error{ EmptyInteger, InvalidDigit, TooLarge, TooSmall, OutOfRange, EmptyFloat, InvalidFloat };

/// Left to right: `-h` wins over what follows it, a bad argument stops the parse,
/// and the `--check`/`--net-json` conflict is reported once every argument has been read.
fn parseArgs(args: []const [*:0]const u8, usage: *Usage) error{ Help, Usage }!Options {
    var mode: ?Mode = null;
    var conflict: ?Usage = null;
    var uart: ?[:0]const u8 = null;
    var baud: ?u32 = null;
    var interval_s: ?f64 = null;
    var next: usize = 1;
    while (next < args.len) {
        const arg = std.mem.span(args[next]);
        next += 1;
        if (std.mem.eql(u8, arg, "-h") or std.mem.eql(u8, arg, "--help")) return error.Help;
        if (std.mem.eql(u8, arg, "--")) {
            if (next < args.len) return refuse(usage, .{ .unexpected_argument = std.mem.span(args[next]) });
            break;
        }
        if (!std.mem.startsWith(u8, arg, "--")) return refuse(usage, .{ .unexpected_argument = arg });
        const equals = std.mem.findScalar(u8, arg, '=');
        const name = arg[0 .. equals orelse arg.len];
        const attached: ?[:0]const u8 = if (equals) |at| arg[at + 1 ..] else null;

        if (std.mem.eql(u8, name, "--check")) {
            if (attached) |value| return refuse(usage, .{ .unexpected_value = .{ .flag = check_arg, .value = value } });
            if (mode) |seen| switch (seen) {
                .check => return refuse(usage, .{ .repeated = check_arg }),
                .net_json => conflict = conflict orelse .{ .conflict = .{ .first = net_json_arg, .second = check_arg } },
            } else mode = .check;
        } else if (std.mem.eql(u8, name, "--net-json")) {
            const value = try optionValue(args, &next, attached, net_json_arg, usage);
            if (mode) |seen| switch (seen) {
                .net_json => return refuse(usage, .{ .repeated = net_json_arg }),
                .check => conflict = conflict orelse .{ .conflict = .{ .first = check_arg, .second = net_json_arg } },
            } else mode = .{ .net_json = value };
        } else if (std.mem.eql(u8, name, "--uart")) {
            const value = try optionValue(args, &next, attached, uart_arg, usage);
            if (uart != null) return refuse(usage, .{ .repeated = uart_arg });
            uart = value;
        } else if (std.mem.eql(u8, name, "--baud")) {
            const value = try optionValue(args, &next, attached, baud_arg, usage);
            if (baud != null) return refuse(usage, .{ .repeated = baud_arg });
            baud = parseBaud(value) catch |reason|
                return refuse(usage, .{ .invalid_value = .{ .option = baud_arg, .value = value, .reason = reason } });
        } else if (std.mem.eql(u8, name, "--interval")) {
            const value = try optionValue(args, &next, attached, interval_arg, usage);
            if (interval_s != null) return refuse(usage, .{ .repeated = interval_arg });
            interval_s = parseInterval(value) catch |reason|
                return refuse(usage, .{ .invalid_value = .{ .option = interval_arg, .value = value, .reason = reason } });
        } else {
            return refuse(usage, .{ .unexpected_argument = name });
        }
    }
    if (conflict) |why| return refuse(usage, why);
    return .{
        .mode = mode orelse return refuse(usage, .missing_mode),
        .uart = uart orelse default_uart,
        .baud = baud orelse default_baud,
        .interval_s = interval_s orelse default_interval_s,
    };
}

/// `--name=value`, or the next argument unless it looks like a flag. `-` alone is a value.
fn optionValue(
    args: []const [*:0]const u8,
    next: *usize,
    attached: ?[:0]const u8,
    option: []const u8,
    usage: *Usage,
) error{Usage}![:0]const u8 {
    if (attached) |value| return value;
    if (next.* == args.len) return refuse(usage, .{ .missing_value = option });
    const value = std.mem.span(args[next.*]);
    if (std.mem.startsWith(u8, value, "--")) return refuse(usage, .{ .missing_value = option });
    if (value.len > 1 and value[0] == '-') return refuse(usage, .{ .unexpected_argument = value });
    next.* += 1;
    return value;
}

fn refuse(usage: *Usage, why: Usage) error{Usage} {
    usage.* = why;
    return error.Usage;
}

/// Read as an `i64`, then checked against the `u32` range, so `-1` and `99999999999999999999`
/// are refused for different reasons.
fn parseBaud(text: []const u8) ValueError!u32 {
    if (text.len == 0) return error.EmptyInteger;
    const wide = parseDecimal(i64, text) catch |err| return switch (err) {
        error.Overflow => if (text[0] == '-') error.TooSmall else error.TooLarge,
        error.InvalidCharacter => error.InvalidDigit,
    };
    return std.math.cast(u32, wide) orelse error.OutOfRange;
}

/// Takes `inf` and `nan`, but neither `_` nor hex, which `std.fmt.parseFloat` alone would accept.
fn parseInterval(text: []const u8) ValueError!f64 {
    if (text.len == 0) return error.EmptyFloat;
    if (std.mem.findAny(u8, text, "_xX") != null) return error.InvalidFloat;
    return std.fmt.parseFloat(f64, text) catch error.InvalidFloat;
}

/// An optional sign, then ASCII digits, and no `-` for an unsigned type.
/// `std.fmt.parseInt` alone would also take `_`.
fn parseDecimal(comptime T: type, text: []const u8) std.fmt.ParseIntError!T {
    if (std.mem.findScalar(u8, text, '_') != null) return error.InvalidCharacter;
    if (@typeInfo(T).int.signedness == .unsigned and std.mem.startsWith(u8, text, "-")) return error.InvalidCharacter;
    return std.fmt.parseInt(T, text, 10);
}

fn printUsage(usage: Usage) void {
    switch (usage) {
        .unexpected_argument => |arg| print("error: unexpected argument '{s}' found\n\nUsage: trck [OPTIONS]\n", .{arg}),
        .unexpected_value => |got| print(
            "error: unexpected value '{s}' for '{s}' found; no more were expected\n\nUsage: trck {s}\n",
            .{ got.value, got.flag, got.flag },
        ),
        .missing_value => |option| print("error: a value is required for '{s}' but none was supplied\n", .{option}),
        .invalid_value => |got| {
            print("error: invalid value '{s}' for '{s}': ", .{ got.value, got.option });
            switch (got.reason) {
                error.EmptyInteger => print("cannot parse integer from empty string\n", .{}),
                error.InvalidDigit => print("invalid digit found in string\n", .{}),
                error.TooLarge => print("number too large to fit in target type\n", .{}),
                error.TooSmall => print("number too small to fit in target type\n", .{}),
                error.OutOfRange => print("{s} is not in 0..=4294967295\n", .{got.value}),
                error.EmptyFloat => print("cannot parse float from empty string\n", .{}),
                error.InvalidFloat => print("invalid float literal\n", .{}),
            }
        },
        .repeated => |option| print("error: the argument '{s}' cannot be used multiple times\n\nUsage: trck [OPTIONS]\n", .{option}),
        .conflict => |pair| print(
            "error: the argument '{s}' cannot be used with '{s}'\n\nUsage: trck {s}\n",
            .{ pair.first, pair.second, pair.first },
        ),
        .missing_mode => print(
            "error: the following required arguments were not provided:\n  " ++ net_json_arg ++
                "\n\nUsage: trck " ++ net_json_arg ++ "\n",
            .{},
        ),
    }
    print("\nFor more information, try '--help'.\n", .{});
}

/// The `--check` pause, at least 0.2 s. `@max` turns a NaN into 0.2 s. More seconds than a
/// `u64` holds falls back to 1 s; fewer, but more nanoseconds than a `u64` holds, is clamped.
fn checkIntervalNs(interval_s: f64) u64 {
    const seconds = @max(interval_s, min_interval_s);
    if (seconds >= 0x1p64) return ns_per_s;
    const nanoseconds = seconds * ns_per_s;
    if (nanoseconds >= 0x1p64) return std.math.maxInt(u64);
    return @intFromFloat(nanoseconds);
}

/// What a failed call reports.
const Failure = union(enum) {
    os: posix.E,
    message: []const u8,

    pub fn format(failure: Failure, w: *std.Io.Writer) std.Io.Writer.Error!void {
        switch (failure) {
            .os => |errno| try w.print("{s} (os error {d})", .{ errnoText(errno), @backingInt(errno) }),
            .message => |text| try w.writeAll(text),
        }
    }
};

extern "c" fn strerror(errnum: c_int) [*:0]const u8;

fn errnoText(errno: posix.E) []const u8 {
    return std.mem.span(strerror(@backingInt(errno)));
}

/// The errno of a libc call that returned `rc`, or null if it succeeded.
fn failed(rc: c_int) ?posix.E {
    return if (rc < 0) posix.errno(rc) else null;
}

const Sink = union(enum) {
    uart: posix.fd_t,
    stdout,
};

/// Frames waiting for the UART. Only the poll loop touches it.
const Writer = struct {
    sink: Sink,
    queue: [queue_cap]frame.Frame = undefined,
    queued: usize = 0,
    /// A frame the UART took part of. `sent` bytes of it are already in the kernel.
    current: ?frame.Frame = null,
    sent: usize = 0,

    fn hasPending(writer: *const Writer) bool {
        return writer.current != null or writer.queued > 0;
    }

    fn uartFd(writer: *const Writer) ?posix.fd_t {
        return switch (writer.sink) {
            .uart => |fd| fd,
            .stdout => null,
        };
    }

    /// A full queue drops its oldest ordinary frame. With none, a new ordinary frame is dropped,
    /// and a new emergency drops the oldest emergency. Byte 27 is the emergency; 0 is ordinary.
    fn enqueue(writer: *Writer, raw: frame.Frame) void {
        if (writer.queued == queue_cap) {
            if (writer.oldestOrdinary()) |index| {
                writer.drop(index);
            } else if (raw[27] == 0) {
                logDrop(raw);
                return;
            } else {
                writer.drop(0);
            }
        }
        writer.queue[writer.queued] = raw;
        writer.queued += 1;
    }

    fn oldestOrdinary(writer: *const Writer) ?usize {
        for (writer.queue[0..writer.queued], 0..) |queued, index| {
            if (queued[27] == 0) return index;
        }
        return null;
    }

    fn drop(writer: *Writer, index: usize) void {
        logDrop(writer.queue[index]);
        writer.remove(index);
    }

    fn remove(writer: *Writer, index: usize) void {
        std.mem.copyForwards(frame.Frame, writer.queue[index .. writer.queued - 1], writer.queue[index + 1 .. writer.queued]);
        writer.queued -= 1;
    }

    /// Write until the UART would block or nothing is left. A short write stays in `current`.
    fn writeSome(writer: *Writer) error{ Stopped, UartFailed }!void {
        while (true) {
            if (stopping()) return error.Stopped;
            if (writer.current == null) {
                if (writer.queued == 0) return;
                writer.current = writer.queue[0];
                writer.sent = 0;
                writer.remove(0);
            }
            const rest = writer.current.?[writer.sent..];
            const fd = writer.uartFd() orelse posix.STDOUT_FILENO;
            const rc = c.write(fd, rest.ptr, rest.len);
            if (rc < 0) switch (posix.errno(rc)) {
                .AGAIN => return,
                .INTR => continue,
                else => |errno| return uartFailed(.{ .os = errno }),
            };
            if (rc == 0) return uartFailed(.{ .message = "uart write wrote nothing" });
            writer.sent += @intCast(rc);
            if (writer.sent == frame.len) writer.current = null;
        }
    }
};

fn uartFailed(failure: Failure) error{UartFailed} {
    print("uart write failed: {f}\n", .{failure});
    return error.UartFailed;
}

fn logDrop(raw: frame.Frame) void {
    print("dropped frame icao={x:0>6} emergency={d}\n", .{ std.mem.readInt(u24, raw[8..11], .little), raw[27] });
}

/// Bytes from the socket that are not a whole line yet.
const Lines = struct {
    /// Room for one `json.max_line` line and its newline.
    buf: [json.max_line + 1]u8 = undefined,
    len: usize = 0,
    /// A line outgrew `buf`. Its bytes up to the next newline are dropped.
    discarding: bool = false,
};

const Pull = union(enum) {
    bytes,
    eof,
    failed: Failure,
};

/// Read what the kernel has. The socket is non-blocking, so this returns on EAGAIN.
fn pull(lines: *Lines, sock: posix.fd_t, writer: *Writer) Pull {
    while (true) {
        const room = lines.buf[lines.len..];
        const rc = c.read(sock, room.ptr, room.len);
        if (rc == 0) return .eof;
        if (rc < 0) switch (posix.errno(rc)) {
            .AGAIN => return .bytes,
            .INTR => continue,
            else => |errno| return .{ .failed = .{ .os = errno } },
        };
        lines.len += @intCast(rc);
        enqueueReady(lines, writer);
    }
}

/// Queue a frame for each whole line, then keep the partial line. `lines.buf` always has room after.
fn enqueueReady(lines: *Lines, writer: *Writer) void {
    var start: usize = 0;
    while (std.mem.findScalarPos(u8, lines.buf[0..lines.len], start, '\n')) |end| {
        const line = std.mem.cutSuffix(u8, lines.buf[start..end], "\r") orelse lines.buf[start..end];
        start = end + 1;
        if (lines.discarding) {
            lines.discarding = false;
            continue;
        }
        const fix = json.parseLine(line) orelse continue;
        writer.enqueue(frame.pack(fix));
        print("fix t={d} icao={x:0>6}\n", .{ fix.unix_s, fix.icao });
    }
    std.mem.copyForwards(u8, lines.buf[0 .. lines.len - start], lines.buf[start..lines.len]);
    lines.len -= start;
    if (lines.len == lines.buf.len) {
        lines.len = 0;
        lines.discarding = true;
    }
}

const End = enum { stopped, disconnected, uart_failed };

/// One connection. The socket is read whenever it has bytes. The UART is written when it can take more.
fn pump(sock: posix.fd_t, writer: *Writer, text: []const u8) End {
    if (enableKeepalive(sock)) |failure| {
        print("net-json {s} keepalive: {f}\n", .{ text, failure });
        return .disconnected;
    }
    var lines: Lines = .{};
    while (true) {
        if (stopping()) return .stopped;
        var fds: [2]posix.pollfd = .{ .{ .fd = sock, .events = posix.POLL.IN, .revents = 0 }, undefined };
        var nfds: usize = 1;
        if (writer.hasPending()) {
            if (writer.uartFd()) |fd| {
                fds[1] = .{ .fd = fd, .events = posix.POLL.OUT, .revents = 0 };
                nfds = 2;
            }
        }
        if (pollWait(fds[0..nfds], poll_slice_ns)) |failure| {
            print("net-json {s}: {f}\n", .{ text, failure });
            return endSocket(writer);
        }
        if (stopping()) return .stopped;
        const socket_events = fds[0].revents;
        var fresh = false;
        if (socket_events & (posix.POLL.IN | posix.POLL.HUP | posix.POLL.ERR) != 0) {
            switch (pull(&lines, sock, writer)) {
                .eof => {
                    print("net-json {s} disconnected\n", .{text});
                    return endSocket(writer);
                },
                .bytes => {
                    fresh = true;
                    if (socket_events & (posix.POLL.HUP | posix.POLL.ERR) != 0) {
                        print("net-json {s} disconnected\n", .{text});
                        return endSocket(writer);
                    }
                },
                .failed => |failure| {
                    print("net-json {s}: {f}\n", .{ text, failure });
                    return endSocket(writer);
                },
            }
        }
        const uart_ready = nfds == 2 and fds[1].revents & (posix.POLL.OUT | posix.POLL.ERR | posix.POLL.HUP) != 0;
        if (writer.hasPending() and (fresh or uart_ready or writer.uartFd() == null)) {
            writer.writeSome() catch |err| return switch (err) {
                error.Stopped => .stopped,
                error.UartFailed => .uart_failed,
            };
        }
    }
}

fn endSocket(writer: *Writer) End {
    writer.writeSome() catch |err| return switch (err) {
        error.Stopped => .stopped,
        error.UartFailed => .uart_failed,
    };
    return .disconnected;
}

/// Retries an interrupted poll with the same timeout.
fn pollWait(fds: []posix.pollfd, timeout_ns: u64) ?Failure {
    const timeout_ms = std.math.cast(c_int, timeout_ns / ns_per_ms) orelse std.math.maxInt(c_int);
    while (true) {
        const rc = c.poll(fds.ptr, @intCast(fds.len), timeout_ms);
        if (rc >= 0) return null;
        switch (posix.errno(rc)) {
            .INTR => continue,
            else => |errno| return .{ .os = errno },
        }
    }
}

fn runPort(addresses: *const posix.addrinfo, text: []const u8, writer: *Writer) u8 {
    var delay_ns: u64 = retry_min_ns;
    while (!stopping()) {
        if (dialFirst(addresses, text)) |sock| {
            print("net-json {s} connected\n", .{text});
            const end = pump(sock, writer, text);
            _ = c.close(sock);
            switch (end) {
                .stopped => break,
                .uart_failed => return 1,
                .disconnected => delay_ns = retry_min_ns,
            }
        }
        if (stopping()) break;
        sleepUntilStop(delay_ns);
        delay_ns = @min(delay_ns * 2, retry_max_ns);
    }
    print("stopped\n", .{});
    return 0;
}

fn runCheck(interval_ns: u64, writer: *Writer) u8 {
    while (!stopping()) {
        for (frame.check_positions) |fix| {
            print("fix t={d} icao={x:0>6}\n", .{ fix.unix_s, fix.icao });
            writer.enqueue(frame.pack(fix));
        }
        const deadline = nowNs() +| interval_ns;
        while (!stopping()) {
            writer.writeSome() catch |err| switch (err) {
                error.Stopped => break,
                error.UartFailed => return 1,
            };
            const now = nowNs();
            if (now >= deadline or !writer.hasPending()) {
                sleepUntilStop(deadline -| now);
                break;
            }
            const fd = writer.uartFd() orelse break;
            var fds: [1]posix.pollfd = .{.{ .fd = fd, .events = posix.POLL.OUT, .revents = 0 }};
            if (pollWait(&fds, @min(deadline - now, poll_slice_ns))) |failure| {
                print("uart poll: {f}\n", .{failure});
                return 1;
            }
        }
    }
    print("stopped\n", .{});
    return 0;
}

fn nowNs() u64 {
    var now: posix.timespec = undefined;
    _ = c.clock_gettime(.MONOTONIC, &now);
    return @as(u64, @intCast(now.sec)) * ns_per_s + @as(u64, @intCast(now.nsec));
}

fn sleepNs(ns: u64) void {
    var left: posix.timespec = .{ .sec = @intCast(ns / ns_per_s), .nsec = @intCast(ns % ns_per_s) };
    var rest: posix.timespec = undefined;
    while (failed(c.nanosleep(&left, &rest)) == .INTR) left = rest;
}

fn sleepUntilStop(ns: u64) void {
    var left = ns;
    while (left > 0 and !stopping()) {
        const slice = @min(left, sleep_step_ns);
        sleepNs(slice);
        left -= slice;
    }
}

/// Split at the last `:`, resolve the host, and put the port on every result. `[v6]` keeps its brackets out of `getaddrinfo`. The list lives as long as the process.
fn resolve(text: []const u8) ?*posix.addrinfo {
    var host, const port_text = std.mem.cutScalarLast(u8, text, ':') orelse return null;
    const port = parseDecimal(u16, port_text) catch return null;
    var hints = std.mem.zeroes(posix.addrinfo);
    hints.socktype = posix.SOCK.STREAM;
    if (std.mem.cutPrefix(u8, host, "[")) |opened| {
        if (std.mem.cutSuffix(u8, opened, "]")) |literal| {
            if (std.mem.findScalar(u8, literal, ':') != null) {
                host = literal;
                hints.flags.NUMERICHOST = true;
            }
        }
    }
    var name: [256]u8 = undefined;
    if (host.len >= name.len) return null;
    @memcpy(name[0..host.len], host);
    name[host.len] = 0;
    var list: ?*posix.addrinfo = null;
    if (@backingInt(c.getaddrinfo(name[0..host.len :0], null, &hints, &list)) != 0) return null;
    var it = list;
    while (it) |address| : (it = address.next) {
        const raw = address.addr orelse continue;
        switch (address.family) {
            posix.AF.INET => @as(*posix.sockaddr.in, @ptrCast(@alignCast(raw))).port = std.mem.nativeToBig(u16, port),
            posix.AF.INET6 => @as(*posix.sockaddr.in6, @ptrCast(@alignCast(raw))).port = std.mem.nativeToBig(u16, port),
            else => {},
        }
    }
    return list;
}

/// Try each resolved address. A refused `::1` must not hide a later `127.0.0.1`.
fn dialFirst(addresses: *const posix.addrinfo, text: []const u8) ?posix.fd_t {
    var it: ?*const posix.addrinfo = addresses;
    while (it) |address| : (it = address.next) {
        switch (dial(address, connect_timeout_ns)) {
            .connected => |sock| return sock,
            .stopped => return null,
            .failed => |failure| print("net-json {s}: {f}\n", .{ text, failure }),
        }
    }
    return null;
}

const Dial = union(enum) {
    connected: posix.fd_t,
    stopped,
    failed: Failure,
};

/// A non-blocking connect, polled in slices so a stop is noticed during the wait.
fn dial(address: *const posix.addrinfo, timeout_ns: u64) Dial {
    if (stopping()) return .stopped;
    const sock = c.socket(@intCast(address.family), posix.SOCK.STREAM | posix.SOCK.CLOEXEC | posix.SOCK.NONBLOCK, 0);
    if (failed(sock)) |errno| return .{ .failed = .{ .os = errno } };
    const result = connect(sock, address, timeout_ns);
    if (result != .connected) _ = c.close(sock);
    return result;
}

fn connect(sock: posix.fd_t, address: *const posix.addrinfo, timeout_ns: u64) Dial {
    switch (posix.errno(c.connect(sock, address.addr.?, address.addrlen))) {
        .SUCCESS => return .{ .connected = sock },
        .INPROGRESS, .AGAIN, .INTR => {},
        else => |errno| return .{ .failed = .{ .os = errno } },
    }
    const deadline = nowNs() +| timeout_ns;
    while (true) {
        if (stopping()) return .stopped;
        const now = nowNs();
        if (now >= deadline) return .{ .failed = .{ .message = "connection timed out" } };
        var fds: [1]posix.pollfd = .{.{ .fd = sock, .events = posix.POLL.OUT, .revents = 0 }};
        if (pollWait(&fds, @min(deadline - now, connect_slice_ns))) |failure| return .{ .failed = failure };
        if (fds[0].revents & (posix.POLL.OUT | posix.POLL.ERR | posix.POLL.HUP) == 0) continue;
        var so_error: c_int = 0;
        var len: posix.socklen_t = @sizeOf(c_int);
        if (failed(c.getsockopt(sock, posix.SOL.SOCKET, posix.SO.ERROR, &so_error, &len))) |errno| {
            return .{ .failed = .{ .os = errno } };
        }
        if (so_error != 0) return .{ .failed = .{ .os = @fromBackingInt(@intCast(so_error)) } };
        return .{ .connected = sock };
    }
}

/// These probes turn a peer that dies without a FIN into a read error, so the caller reconnects.
fn enableKeepalive(sock: posix.fd_t) ?Failure {
    const options = [_]struct { i32, u32, c_int }{
        .{ posix.SOL.SOCKET, posix.SO.KEEPALIVE, 1 },
        .{ posix.IPPROTO.TCP, posix.TCP.KEEPIDLE, keepalive_idle_s },
        .{ posix.IPPROTO.TCP, posix.TCP.KEEPINTVL, keepalive_interval_s },
        .{ posix.IPPROTO.TCP, posix.TCP.KEEPCNT, keepalive_probes },
    };
    for (options) |option| {
        const level, const name, const value = option;
        if (failed(c.setsockopt(sock, level, name, &value, @sizeOf(c_int)))) |errno| return .{ .os = errno };
    }
    return null;
}

/// `struct termios2` from asm-generic/termbits.h. The std type keeps the speeds in an
/// exhaustive enum, which cannot hold a `BOTHER` rate.
const Termios2 = extern struct {
    iflag: u32,
    oflag: u32,
    cflag: u32,
    lflag: u32,
    line: u8,
    cc: [19]u8,
    ispeed: u32,
    ospeed: u32,
};

comptime {
    std.debug.assert(@sizeOf(Termios2) == @sizeOf(linux.termios2));
}

/// asm-generic/termbits.h, which arm and x86_64 share.
const tio = struct {
    const IGNBRK: u32 = 0x1;
    const BRKINT: u32 = 0x2;
    const IGNPAR: u32 = 0x4;
    const PARMRK: u32 = 0x8;
    const INPCK: u32 = 0x10;
    const ISTRIP: u32 = 0x20;
    const INLCR: u32 = 0x40;
    const IGNCR: u32 = 0x80;
    const ICRNL: u32 = 0x100;
    const IXON: u32 = 0x400;
    const IXOFF: u32 = 0x1000;
    const OPOST: u32 = 0x1;
    const CBAUD: u32 = 0x100F;
    const CSIZE: u32 = 0x30;
    const CS8: u32 = 0x30;
    const CSTOPB: u32 = 0x40;
    const CREAD: u32 = 0x80;
    const PARENB: u32 = 0x100;
    const PARODD: u32 = 0x200;
    const CLOCAL: u32 = 0x800;
    const BOTHER: u32 = 0x1000;
    const CIBAUD: u32 = 0x100F0000;
    const CRTSCTS: u32 = 0x80000000;
    const ISIG: u32 = 0x1;
    const ICANON: u32 = 0x2;
    const ECHO: u32 = 0x8;
    const ECHONL: u32 = 0x40;
    const IEXTEN: u32 = 0x8000;
    const VTIME = 5;
    const VMIN = 6;
    const TCIOFLUSH: c_int = 2;
};

fn openUart(path: [:0]const u8, baud: u32) ?posix.fd_t {
    const fd = c.open(path, .{ .ACCMODE = .RDWR, .NOCTTY = true, .NONBLOCK = true, .CLOEXEC = true });
    if (failed(fd)) |errno| {
        print("cannot open {s}: {s}\n", .{ path, errnoText(errno) });
        return null;
    }
    if (configureUart(fd, baud)) |problem| {
        print("cannot open {s}: {s}\n", .{ path, problem });
        _ = c.close(fd);
        return null;
    }
    return fd;
}

/// Exclusive, raw and checked, then 8N1 with no flow control and the baud through `BOTHER`,
/// then both buffers flushed. The fd stays non-blocking.
fn configureUart(fd: posix.fd_t, baud: u32) ?[]const u8 {
    if (failed(c.ioctl(fd, linux.T.IOCEXCL))) |errno| return errnoText(errno);
    if (failed(c.flock(fd, posix.LOCK.EX | posix.LOCK.NB))) |errno| {
        return if (errno == .AGAIN) "Unable to acquire exclusive lock on serial port" else errnoText(errno);
    }
    var settings: Termios2 = undefined;
    if (failed(c.ioctl(fd, @bitCast(linux.T.CGETS2), &settings))) |errno| return errnoText(errno);
    settings.cflag |= tio.CREAD | tio.CLOCAL;
    settings.iflag &= ~(tio.IGNBRK | tio.BRKINT | tio.PARMRK | tio.ISTRIP | tio.INLCR | tio.IGNCR | tio.ICRNL | tio.IXON);
    settings.oflag &= ~tio.OPOST;
    settings.lflag &= ~(tio.ECHO | tio.ECHONL | tio.ICANON | tio.ISIG | tio.IEXTEN);
    settings.cflag = settings.cflag & ~(tio.CSIZE | tio.PARENB) | tio.CS8;
    settings.cc[tio.VMIN] = 1;
    settings.cc[tio.VTIME] = 0;
    if (failed(c.ioctl(fd, @bitCast(linux.T.CSETS2), &settings))) |errno| return errnoText(errno);

    var applied: Termios2 = undefined;
    if (failed(c.ioctl(fd, @bitCast(linux.T.CGETS2), &applied))) |errno| return errnoText(errno);
    if (applied.iflag != settings.iflag or applied.oflag != settings.oflag or
        applied.cflag != settings.cflag or applied.lflag != settings.lflag)
    {
        return "Settings did not apply correctly";
    }

    applied.iflag = applied.iflag & ~(tio.INPCK | tio.IXON | tio.IXOFF) | tio.IGNPAR;
    applied.cflag &= ~(tio.PARENB | tio.PARODD | tio.CRTSCTS | tio.CSIZE | tio.CSTOPB | tio.CBAUD | tio.CIBAUD);
    applied.cflag |= tio.CS8 | tio.BOTHER;
    applied.ispeed = baud;
    applied.ospeed = baud;
    if (failed(c.ioctl(fd, @bitCast(linux.T.CSETS2), &applied))) |errno| return errnoText(errno);
    if (failed(c.ioctl(fd, linux.T.CFLSH, tio.TCIOFLUSH))) |errno| return errnoText(errno);
    return null;
}

fn onSigint(_: posix.SIG) callconv(.c) void {
    stop.store(true, .monotonic);
}

/// A closed reader turns into EPIPE on write, not a dead process.
fn ignoreSigpipe() void {
    const ignore: posix.Sigaction = .{ .handler = .{ .handler = posix.SIG.IGN }, .mask = posix.sigemptyset(), .flags = 0 };
    posix.sigaction(.PIPE, &ignore, null);
}

fn catchSigint() void {
    const action: posix.Sigaction = .{ .handler = .{ .handler = onSigint }, .mask = posix.sigemptyset(), .flags = posix.SA.RESTART };
    posix.sigaction(.INT, &action, null);
}

fn writeAll(fd: posix.fd_t, bytes: []const u8) void {
    var rest = bytes;
    while (rest.len > 0) {
        const rc = c.write(fd, rest.ptr, rest.len);
        if (rc < 0 and posix.errno(rc) == .INTR) continue;
        if (rc <= 0) return;
        rest = rest[@intCast(rc)..];
    }
}

const testing = std.testing;

test {
    _ = frame;
    _ = json;
}

fn parseTest(args: []const [*:0]const u8, usage: *Usage) error{ Help, Usage }!Options {
    usage.* = .missing_mode;
    return parseArgs(args, usage);
}

test "--check and --net-json together are refused in either order" {
    var usage: Usage = undefined;
    try testing.expectError(error.Usage, parseTest(&.{ "trck", "--check", "--net-json", "localhost:30154" }, &usage));
    try testing.expectEqualStrings(check_arg, usage.conflict.first);
    try testing.expectEqualStrings(net_json_arg, usage.conflict.second);
    try testing.expectError(error.Usage, parseTest(&.{ "trck", "--net-json=localhost:30154", "--check" }, &usage));
    try testing.expectEqualStrings(net_json_arg, usage.conflict.first);
    try testing.expectError(error.Usage, parseTest(&.{ "trck", "--check", "--net-json", "a:1", "--bogus" }, &usage));
    try testing.expectEqualStrings("--bogus", usage.unexpected_argument);
    try testing.expectError(error.Help, parseTest(&.{ "trck", "--check", "--net-json", "a:1", "-h" }, &usage));
}

test "options" {
    var usage: Usage = undefined;
    const check = try parseTest(&.{ "trck", "--check" }, &usage);
    try testing.expectEqual(Mode.check, check.mode);
    try testing.expectEqualStrings("/dev/serial0", check.uart);
    try testing.expectEqual(115_200, check.baud);
    try testing.expectEqual(1.0, check.interval_s);

    const port = try parseTest(&.{ "trck", "--net-json", "localhost:30154", "--uart", "-", "--baud=9600", "--interval", "0.5" }, &usage);
    try testing.expectEqualStrings("localhost:30154", port.mode.net_json);
    try testing.expectEqualStrings("-", port.uart);
    try testing.expectEqual(9600, port.baud);
    try testing.expectEqual(0.5, port.interval_s);

    try testing.expectEqual(5, (try parseTest(&.{ "trck", "--check", "--baud", "+5" }, &usage)).baud);
    try testing.expectEqual(-5.0, (try parseTest(&.{ "trck", "--check", "--interval=-5" }, &usage)).interval_s);
    try testing.expect(std.math.isNan((try parseTest(&.{ "trck", "--check", "--interval", "nan" }, &usage)).interval_s));
}

test "bad command lines" {
    var usage: Usage = undefined;
    try testing.expectError(error.Usage, parseTest(&.{"trck"}, &usage));
    try testing.expect(usage == .missing_mode);
    try testing.expectError(error.Usage, parseTest(&.{ "trck", "--uart", "/dev/ttyS0" }, &usage));
    try testing.expect(usage == .missing_mode);
    try testing.expectError(error.Usage, parseTest(&.{ "trck", "--check", "--check" }, &usage));
    try testing.expectEqualStrings(check_arg, usage.repeated);
    try testing.expectError(error.Usage, parseTest(&.{ "trck", "--check", "--baud", "1", "--baud", "2" }, &usage));
    try testing.expectEqualStrings(baud_arg, usage.repeated);
    try testing.expectError(error.Usage, parseTest(&.{ "trck", "--check=1" }, &usage));
    try testing.expectEqualStrings("1", usage.unexpected_value.value);
    try testing.expectError(error.Usage, parseTest(&.{ "trck", "--net-json", "--check" }, &usage));
    try testing.expectEqualStrings(net_json_arg, usage.missing_value);
    try testing.expectError(error.Usage, parseTest(&.{ "trck", "--check", "--uart" }, &usage));
    try testing.expectEqualStrings(uart_arg, usage.missing_value);
    try testing.expectError(error.Usage, parseTest(&.{ "trck", "--check", "--interval", "-5" }, &usage));
    try testing.expectEqualStrings("-5", usage.unexpected_argument);
    try testing.expectError(error.Usage, parseTest(&.{ "trck", "--check", "extra" }, &usage));
    try testing.expectEqualStrings("extra", usage.unexpected_argument);
    try testing.expectError(error.Usage, parseTest(&.{ "trck", "--check", "--", "--uart" }, &usage));
    try testing.expectEqualStrings("--uart", usage.unexpected_argument);

    const values = [_]struct { []const u8, ValueError }{
        .{ "--baud=", error.EmptyInteger },
        .{ "--baud=-1", error.OutOfRange },
        .{ "--baud=4294967296", error.OutOfRange },
        .{ "--baud=99999999999999999999", error.TooLarge },
        .{ "--baud=1_0", error.InvalidDigit },
        .{ "--baud=0x10", error.InvalidDigit },
        .{ "--baud= 5", error.InvalidDigit },
        .{ "--interval=", error.EmptyFloat },
        .{ "--interval=abc", error.InvalidFloat },
        .{ "--interval=1_0", error.InvalidFloat },
        .{ "--interval=0x1p4", error.InvalidFloat },
        .{ "--interval= 1", error.InvalidFloat },
    };
    for (values) |case| {
        const arg, const reason = case;
        var buf: [64]u8 = undefined;
        const z = try std.fmt.bufPrintSentinel(&buf, "{s}", .{arg}, 0);
        try testing.expectError(error.Usage, parseTest(&.{ "trck", "--check", z }, &usage));
        try testing.expectEqual(reason, usage.invalid_value.reason);
    }
}

test "the --check pause" {
    try testing.expectEqual(1 * ns_per_s, checkIntervalNs(1));
    try testing.expectEqual(200 * ns_per_ms, checkIntervalNs(0));
    try testing.expectEqual(200 * ns_per_ms, checkIntervalNs(-5));
    try testing.expectEqual(200 * ns_per_ms, checkIntervalNs(std.math.nan(f64)));
    try testing.expectEqual(1 * ns_per_s, checkIntervalNs(std.math.inf(f64)));
    try testing.expectEqual(1 * ns_per_s, checkIntervalNs(1e300));
    try testing.expectEqual(std.math.maxInt(u64), checkIntervalNs(1e12));
}

fn frameFor(icao: u24, emergency: bool) !frame.Frame {
    var buf: [128]u8 = undefined;
    const line = try std.fmt.bufPrint(&buf, "{{\"now\":1700000000,\"hex\":\"{x:0>6}\",\"lat\":1,\"lon\":2{s}}}", .{
        icao,
        if (emergency) ",\"squawk\":\"7700\"" else "",
    });
    const fix = json.parseLine(line).?;
    try testing.expectEqual(emergency, fix.emergency != .none);
    return frame.pack(fix);
}

fn frameIcao(raw: frame.Frame) u24 {
    return std.mem.readInt(u24, raw[8..11], .little);
}

test "a 7700 survives a queue full of ordinary frames" {
    var writer: Writer = .{ .sink = .stdout };
    for (0..queue_cap) |icao| writer.enqueue(try frameFor(@intCast(icao), false));
    writer.enqueue(try frameFor(0x7700, true));
    try testing.expectEqual(queue_cap, writer.queued);
    const kept = writer.queue[queue_cap - 1];
    try testing.expectEqual(@backingInt(frame.Emergency.general), kept[27]);
    try testing.expectEqual(0x7700, frameIcao(kept));
    try testing.expectEqual(1_700_000_000, std.mem.readInt(u32, kept[4..8], .little));
    try testing.expectEqual(1, frameIcao(writer.queue[0]));

    writer.queued = 0;
    for (1..queue_cap + 1) |icao| writer.enqueue(try frameFor(@intCast(icao), true));
    writer.enqueue(try frameFor(0x100, false));
    try testing.expectEqual(queue_cap, writer.queued);
    for (writer.queue) |queued| {
        try testing.expect(queued[27] != 0);
        try testing.expect(frameIcao(queued) != 0x100);
    }

    writer.enqueue(try frameFor(0x200, true));
    try testing.expectEqual(2, frameIcao(writer.queue[0]));
    try testing.expectEqual(0x200, frameIcao(writer.queue[queue_cap - 1]));
}

fn socketPair() ![2]posix.fd_t {
    var fds: [2]posix.fd_t = undefined;
    if (c.socketpair(posix.AF.UNIX, posix.SOCK.STREAM | posix.SOCK.NONBLOCK | posix.SOCK.CLOEXEC, 0, &fds) < 0) {
        return error.SocketPair;
    }
    return fds;
}

test "lines across reads, CRLF, and a line longer than readsb writes" {
    const ends = try socketPair();
    defer _ = c.close(ends[0]);
    var writer: Writer = .{ .sink = .stdout };
    var lines: Lines = .{};
    const line = "{\"now\":1700000000,\"hex\":\"000001\",\"lat\":1,\"lon\":2}";

    writeAll(ends[1], line[0..20]);
    try testing.expectEqual(Pull.bytes, pull(&lines, ends[0], &writer));
    try testing.expectEqual(0, writer.queued);
    writeAll(ends[1], line[20..] ++ "\r\n" ++ "not json\n");
    try testing.expectEqual(Pull.bytes, pull(&lines, ends[0], &writer));
    try testing.expectEqual(1, writer.queued);
    try testing.expectEqual(0, lines.len);

    var longest: [json.max_line + 1]u8 = @splat(' ');
    @memcpy(longest[0 .. line.len - 1], line[0 .. line.len - 1]);
    longest[json.max_line - 1] = '}';
    longest[json.max_line] = '\n';
    writeAll(ends[1], &longest);
    try testing.expectEqual(Pull.bytes, pull(&lines, ends[0], &writer));
    try testing.expectEqual(2, writer.queued);

    var too_long: [json.max_line + 2]u8 = undefined;
    @memcpy(too_long[0 .. json.max_line - 1], longest[0 .. json.max_line - 1]);
    too_long[json.max_line - 1] = ' ';
    too_long[json.max_line] = '}';
    too_long[json.max_line + 1] = '\n';
    writeAll(ends[1], &too_long);
    writeAll(ends[1], line ++ "\n");
    _ = c.close(ends[1]);
    try testing.expectEqual(Pull.eof, pull(&lines, ends[0], &writer));
    try testing.expectEqual(3, writer.queued);
    try testing.expect(!lines.discarding);
}

const Pty = struct {
    master: posix.fd_t,
    path_buf: [32]u8,
    path_len: usize,

    fn open() !Pty {
        const master = c.open("/dev/ptmx", .{ .ACCMODE = .RDWR, .NOCTTY = true, .CLOEXEC = true });
        if (master < 0) return error.NoPty;
        errdefer _ = c.close(master);
        var unlock: c_int = 0;
        var number: c_uint = 0;
        if (c.ioctl(master, @bitCast(linux.T.IOCSPTLCK), &unlock) < 0) return error.NoPty;
        if (c.ioctl(master, @bitCast(linux.T.IOCGPTN), &number) < 0) return error.NoPty;
        var pty: Pty = .{ .master = master, .path_buf = undefined, .path_len = 0 };
        pty.path_len = (try std.fmt.bufPrintSentinel(&pty.path_buf, "/dev/pts/{d}", .{number}, 0)).len;
        return pty;
    }

    fn path(pty: *const Pty) [:0]const u8 {
        return pty.path_buf[0..pty.path_len :0];
    }

    fn uartWriter(pty: *const Pty) !Writer {
        return .{ .sink = .{ .uart = openUart(pty.path(), default_baud) orelse return error.OpenUart } };
    }

    fn close(pty: *const Pty) void {
        _ = c.close(pty.master);
    }
};

fn closeUart(writer: *const Writer) void {
    _ = c.close(writer.uartFd().?);
}

/// Fill the pty until a write would block, so the UART looks stalled.
fn fillUntilBlocked(fd: posix.fd_t) !void {
    const junk: [1024]u8 = @splat(0xA5);
    var wrote: usize = 0;
    while (true) {
        const rc = c.write(fd, &junk, junk.len);
        if (rc < 0) {
            try testing.expectEqual(posix.E.AGAIN, posix.errno(rc));
            break;
        }
        wrote += @intCast(rc);
        try testing.expect(wrote < 1024 * 1024);
    }
    try testing.expect(wrote > 0);
}

test "the pty master returns what was written" {
    const pty = try Pty.open();
    defer pty.close();
    var writer = try pty.uartWriter();
    defer closeUart(&writer);
    const raw = try frameFor(0x40621D, false);
    writer.enqueue(raw);
    try writer.writeSome();
    try testing.expect(!writer.hasPending());
    var fds: [1]posix.pollfd = .{.{ .fd = pty.master, .events = posix.POLL.IN, .revents = 0 }};
    try testing.expectEqual(null, pollWait(&fds, ns_per_s));
    var buf: [64]u8 = undefined;
    const n = c.read(pty.master, &buf, buf.len);
    try testing.expectEqualSlices(u8, &raw, buf[0..@intCast(n)]);
}

test "a busy UART cannot be opened twice" {
    const pty = try Pty.open();
    defer pty.close();
    var writer = try pty.uartWriter();
    defer closeUart(&writer);
    try testing.expectEqual(null, openUart(pty.path(), default_baud));
}

const Listener = struct {
    fd: posix.fd_t,
    v4: posix.sockaddr.in,
    v6: posix.sockaddr.in6,

    fn open(backlog: c_uint) !Listener {
        const fd = c.socket(posix.AF.INET, posix.SOCK.STREAM | posix.SOCK.CLOEXEC, 0);
        if (fd < 0) return error.Socket;
        errdefer _ = c.close(fd);
        var v4: posix.sockaddr.in = .{ .port = 0, .addr = std.mem.nativeToBig(u32, 0x7F00_0001) };
        var len: posix.socklen_t = @sizeOf(posix.sockaddr.in);
        if (c.bind(fd, @ptrCast(&v4), len) < 0) return error.Bind;
        if (c.listen(fd, backlog) < 0) return error.Listen;
        if (c.getsockname(fd, @ptrCast(&v4), &len) < 0) return error.SockName;
        var v6: posix.sockaddr.in6 = .{ .port = v4.port, .flowinfo = 0, .addr = @splat(0), .scope_id = 0 };
        v6.addr[15] = 1;
        return .{ .fd = fd, .v4 = v4, .v6 = v6 };
    }

    fn info(listener: *Listener, family: enum { v4, v6 }, next: ?*posix.addrinfo) posix.addrinfo {
        var address = std.mem.zeroes(posix.addrinfo);
        address.socktype = posix.SOCK.STREAM;
        address.next = next;
        switch (family) {
            .v4 => {
                address.family = posix.AF.INET;
                address.addr = @ptrCast(&listener.v4);
                address.addrlen = @sizeOf(posix.sockaddr.in);
            },
            .v6 => {
                address.family = posix.AF.INET6;
                address.addr = @ptrCast(&listener.v6);
                address.addrlen = @sizeOf(posix.sockaddr.in6);
            },
        }
        return address;
    }

    fn connect(listener: *Listener) !posix.fd_t {
        const address = listener.info(.v4, null);
        return switch (dial(&address, ns_per_s)) {
            .connected => |sock| sock,
            else => error.Dial,
        };
    }

    fn close(listener: *const Listener) void {
        _ = c.close(listener.fd);
    }
};

fn stopAfter(ns: u64) void {
    sleepNs(ns);
    stop.store(true, .monotonic);
}

fn socketInt(sock: posix.fd_t, level: i32, name: u32) !c_int {
    var value: c_int = -1;
    var len: posix.socklen_t = @sizeOf(c_int);
    if (c.getsockopt(sock, level, name, &value, &len) < 0) return error.GetSockOpt;
    return value;
}

test "the readsb socket probes a silent peer" {
    var listener = try Listener.open(1);
    defer listener.close();
    const sock = try listener.connect();
    defer _ = c.close(sock);
    try testing.expectEqual(null, enableKeepalive(sock));
    try testing.expectEqual(1, try socketInt(sock, posix.SOL.SOCKET, posix.SO.KEEPALIVE));
    try testing.expectEqual(keepalive_idle_s, try socketInt(sock, posix.IPPROTO.TCP, posix.TCP.KEEPIDLE));
    try testing.expectEqual(keepalive_interval_s, try socketInt(sock, posix.IPPROTO.TCP, posix.TCP.KEEPINTVL));
    try testing.expectEqual(keepalive_probes, try socketInt(sock, posix.IPPROTO.TCP, posix.TCP.KEEPCNT));
}

test "a silent UART does not hold up dozens of lines" {
    const pty = try Pty.open();
    defer pty.close();
    var writer = try pty.uartWriter();
    defer closeUart(&writer);
    try fillUntilBlocked(writer.uartFd().?);

    var listener = try Listener.open(1);
    defer listener.close();
    const sock = try listener.connect();
    defer _ = c.close(sock);
    const peer = c.accept(listener.fd, null, null);
    try testing.expect(peer >= 0);
    const count = 48;
    for (0..count) |icao| {
        var buf: [96]u8 = undefined;
        writeAll(peer, try std.fmt.bufPrint(&buf, "{{\"now\":1700000000,\"hex\":\"{x:0>6}\",\"lat\":1,\"lon\":2}}\n", .{icao}));
    }
    _ = c.close(peer);

    const started = nowNs();
    try testing.expectEqual(End.disconnected, pump(sock, &writer, "test"));
    try testing.expect(nowNs() - started < 50 * ns_per_ms);
    try testing.expect(writer.queued + @intFromBool(writer.current != null) > count / 2);
}

test "a stop ends the loop while the UART is not draining" {
    const pty = try Pty.open();
    defer pty.close();
    var writer = try pty.uartWriter();
    defer closeUart(&writer);
    try fillUntilBlocked(writer.uartFd().?);
    writer.enqueue(try frameFor(1, false));

    var listener = try Listener.open(1);
    defer listener.close();
    const sock = try listener.connect();
    defer _ = c.close(sock);
    const thread = try std.Thread.spawn(.{}, stopAfter, .{100 * ns_per_ms});
    defer stop.store(false, .monotonic);
    defer thread.join();

    const started = nowNs();
    try testing.expectEqual(End.stopped, pump(sock, &writer, "test"));
    try testing.expect(nowNs() - started < ns_per_s);
    try testing.expect(writer.hasPending());
}

test "localhost resolves every address" {
    const list = resolve("localhost:9") orelse return error.Unresolved;
    var v4 = false;
    var v6 = false;
    var it: ?*posix.addrinfo = list;
    while (it) |address| : (it = address.next) {
        v4 = v4 or address.family == posix.AF.INET;
        v6 = v6 or address.family == posix.AF.INET6;
    }
    try testing.expect(v4 and v6);
}

test "resolve" {
    const v4 = resolve("127.0.0.1:30154").?;
    try testing.expectEqual(posix.AF.INET, v4.family);
    try testing.expectEqual(null, v4.next);
    const port = @as(*posix.sockaddr.in, @ptrCast(@alignCast(v4.addr.?))).port;
    try testing.expectEqual(30154, std.mem.bigToNative(u16, port));
    try testing.expectEqual(posix.AF.INET6, resolve("[::1]:30154").?.family);
    try testing.expectEqual(posix.AF.INET6, resolve("::1:30154").?.family);
    try testing.expect(resolve("127.0.0.1:+80") != null);

    const bad = [_][]const u8{ "localhost", "localhost:", "localhost:65536", "localhost:-1", "localhost:1_0", "[::1]", "[127.0.0.1]:80" };
    for (bad) |text| {
        testing.expectEqual(null, resolve(text)) catch |err| {
            print("resolved: {s}\n", .{text});
            return err;
        };
    }
}

test "dials the later address when the first refuses" {
    var listener = try Listener.open(1);
    defer listener.close();
    var second = listener.info(.v4, null);
    const first = listener.info(.v6, &second);
    const sock = dialFirst(&first, "localhost") orelse return error.Dial;
    defer _ = c.close(sock);
    var peer: posix.sockaddr.storage = undefined;
    var len: posix.socklen_t = @sizeOf(posix.sockaddr.storage);
    try testing.expect(c.getpeername(sock, @ptrCast(&peer), &len) == 0);
    try testing.expectEqual(posix.AF.INET, peer.family);
}

test "a stop interrupts a connect" {
    // With a backlog of 0 the listener takes one connection and drops later SYNs,
    // so the second connect stays in progress.
    var listener = try Listener.open(0);
    defer listener.close();
    const first = try listener.connect();
    defer _ = c.close(first);
    const thread = try std.Thread.spawn(.{}, stopAfter, .{100 * ns_per_ms});
    defer stop.store(false, .monotonic);
    defer thread.join();

    const address = listener.info(.v4, null);
    const started = nowNs();
    const result = dial(&address, connect_timeout_ns);
    if (result == .connected) _ = c.close(result.connected);
    try testing.expectEqual(Dial.stopped, result);
    try testing.expect(nowNs() - started < ns_per_s);
}
