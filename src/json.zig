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

const Position = struct {
    now: f64,
    hex: []const u8,
    lat: f64,
    lon: f64,
    alt_baro: ?Baro = null,
    alt_geom: ?f64 = null,
    gs: ?f64 = null,
    track: ?f64 = null,
    squawk: ?Squawk = null,
    emergency: ?[]const u8 = null,
};

/// `alt_baro` is a number of feet, or the string `ground`. Anything else is ignored.
const Baro = struct {
    feet: ?f64 = null,
    ground: bool = false,

    pub fn jsonParse(allocator: std.mem.Allocator, source: anytype, options: std.json.ParseOptions) !Baro {
        return switch (try source.peekNextTokenType()) {
            .number => .{ .feet = try std.json.innerParse(f64, allocator, source, options) },
            .string => .{ .ground = std.mem.eql(u8, try std.json.innerParse([]const u8, allocator, source, options), "ground") },
            else => error.UnexpectedToken,
        };
    }
};

/// A squawk is four digits. readsb sends a string; a JSON integer is the same digits.
const Squawk = struct {
    text: []const u8,

    pub fn jsonParse(allocator: std.mem.Allocator, source: anytype, options: std.json.ParseOptions) !Squawk {
        const when = options.allocate orelse .alloc_if_needed;
        const max = options.max_value_len orelse max_line;
        return switch (try source.nextAllocMax(allocator, when, max)) {
            inline .number, .allocated_number, .string, .allocated_string => |text| .{ .text = text },
            else => error.UnexpectedToken,
        };
    }
};

pub fn parseLine(bytes: []const u8) ?Fix {
    var scratch: [32 * 1024]u8 = undefined;
    var fba: std.heap.FixedBufferAllocator = .init(&scratch);
    const parsed = std.json.parseFromSlice(Position, fba.allocator(), bytes, .{
        .ignore_unknown_fields = true,
        .duplicate_field_behavior = .use_last,
    }) catch return null;
    defer parsed.deinit();
    const line = parsed.value;

    if (!std.math.isFinite(line.now) or line.now < 0 or line.now > std.math.maxInt(u32)) return null;
    if (line.lat < -90 or line.lat > 90 or line.lon < -180 or line.lon > 180) return null;
    return .{
        .unix_s = @intFromFloat(line.now),
        .icao = icao(line.hex) orelse return null,
        .lat_e7 = frame.degE7(line.lat),
        .lon_e7 = frame.degE7(line.lon),
        .alt_ft = altitude(line),
        .gs_kt = groundSpeed(line.gs),
        .track_deg = track(line.track),
        .emergency = emergency(line),
    };
}

/// readsb writes six hex digits, optionally after one `~`.
fn icao(hex: []const u8) ?u24 {
    const digits = std.mem.cutPrefix(u8, hex, "~") orelse hex;
    if (digits.len != 6) return null;
    for (digits) |digit| if (!std.ascii.isHex(digit)) return null;
    return std.fmt.parseInt(u24, digits, 16) catch null;
}

fn altitude(line: Position) i32 {
    if (line.alt_baro) |baro| {
        if (baro.ground) return 0;
        if (baro.feet) |feet| return roundAltitude(feet);
    }
    return roundAltitude(line.alt_geom);
}

fn roundAltitude(feet: ?f64) i32 {
    const value = feet orelse return frame.alt_unknown;
    if (!std.math.isFinite(value)) return frame.alt_unknown;
    const rounded = @round(value);
    if (rounded < std.math.minInt(i32) or rounded > std.math.maxInt(i32)) return frame.alt_unknown;
    return @intFromFloat(rounded);
}

fn groundSpeed(knots: ?f64) u16 {
    const value = knots orelse return frame.speed_unknown;
    if (!std.math.isFinite(value) or value < 0) return frame.speed_unknown;
    const rounded = @round(value);
    return if (rounded >= frame.speed_unknown) frame.speed_unknown - 1 else @intFromFloat(rounded);
}

fn track(degrees: ?f64) u16 {
    const value = degrees orelse return frame.track_unknown;
    if (!std.math.isFinite(value)) return frame.track_unknown;
    const rounded = @round(value);
    if (rounded < 0 or rounded > 360) return frame.track_unknown;
    return if (rounded == 360) 0 else @intFromFloat(rounded);
}

/// A known name wins. `reserved` still yields to squawk 7700, 7600, or 7500.
/// Those squawks apply when the name is missing, empty, `none`, or unknown.
fn emergency(line: Position) Emergency {
    const named = byName(std.mem.trim(u8, line.emergency orelse "", &std.ascii.whitespace));
    if (named != .none and named != .reserved) return named;
    const squawked = bySquawk(if (line.squawk) |squawk| std.mem.trim(u8, squawk.text, &std.ascii.whitespace) else "");
    return if (squawked != .none) squawked else named;
}

fn byName(text: []const u8) Emergency {
    if (text.len == 0 or std.ascii.eqlIgnoreCase(text, "none")) return .none;
    for (std.enums.values(Emergency)) |kind| {
        if (std.ascii.eqlIgnoreCase(text, @tagName(kind))) return kind;
    }
    return .none;
}

/// readsb prints four digits. `07700` is not `7700`.
fn bySquawk(text: []const u8) Emergency {
    if (std.mem.eql(u8, text, "7700")) return .general;
    if (std.mem.eql(u8, text, "7600")) return .nordo;
    if (std.mem.eql(u8, text, "7500")) return .unlawful;
    return .none;
}

const testing = std.testing;

const position =
    \\"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375
;

test "a published line is the README frame" {
    const fix = parseLine(
        \\{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4}
    ).?;
    try testing.expectEqual(frame.fromHex(frame.readme_ordinary), frame.pack(fix));
}

test "field rules" {
    const ground = parseLine(
        \\{"now":10,"hex":"abc124","lat":0,"lon":1,"alt_baro":"ground","gs":65534.6,"track":360}
    ).?;
    try testing.expectEqual(@as(i32, 0), ground.alt_ft);
    try testing.expectEqual(@as(u16, 65534), ground.gs_kt);
    try testing.expectEqual(@as(u16, 0), ground.track_deg);

    const tilde = parseLine(
        \\{"now":1,"hex":"~40621d","lat":1,"lon":2}
    ).?;
    try testing.expectEqual(@as(u24, 0x40621d), tilde.icao);

    const geom = parseLine(
        \\{"now":10,"hex":"000001","lat":1,"lon":2,"alt_geom":3280.4}
    ).?;
    try testing.expectEqual(@as(i32, 3280), geom.alt_ft);
}

test "a long line with extra fields still parses" {
    var buf: [max_line]u8 = undefined;
    const prefix =
        \\{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"flight":"TEST123 ","lastPosition":{"lat":1,"lon":2},"nav_modes":["althold"]
    ;
    @memcpy(buf[0..prefix.len], prefix);
    var len: usize = prefix.len;
    var n: usize = 0;
    while (len + 24 < max_line) : (n += 1) {
        const piece = std.fmt.bufPrint(buf[len..], ",\"x{d}\":{d}", .{ n, n }) catch break;
        len += piece.len;
    }
    buf[len] = '}';
    len += 1;
    const fix = parseLine(buf[0..len]) orelse return error.Skipped;
    try testing.expectEqual(frame.fromHex(frame.readme_ordinary), frame.pack(fix));
}

test "bad lines are skipped" {
    const skipped = [_][]const u8{
        "",
        "not json",
        \\{"hex":"40621d","lat":1,"lon":2}
        ,
        \\{"now":-1,"hex":"40621d","lat":1,"lon":2}
        ,
        \\{"now":1700000000,"hex":"40621d","lat":91,"lon":2}
        ,
        \\{"now":1,"hex":"40621","lat":1,"lon":2}
        ,
    };
    for (skipped) |line| try testing.expect(parseLine(line) == null);
}

test "emergency names and squawks" {
    const cases = [_]struct { []const u8, Emergency }{
        .{ "\"squawk\":\"7700\"", .general },
        .{ "\"squawk\":\"7600\"", .nordo },
        .{ "\"squawk\":\"7500\"", .unlawful },
        .{ "\"squawk\":7700", .general },
        .{ "\"squawk\":\"07700\"", .none },
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
        .{ "\"emergency\":\"medical\",\"squawk\":\"7700\"", .general },
        .{ "\"emergency\":\"reserved\"", .reserved },
        .{ "\"emergency\":\"reserved\",\"squawk\":\"7700\"", .general },
        .{ "\"emergency\":\"reserved\",\"squawk\":\"1000\"", .reserved },
        .{ "\"alt_baro\":\"ground\"", .none },
    };
    for (cases) |case| {
        const extra, const expected = case;
        var buf: [256]u8 = undefined;
        const line = try std.fmt.bufPrint(&buf, "{{" ++ position ++ ",{s}}}", .{extra});
        const fix = parseLine(line) orelse return error.Skipped;
        try testing.expectEqual(expected, fix.emergency);
    }

    const general = frame.fromHex(frame.readme_general);
    try testing.expectEqual(general, frame.pack(parseLine(
        \\{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"emergency":"general"}
    ).?));
    try testing.expectEqual(general, frame.pack(parseLine(
        \\{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"squawk":"7700"}
    ).?));
}
