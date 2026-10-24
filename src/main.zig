//! Read readsb `--net-json-port` and write one UART frame per position.

const std = @import("std");
const clap = @import("clap");
const serial = @import("serial");
const frame = @import("frame.zig");
const json = @import("json.zig");

const c = std.c;
const posix = std.posix;
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

const params = clap.parseParamsComptime(
    \\-h, --help             Print help.
    \\--net-json <str>       Host and port of readsb --net-json-port.
    \\--uart <str>           UART device, or - for stdout.
    \\--baud <u32>           Baud rate.
    \\--interval <f64>       Seconds between --check passes. The JSON port ignores this.
    \\--check                Send the known frames instead of reading readsb.
    \\
);

/// One run reads readsb or checks the UART, never both.
const Mode = union(enum) {
    check,
    net_json: []const u8,
};

const Options = struct {
    mode: Mode,
    uart: []const u8,
    baud: u32,
    interval_s: f64,
};

/// Set by SIGINT. Every wait below is sliced, so a stop is seen within one slice.
var stop: std.atomic.Value(bool) = .init(false);

fn stopping() bool {
    return stop.load(.monotonic);
}

pub fn main(init: std.process.Init) u8 {
    ignoreSigpipe();
    const options = parseCommand(init) catch |err| switch (err) {
        error.Help => return 0,
        error.Conflict => {
            print("error: --check and --net-json cannot be used together\n", .{});
            return 1;
        },
        error.MissingMode => {
            print("error: pass --net-json HOST:PORT, or --check\n", .{});
            return 1;
        },
        else => return 1,
    };

    var writer: Writer = .{
        .sink = if (std.mem.eql(u8, options.uart, "-"))
            .stdout
        else
            .{ .uart = openUart(options.uart, options.baud) orelse return 1 },
    };
    defer switch (writer.sink) {
        .uart => |*port| port.close(),
        .stdout => {},
    };
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

fn parseCommand(init: std.process.Init) !Options {
    var diag: clap.Diagnostic = .{};
    var res = clap.parse(clap.Help, &params, clap.parsers.default, init.minimal.args, .{
        .diagnostic = &diag,
        .allocator = init.gpa,
    }) catch |err| {
        diag.reportToFile(init.io, .stderr(), err) catch {};
        return err;
    };
    defer res.deinit();

    if (res.args.help != 0) {
        clap.helpToFile(init.io, .stdout(), clap.Help, &params, .{}) catch {};
        return error.Help;
    }
    return .{
        .mode = try decide(res.args.check != 0, res.args.@"net-json"),
        .uart = res.args.uart orelse default_uart,
        .baud = res.args.baud orelse default_baud,
        .interval_s = res.args.interval orelse default_interval_s,
    };
}

fn decide(check: bool, net_json: ?[]const u8) error{ Conflict, MissingMode }!Mode {
    if (check and net_json != null) return error.Conflict;
    if (check) return .check;
    return .{ .net_json = net_json orelse return error.MissingMode };
}

/// The `--check` pause, at least 0.2 s. A non-finite value is 0.2 s. A value that does not fit
/// in nanoseconds is 1 s.
fn checkIntervalNs(interval_s: f64) u64 {
    if (!std.math.isFinite(interval_s) or interval_s < min_interval_s) return @intFromFloat(min_interval_s * ns_per_s);
    if (interval_s >= 0x1p64) return ns_per_s;
    const nanoseconds = interval_s * ns_per_s;
    if (nanoseconds >= 0x1p64) return std.math.maxInt(u64);
    return @intFromFloat(nanoseconds);
}

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

fn failed(rc: c_int) ?posix.E {
    return if (rc < 0) posix.errno(rc) else null;
}

const Sink = union(enum) {
    uart: serial.Port,
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
            .uart => |*port| port.handle,
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
        for (writer.queue[0..writer.queued], 0..) |raw, index| {
            if (raw[27] == 0) return index;
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
            const n: usize = switch (writer.sink) {
                .uart => |*port| port.write(rest) catch |err| switch (err) {
                    error.WouldBlock => return,
                    else => {
                        print("uart write failed: {t}\n", .{err});
                        return error.UartFailed;
                    },
                },
                .stdout => writeStdout(rest) catch |err| switch (err) {
                    error.WouldBlock => return,
                    error.Failed => {
                        print("uart write failed\n", .{});
                        return error.UartFailed;
                    },
                },
            };
            if (n == 0) {
                print("uart write wrote nothing\n", .{});
                return error.UartFailed;
            }
            writer.sent += n;
            if (writer.sent == frame.len) writer.current = null;
        }
    }
};

fn writeStdout(bytes: []const u8) error{ WouldBlock, Failed }!usize {
    while (true) {
        const rc = c.write(posix.STDOUT_FILENO, bytes.ptr, bytes.len);
        if (rc > 0) return @intCast(rc);
        if (rc == 0) return error.Failed;
        switch (posix.errno(rc)) {
            .AGAIN => return error.WouldBlock,
            .INTR => continue,
            else => return error.Failed,
        }
    }
}

fn logDrop(raw: frame.Frame) void {
    print("dropped frame icao={x:0>6} emergency={d}\n", .{ std.mem.readInt(u24, raw[8..11], .little), raw[27] });
}

fn openUart(path: []const u8, baud: u32) ?serial.Port {
    return serial.Port.open(path, .{ .baud = baud }, .{}) catch |err| {
        print("cannot open {s}: {t}\n", .{ path, err });
        return null;
    };
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

/// Queue a frame for each whole line, then keep the partial line.
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

fn resolve(text: []const u8) ?*posix.addrinfo {
    var host, const port_text = std.mem.cutScalarLast(u8, text, ':') orelse return null;
    const port = std.fmt.parseInt(u16, port_text, 10) catch return null;
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

fn onSigint(_: posix.SIG) callconv(.c) void {
    stop.store(true, .monotonic);
}

fn ignoreSigpipe() void {
    const ignore: posix.Sigaction = .{ .handler = .{ .handler = posix.SIG.IGN }, .mask = posix.sigemptyset(), .flags = 0 };
    posix.sigaction(.PIPE, &ignore, null);
}

fn catchSigint() void {
    const action: posix.Sigaction = .{ .handler = .{ .handler = onSigint }, .mask = posix.sigemptyset(), .flags = posix.SA.RESTART };
    posix.sigaction(.INT, &action, null);
}

const testing = std.testing;

test {
    _ = frame;
    _ = json;
}

fn parsedMode(argv: []const []const u8) !Mode {
    var iter: clap.args.SliceIterator = .{ .args = argv };
    var res = try clap.parseEx(clap.Help, &params, clap.parsers.default, &iter, .{
        .allocator = testing.allocator,
    });
    defer res.deinit();
    return decide(res.args.check != 0, res.args.@"net-json");
}

test "--check and --net-json together are an error" {
    try testing.expectError(error.Conflict, parsedMode(&.{ "--check", "--net-json", "127.0.0.1:9" }));
    try testing.expectError(error.Conflict, parsedMode(&.{ "--net-json=127.0.0.1:9", "--check" }));
    try testing.expectEqual(Mode.check, try parsedMode(&.{"--check"}));
    try testing.expectEqualStrings("127.0.0.1:9", (try parsedMode(&.{ "--net-json", "127.0.0.1:9" })).net_json);
}

test "a 7700 survives a queue full of ordinary frames" {
    var writer: Writer = .{ .sink = .stdout };
    for (0..queue_cap) |icao| writer.enqueue(try queued(@intCast(icao), false));
    writer.enqueue(try queued(0x7700, true));
    try testing.expectEqual(queue_cap, writer.queued);
    try testing.expectEqual(@backingInt(frame.Emergency.general), writer.queue[queue_cap - 1][27]);
    try testing.expectEqual(@as(u24, 1), std.mem.readInt(u24, writer.queue[0][8..11], .little));

    writer.queued = 0;
    for (1..queue_cap + 1) |icao| writer.enqueue(try queued(@intCast(icao), true));
    writer.enqueue(try queued(0x100, false));
    try testing.expectEqual(queue_cap, writer.queued);
    for (writer.queue) |raw| try testing.expect(raw[27] != 0);
}

fn queued(icao: u24, emergency: bool) !frame.Frame {
    var buf: [128]u8 = undefined;
    const line = try std.fmt.bufPrint(&buf, "{{\"now\":1700000000,\"hex\":\"{x:0>6}\",\"lat\":1,\"lon\":2{s}}}", .{
        icao,
        if (emergency) ",\"squawk\":\"7700\"" else "",
    });
    return frame.pack(json.parseLine(line).?);
}
