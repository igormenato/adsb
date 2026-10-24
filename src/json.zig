//! One readsb `--net-json-port` line into a `Fix`. `null` means skip the line.
//!
//! `now` has to be a Unix second that fits in a u32. `hex`, `lat`, and `lon` have to be present.
//! `seen_pos` is ignored. On this port it is 0, and the line's `now` is the fix time.

const std = @import("std");
const frame = @import("frame.zig");

const Emergency = frame.Emergency;
const Fix = frame.Fix;

/// readsb builds one JSON line in an 8192-byte buffer and drops what does not fit.
pub const max_line = 8192;

/// One top-level member, before it is checked. A later duplicate key replaces an earlier one.
const Member = union(enum) {
    absent,
    number: []const u8,
    string: []const u8,
    other,
};

const Line = struct {
    now: Member = .absent,
    hex: Member = .absent,
    lat: Member = .absent,
    lon: Member = .absent,
    alt_baro: Member = .absent,
    alt_geom: Member = .absent,
    gs: Member = .absent,
    track: Member = .absent,
    squawk: Member = .absent,
    emergency: Member = .absent,
};

pub fn parseLine(bytes: []const u8) ?Fix {
    var scratch: [2 * max_line]u8 = undefined;
    var fba: std.heap.FixedBufferAllocator = .init(&scratch);
    const line = scan(fba.allocator(), bytes) catch return null;

    const now = number(line.now) orelse return null;
    if (now < 0 or now > std.math.maxInt(u32)) return null;
    const lat = number(line.lat) orelse return null;
    const lon = number(line.lon) orelse return null;
    if (lat < -90 or lat > 90 or lon < -180 or lon > 180) return null;
    return .{
        .unix_s = @intFromFloat(now),
        .icao = icao(line.hex) orelse return null,
        .lat_e7 = frame.degE7(lat),
        .lon_e7 = frame.degE7(lon),
        .alt_ft = altitude(line),
        .gs_kt = groundSpeed(line.gs),
        .track_deg = track(line.track),
        .emergency = emergency(line),
    };
}

/// The whole line has to be one JSON object. Members this file does not read are skipped.
fn scan(gpa: std.mem.Allocator, bytes: []const u8) !Line {
    var scanner: std.json.Scanner = .initCompleteInput(gpa, bytes);
    var line: Line = .{};
    if (try scanner.next() != .object_begin) return error.NotAnObject;
    while (true) {
        const key = switch (try scanner.nextAlloc(gpa, .alloc_if_needed)) {
            .object_end => break,
            .string => |text| text,
            .allocated_string => |text| text,
            else => unreachable,
        };
        const member = try scanMember(gpa, &scanner);
        inline for (@typeInfo(Line).@"struct".field_names) |name| {
            if (std.mem.eql(u8, key, name)) @field(line, name) = member;
        }
    }
    if (try scanner.next() != .end_of_document) unreachable;
    return line;
}

fn scanMember(gpa: std.mem.Allocator, scanner: *std.json.Scanner) !Member {
    switch (try scanner.nextAlloc(gpa, .alloc_if_needed)) {
        .number => |text| return .{ .number = text },
        .allocated_number => |text| return .{ .number = text },
        .string => |text| return .{ .string = text },
        .allocated_string => |text| return .{ .string = text },
        .object_begin, .array_begin => {
            try scanner.skipUntilStackHeight(1);
            return .other;
        },
        .true, .false, .null => return .other,
        else => unreachable,
    }
}

/// A finite JSON number.
fn number(member: Member) ?f64 {
    const text = switch (member) {
        .number => |text| text,
        else => return null,
    };
    const value = std.fmt.parseFloat(f64, text) catch return null;
    return if (std.math.isFinite(value)) value else null;
}

/// readsb writes six hex digits, optionally after one `~`.
fn icao(member: Member) ?u24 {
    const hex = switch (member) {
        .string => |text| text,
        else => return null,
    };
    const digits = std.mem.cutPrefix(u8, hex, "~") orelse hex;
    if (digits.len != 6) return null;
    for (digits) |digit| if (!std.ascii.isHex(digit)) return null;
    return std.fmt.parseInt(u24, digits, 16) catch unreachable;
}

fn altitude(line: Line) i32 {
    switch (line.alt_baro) {
        .string => |text| if (std.mem.eql(u8, text, "ground")) return 0,
        .number => return roundAltitude(number(line.alt_baro)),
        else => {},
    }
    return roundAltitude(number(line.alt_geom));
}

fn roundAltitude(feet: ?f64) i32 {
    const rounded = @round(feet orelse return frame.alt_unknown);
    if (rounded < std.math.minInt(i32) or rounded > std.math.maxInt(i32)) return frame.alt_unknown;
    return @intFromFloat(rounded);
}

fn groundSpeed(member: Member) u16 {
    const knots = number(member) orelse return frame.speed_unknown;
    if (knots < 0) return frame.speed_unknown;
    const rounded = @round(knots);
    return if (rounded >= frame.speed_unknown) frame.speed_unknown - 1 else @intFromFloat(rounded);
}

fn track(member: Member) u16 {
    const rounded = @round(number(member) orelse return frame.track_unknown);
    if (rounded < 0 or rounded > 360) return frame.track_unknown;
    return if (rounded == 360) 0 else @intFromFloat(rounded);
}

/// A known name wins. `reserved` still yields to squawk 7700, 7600, or 7500.
/// Those squawks apply when the name is missing, empty, `none`, or unknown.
fn emergency(line: Line) Emergency {
    const named: Emergency = switch (line.emergency) {
        .string => |text| byName(std.mem.trim(u8, text, &std.ascii.whitespace)),
        else => .none,
    };
    if (named != .none and named != .reserved) return named;
    const squawked = bySquawk(line.squawk);
    return if (squawked != .none) squawked else named;
}

fn byName(text: []const u8) Emergency {
    for (std.enums.values(Emergency)) |kind| {
        if (std.ascii.eqlIgnoreCase(text, @tagName(kind))) return kind;
    }
    return .none;
}

/// readsb prints four digits. `07700` is not `7700`.
fn bySquawk(member: Member) Emergency {
    const code = switch (member) {
        .string => |text| std.mem.trim(u8, text, &std.ascii.whitespace),
        .number => |text| text,
        else => return .none,
    };
    if (std.mem.eql(u8, code, "7700")) return .general;
    if (std.mem.eql(u8, code, "7600")) return .nordo;
    if (std.mem.eql(u8, code, "7500")) return .unlawful;
    return .none;
}

const testing = std.testing;

const published_line =
    \\{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4}
;
const position =
    \\"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375
;

test "a published line is the README frame" {
    const fix = parseLine(published_line).?;
    try testing.expectEqual(Fix{
        .unix_s = 1_700_000_000,
        .icao = 0x40621D,
        .lat_e7 = 522_572_021,
        .lon_e7 = 39_193_726,
        .alt_ft = 38_000,
        .gs_kt = 450,
        .track_deg = 271,
        .emergency = .none,
    }, fix);
    try testing.expectEqual(frame.fromHex(frame.readme_ordinary), frame.pack(fix));
}

test "a line uses now and ignores seen_pos" {
    const old = parseLine("{" ++ position ++ ",\"seen_pos\":100}").?;
    try testing.expectEqual(1_700_000_000, old.unix_s);
    try testing.expectEqual(0x40621D, old.icao);
}

test "bad lines are skipped" {
    const skipped = [_][]const u8{
        "",
        "not json",
        "[1,2]",
        \\{"hex":"40621d","lat":1,"lon":2}
        ,
        \\{"now":-1,"hex":"40621d","lat":1,"lon":2}
        ,
        \\{"now":"1700000000","hex":"40621d","lat":1,"lon":2}
        ,
        \\{"now":1e20,"hex":"40621d","lat":1,"lon":2}
        ,
        \\{"now":4294967296,"hex":"40621d","lat":1,"lon":2}
        ,
        \\{"now":1700000000,"hex":"40621d","lon":2}
        ,
        \\{"now":1700000000,"hex":"40621d","lat":1}
        ,
        \\{"now":1700000000,"hex":"40621d","lat":"1","lon":2}
        ,
        \\{"now":1700000000,"hex":"40621d","lat":1,"lon":2} x
        ,
        \\{"now":1700000000,"hex":"40621d","lat":1,"lon":2,"lat":null}
        ,
        \\{"now":1,"aircraft":[]}
        ,
        \\{"now":1700000000.9,"aircraft":[{"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375}]}
        ,
        \\{"now":30,"hex":"40621d","squawk":"7700","emergency":"general"}
        ,
        \\{"now":1,"hex":"40621d","lat":91,"lon":2}
        ,
        \\{"now":1,"hex":"40621d","lat":1,"lon":-180.5}
        ,
    };
    for (skipped) |line| {
        testing.expect(parseLine(line) == null) catch |err| {
            std.debug.print("accepted: {s}\n", .{line});
            return err;
        };
    }
}

test "hex" {
    try testing.expectEqual(0x40621D, parseLine(
        \\{"now":1,"hex":"~40621d","lat":1,"lon":2}
    ).?.icao);
    try testing.expectEqual(0xABCDEF, parseLine(
        \\{"now":1,"hex":"ABCDEF","lat":1,"lon":2}
    ).?.icao);
    const bad = [_][]const u8{ "40G621", "abcdefg", " abc", "~~40621d", "+40621", "40621", "" };
    for (bad) |hex| {
        var buf: [64]u8 = undefined;
        const line = try std.fmt.bufPrint(&buf, "{{\"now\":1,\"hex\":\"{s}\",\"lat\":1,\"lon\":2}}", .{hex});
        try testing.expect(parseLine(line) == null);
    }
}

test "unknown, saturated, and geometric" {
    const unknown = parseLine(
        \\{"now":10,"hex":"abc123","lat":0,"lon":0,"gs":-1,"track":-5}
    ).?;
    try testing.expectEqual(frame.alt_unknown, unknown.alt_ft);
    try testing.expectEqual(frame.speed_unknown, unknown.gs_kt);
    try testing.expectEqual(frame.track_unknown, unknown.track_deg);
    try testing.expectEqual(frame.fromHex(
        "54 52 43 4B 0A 00 00 00 23 C1 AB 00 00 00 00 00 00 00 00 00 00 00 80 FF FF FF FF 00 2A 6A",
    ), frame.pack(unknown));

    const saturated = parseLine(
        \\{"now":10,"hex":"abc124","lat":0,"lon":1,"alt_baro":"ground","gs":65534.6,"track":360}
    ).?;
    try testing.expectEqual(0, saturated.alt_ft);
    try testing.expectEqual(65534, saturated.gs_kt);
    try testing.expectEqual(0, saturated.track_deg);
    try testing.expectEqual(frame.fromHex(
        "54 52 43 4B 0A 00 00 00 24 C1 AB 00 00 00 00 80 96 98 00 00 00 00 00 FE FF 00 00 00 A8 0D",
    ), frame.pack(saturated));

    const geometric = parseLine(
        \\{"now":10,"hex":"000001","lat":1,"lon":2,"alt_geom":3280.4}
    ).?;
    try testing.expectEqual(3280, geometric.alt_ft);
    try testing.expectEqual(frame.speed_unknown, geometric.gs_kt);
    try testing.expectEqual(frame.track_unknown, geometric.track_deg);

    const second = parseLine(
        \\{"now":1700000000.9,"hex":"400abc","lat":10.5,"lon":-20.25,"alt_baro":12000,"gs":280,"track":90}
    ).?;
    try testing.expectEqual(frame.fromHex(
        "54 52 43 4B 00 F1 53 65 BC 0A 40 40 2C 42 06 60 18 EE F3 E0 2E 00 00 18 01 5A 00 00 D7 48",
    ), frame.pack(second));
}

test "emergency names and squawks" {
    const cases = [_]struct { []const u8, Emergency }{
        .{ "\"squawk\":\"7700\"", .general },
        .{ "\"squawk\":\"7600\"", .nordo },
        .{ "\"squawk\":\"7500\"", .unlawful },
        .{ "\"squawk\":7700", .general },
        .{ "\"squawk\":7600", .nordo },
        .{ "\"squawk\":7500", .unlawful },
        .{ "\"squawk\":\" 7700 \"", .general },
        .{ "\"squawk\":\"07700\"", .none },
        .{ "\"squawk\":\"77000\"", .none },
        .{ "\"squawk\":\"0770\"", .none },
        .{ "\"squawk\":7700.0", .none },
        .{ "\"emergency\":\"general\"", .general },
        .{ "\"emergency\":\"lifeguard\"", .lifeguard },
        .{ "\"emergency\":\"minfuel\"", .minfuel },
        .{ "\"emergency\":\"nordo\"", .nordo },
        .{ "\"emergency\":\"unlawful\"", .unlawful },
        .{ "\"emergency\":\"downed\"", .downed },
        .{ "\"emergency\":\"GENERAL\"", .general },
        .{ "\"emergency\":\"unlawful\",\"squawk\":\"7700\"", .unlawful },
        .{ "\"emergency\":\"none\",\"squawk\":\"7700\"", .general },
        .{ "\"emergency\":\"\",\"squawk\":\"7600\"", .nordo },
        .{ "\"emergency\":\"none\"", .none },
        .{ "\"squawk\":\"1000\"", .none },
        .{ "\"emergency\":\"medical\"", .none },
        .{ "\"emergency\":\"medical\",\"squawk\":\"7700\"", .general },
        .{ "\"emergency\":7,\"squawk\":\"7500\"", .unlawful },
        .{ "\"emergency\":\"reserved\"", .reserved },
        .{ "\"emergency\":\"RESERVED\"", .reserved },
        .{ "\"emergency\":\"reserved\",\"squawk\":\"1000\"", .reserved },
        .{ "\"emergency\":\"reserved\",\"squawk\":\"7700\"", .general },
        .{ "\"emergency\":\"reserved\",\"squawk\":\"7600\"", .nordo },
        .{ "\"emergency\":\"reserved\",\"squawk\":\"7500\"", .unlawful },
    };
    for (cases) |case| {
        const extra, const expected = case;
        var buf: [256]u8 = undefined;
        const line = try std.fmt.bufPrint(&buf, "{{" ++ position ++ ",{s}}}", .{extra});
        const fix = parseLine(line).?;
        testing.expectEqual(expected, fix.emergency) catch |err| {
            std.debug.print("case: {s}\n", .{extra});
            return err;
        };
        try testing.expectEqual(@backingInt(expected), frame.pack(fix)[27]);
    }

    const general = frame.fromHex(frame.readme_general);
    try testing.expectEqual(general, frame.pack(parseLine(
        \\{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"emergency":"general"}
    ).?));
    try testing.expectEqual(general, frame.pack(parseLine(
        \\{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"squawk":"7700"}
    ).?));
}

test "escaped keys and values read as written" {
    const fix = parseLine(
        \\{"n\u006fw":1700000000,"hex":"\u0034\u00306\u0032\u0031d","lat":1,"lon":2,"squawk":"\u0037700"}
    ).?;
    try testing.expectEqual(1_700_000_000, fix.unix_s);
    try testing.expectEqual(0x40621D, fix.icao);
    try testing.expectEqual(Emergency.general, fix.emergency);
}
