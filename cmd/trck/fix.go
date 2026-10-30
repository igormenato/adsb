package main

import (
	"bytes"
	"encoding/json"
	"math"
	"strings"
)

func ParseFix(line []byte) (Fix, bool) {
	var raw struct {
		Now       json.RawMessage `json:"now"`
		Hex       string          `json:"hex"`
		Lat       *float64        `json:"lat"`
		Lon       *float64        `json:"lon"`
		AltBaro   json.RawMessage `json:"alt_baro"`
		AltGeom   *float64        `json:"alt_geom"`
		GS        *float64        `json:"gs"`
		Track     *float64        `json:"track"`
		Emergency *string         `json:"emergency"`
		Squawk    json.RawMessage `json:"squawk"`
	}
	if json.Unmarshal(line, &raw) != nil {
		return Fix{}, false
	}
	unixS, ok := parseNow(raw.Now)
	if !ok || raw.Lat == nil || raw.Lon == nil {
		return Fix{}, false
	}
	icao, ok := parseICAO(raw.Hex)
	if !ok {
		return Fix{}, false
	}
	lat, lon := *raw.Lat, *raw.Lon
	if !finite(lat) || !finite(lon) || lat < -90 || lat > 90 || lon < -180 || lon > 180 {
		return Fix{}, false
	}
	name := ""
	if raw.Emergency != nil {
		name = *raw.Emergency
	}
	return Fix{
		UnixS:     unixS,
		ICAO:      icao,
		LatE7:     degE7(lat),
		LonE7:     degE7(lon),
		AltFt:     altitude(raw.AltBaro, raw.AltGeom),
		SpeedKt:   groundSpeed(raw.GS),
		TrackDeg:  track(raw.Track),
		Emergency: emergency(name, squawk(raw.Squawk)),
	}, true
}

func parseNow(raw json.RawMessage) (uint32, bool) {
	if len(raw) == 0 || raw[0] == '"' {
		return 0, false
	}
	var n json.Number
	if json.Unmarshal(raw, &n) != nil {
		return 0, false
	}
	v, err := n.Float64()
	if err != nil || !finite(v) || v < 0 || v > float64(math.MaxUint32) {
		return 0, false
	}
	return uint32(math.Trunc(v)), true
}

func parseICAO(hex string) (uint32, bool) {
	if strings.HasPrefix(hex, "~") {
		hex = hex[1:]
		if strings.HasPrefix(hex, "~") {
			return 0, false
		}
	}
	if len(hex) != 6 {
		return 0, false
	}
	var n uint32
	for i := 0; i < 6; i++ {
		v, ok := unhex(hex[i])
		if !ok {
			return 0, false
		}
		n = n<<4 | uint32(v)
	}
	return n, true
}

func unhex(b byte) (byte, bool) {
	switch {
	case b >= '0' && b <= '9':
		return b - '0', true
	case b >= 'a' && b <= 'f':
		return b - 'a' + 10, true
	case b >= 'A' && b <= 'F':
		return b - 'A' + 10, true
	default:
		return 0, false
	}
}

func finite(v float64) bool {
	return !math.IsNaN(v) && !math.IsInf(v, 0)
}

func altitude(altBaro json.RawMessage, altGeom *float64) int32 {
	if len(altBaro) > 0 {
		if bytes.Equal(altBaro, []byte(`"ground"`)) {
			return 0
		}
		var n float64
		if json.Unmarshal(altBaro, &n) == nil {
			return roundI32(n)
		}
	}
	if altGeom != nil {
		return roundI32(*altGeom)
	}
	return AltUnknown
}

func roundI32(n float64) int32 {
	if !finite(n) {
		return AltUnknown
	}
	r := math.Round(n)
	if r < float64(math.MinInt32) || r > float64(math.MaxInt32) {
		return AltUnknown
	}
	return int32(r)
}

func groundSpeed(gs *float64) uint16 {
	if gs == nil || !finite(*gs) || *gs < 0 {
		return SpeedUnknown
	}
	r := math.Round(*gs)
	if r >= 65535 {
		return 65534
	}
	return uint16(r)
}

func track(deg *float64) uint16 {
	if deg == nil || !finite(*deg) {
		return TrackUnknown
	}
	r := math.Round(*deg)
	if r < 0 || r > 360 {
		return TrackUnknown
	}
	if r == 360 {
		return 0
	}
	return uint16(r)
}

var emergencyByName = map[string]Emergency{
	"general":   General,
	"lifeguard": Lifeguard,
	"minfuel":   MinFuel,
	"nordo":     Nordo,
	"unlawful":  Unlawful,
	"downed":    Downed,
	"reserved":  Reserved,
}

func emergency(name string, fromSquawk Emergency) Emergency {
	name = strings.TrimSpace(name)
	if name == "" || strings.EqualFold(name, "none") {
		return fromSquawk
	}
	kind, ok := emergencyByName[strings.ToLower(name)]
	if !ok {
		return fromSquawk
	}
	if kind != Reserved {
		return kind
	}
	if fromSquawk != None {
		return fromSquawk
	}
	return Reserved
}

func squawk(raw json.RawMessage) Emergency {
	if len(raw) == 0 {
		return None
	}
	var s string
	if json.Unmarshal(raw, &s) == nil {
		return squawkDigits(strings.TrimSpace(s))
	}
	var n json.Number
	if json.Unmarshal(raw, &n) == nil {
		return squawkDigits(n.String())
	}
	return None
}

func squawkDigits(s string) Emergency {
	switch s {
	case "7700":
		return General
	case "7600":
		return Nordo
	case "7500":
		return Unlawful
	default:
		return None
	}
}
