//! One UART frame for one aircraft. Little-endian. The README tables are the layout.

const std = @import("std");

pub const len = 30;
pub const Frame = [len]u8;

pub const alt_unknown: i32 = std.math.minInt(i32);
pub const speed_unknown: u16 = 65535;
pub const track_unknown: u16 = 65535;

/// The tag value is the byte at offset 27.
pub const Emergency = enum(u8) {
    none,
    general,
    lifeguard,
    minfuel,
    nordo,
    unlawful,
    downed,
    reserved,
};

/// One accepted position, already in wire units.
pub const Fix = struct {
    unix_s: u32,
    icao: u24,
    lat_e7: i32,
    lon_e7: i32,
    alt_ft: i32,
    gs_kt: u16,
    track_deg: u16,
    emergency: Emergency,
};

/// CRC-16/CCITT-FALSE: poly 0x1021, init 0xFFFF, not reflected, xorout 0.
/// The std catalog names it CRC-16/IBM-3740.
pub const crc16 = std.hash.crc.@"CRC-16/IBM-3740".hash;

/// `floor(|degrees| * 1e7 + 0.5)` with the original sign.
pub fn degE7(degrees: f64) i32 {
    const scaled: i32 = @intFromFloat(@floor(@abs(degrees) * 1e7 + 0.5));
    return if (degrees < 0) -scaled else scaled;
}

pub fn pack(fix: Fix) Frame {
    var raw: Frame = undefined;
    raw[0..4].* = "TRCK".*;
    std.mem.writeInt(u32, raw[4..8], fix.unix_s, .little);
    std.mem.writeInt(u24, raw[8..11], fix.icao, .little);
    std.mem.writeInt(i32, raw[11..15], fix.lat_e7, .little);
    std.mem.writeInt(i32, raw[15..19], fix.lon_e7, .little);
    std.mem.writeInt(i32, raw[19..23], fix.alt_ft, .little);
    std.mem.writeInt(u16, raw[23..25], fix.gs_kt, .little);
    std.mem.writeInt(u16, raw[25..27], fix.track_deg, .little);
    raw[27] = @backingInt(fix.emergency);
    std.mem.writeInt(u16, raw[28..30], crc16(raw[0..28]), .little);
    return raw;
}

/// What `--check` sends, through `pack`.
pub const check_positions = [_]Fix{
    .{
        .unix_s = 1_700_000_000,
        .icao = 0x400ABC,
        .lat_e7 = degE7(10.5),
        .lon_e7 = degE7(-20.25),
        .alt_ft = 12_000,
        .gs_kt = 280,
        .track_deg = 90,
        .emergency = .none,
    },
    .{
        .unix_s = 1_700_000_000,
        .icao = 0x40621D,
        .lat_e7 = degE7(52.2572021484375),
        .lon_e7 = degE7(3.91937255859375),
        .alt_ft = 38_000,
        .gs_kt = 450,
        .track_deg = 271,
        .emergency = .none,
    },
    .{
        .unix_s = 1_700_000_000,
        .icao = 0x4CA123,
        .lat_e7 = degE7(51.47),
        .lon_e7 = degE7(-0.45),
        .alt_ft = 8_000,
        .gs_kt = 220,
        .track_deg = 180,
        .emergency = .general,
    },
};

/// A frame written as the README writes it: hex bytes separated by spaces.
pub fn fromHex(text: []const u8) Frame {
    var raw: Frame = undefined;
    var bytes = std.mem.tokenizeScalar(u8, text, ' ');
    for (&raw) |*byte| byte.* = std.fmt.parseInt(u8, bytes.next().?, 16) catch unreachable;
    std.debug.assert(bytes.next() == null);
    return raw;
}

pub const readme_ordinary = "54 52 43 4B 00 F1 53 65 1D 62 40 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 C2 01 0F 01 00 73 81";
pub const readme_general = "54 52 43 4B 00 F1 53 65 1D 62 40 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 C2 01 0F 01 01 52 91";

const readme_fix: Fix = .{
    .unix_s = 1_700_000_000,
    .icao = 0x40621d,
    .lat_e7 = 522_572_021,
    .lon_e7 = 39_193_726,
    .alt_ft = 38_000,
    .gs_kt = 450,
    .track_deg = 271,
    .emergency = .none,
};

test "crc check value" {
    try std.testing.expectEqual(0x29B1, crc16("123456789"));
}

test "pack writes the README frames" {
    try std.testing.expectEqual(fromHex(readme_ordinary), pack(readme_fix));
    var general = readme_fix;
    general.emergency = .general;
    try std.testing.expectEqual(fromHex(readme_general), pack(general));
}

test "emergency wire values" {
    var fix = readme_fix;
    for (std.enums.values(Emergency), 0..) |emergency, code| {
        fix.emergency = emergency;
        try std.testing.expectEqual(code, pack(fix)[27]);
    }
    try std.testing.expectEqual(8, std.enums.values(Emergency).len);
}

test "check positions" {
    try std.testing.expectEqual(3, check_positions.len);
    try std.testing.expectEqual(fromHex(readme_ordinary), pack(check_positions[1]));
    const icaos = [_]u24{ 0x400ABC, 0x40621D, 0x4CA123 };
    const emergencies = [_]u8{ 0, 0, 1 };
    for (check_positions, icaos, emergencies) |fix, icao, emergency| {
        const raw = pack(fix);
        try std.testing.expectEqual(1_700_000_000, std.mem.readInt(u32, raw[4..8], .little));
        try std.testing.expectEqual(icao, std.mem.readInt(u24, raw[8..11], .little));
        try std.testing.expectEqual(emergency, raw[27]);
    }
}
