package main

import (
	"encoding/binary"
	"math"
)

const (
	FrameSize    = 30
	SpeedUnknown = uint16(0xFFFF)
	TrackUnknown = uint16(0xFFFF)
	AltUnknown   = int32(math.MinInt32)
)

type Frame [FrameSize]byte

type Emergency uint8

const (
	None      Emergency = 0
	General   Emergency = 1
	Lifeguard Emergency = 2
	MinFuel   Emergency = 3
	Nordo     Emergency = 4
	Unlawful  Emergency = 5
	Downed    Emergency = 6
	Reserved  Emergency = 7
)

type Fix struct {
	UnixS     uint32
	ICAO      uint32
	LatE7     int32
	LonE7     int32
	AltFt     int32
	SpeedKt   uint16
	TrackDeg  uint16
	Emergency Emergency
}

func crc16IBM3740(data []byte) uint16 {
	crc := uint16(0xFFFF)
	for _, b := range data {
		crc ^= uint16(b) << 8
		for i := 0; i < 8; i++ {
			if crc&0x8000 != 0 {
				crc = crc<<1 ^ 0x1021
			} else {
				crc <<= 1
			}
		}
	}
	return crc
}

func degE7(degrees float64) int32 {
	scaled := math.Floor(math.Abs(degrees)*10_000_000 + 0.5)
	n := int32(scaled)
	if degrees < 0 {
		return -n
	}
	return n
}

func Pack(fix Fix) Frame {
	var raw Frame
	copy(raw[0:4], "TRCK")
	binary.LittleEndian.PutUint32(raw[4:8], fix.UnixS)
	icao := fix.ICAO & 0x00ffffff
	raw[8] = byte(icao)
	raw[9] = byte(icao >> 8)
	raw[10] = byte(icao >> 16)
	binary.LittleEndian.PutUint32(raw[11:15], uint32(fix.LatE7))
	binary.LittleEndian.PutUint32(raw[15:19], uint32(fix.LonE7))
	binary.LittleEndian.PutUint32(raw[19:23], uint32(fix.AltFt))
	binary.LittleEndian.PutUint16(raw[23:25], fix.SpeedKt)
	binary.LittleEndian.PutUint16(raw[25:27], fix.TrackDeg)
	raw[27] = byte(fix.Emergency)
	binary.LittleEndian.PutUint16(raw[28:30], crc16IBM3740(raw[:28]))
	return raw
}

func Unpack(bytes []byte) (Fix, bool) {
	if len(bytes) != FrameSize || string(bytes[0:4]) != "TRCK" {
		return Fix{}, false
	}
	got := binary.LittleEndian.Uint16(bytes[28:30])
	if crc16IBM3740(bytes[:28]) != got {
		return Fix{}, false
	}
	em := bytes[27]
	if em > 7 {
		return Fix{}, false
	}
	icao := uint32(bytes[8]) | uint32(bytes[9])<<8 | uint32(bytes[10])<<16
	return Fix{
		UnixS:     binary.LittleEndian.Uint32(bytes[4:8]),
		ICAO:      icao,
		LatE7:     int32(binary.LittleEndian.Uint32(bytes[11:15])),
		LonE7:     int32(binary.LittleEndian.Uint32(bytes[15:19])),
		AltFt:     int32(binary.LittleEndian.Uint32(bytes[19:23])),
		SpeedKt:   binary.LittleEndian.Uint16(bytes[23:25]),
		TrackDeg:  binary.LittleEndian.Uint16(bytes[25:27]),
		Emergency: Emergency(em),
	}, true
}

func CheckFixes() [3]Fix {
	const unixS = 1_700_000_000
	return [3]Fix{
		{
			UnixS: unixS, ICAO: 0x400ABC,
			LatE7: degE7(10.5), LonE7: degE7(-20.25),
			AltFt: 12_000, SpeedKt: 280, TrackDeg: 90,
		},
		{
			UnixS: unixS, ICAO: 0x40621D,
			LatE7: degE7(52.2572021484375), LonE7: degE7(3.91937255859375),
			AltFt: 38_000, SpeedKt: 450, TrackDeg: 271,
		},
		{
			UnixS: unixS, ICAO: 0x4CA123,
			LatE7: degE7(51.47), LonE7: degE7(-0.45),
			AltFt: 8_000, SpeedKt: 220, TrackDeg: 180, Emergency: General,
		},
	}
}

func CheckFrames() [3]Frame {
	fixes := CheckFixes()
	return [3]Frame{Pack(fixes[0]), Pack(fixes[1]), Pack(fixes[2])}
}

func frameICAO(f Frame) uint32 {
	return uint32(f[8]) | uint32(f[9])<<8 | uint32(f[10])<<16
}
