package main

import (
	"bytes"
	"testing"
)

var (
	published = Frame{
		0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0x1d, 0x62, 0x40, 0xf5, 0xd0, 0x25, 0x1f, 0x7e,
		0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xc2, 0x01, 0x0f, 0x01, 0x00, 0x73, 0x81,
	}
	emergencyGeneral = Frame{
		0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0x1d, 0x62, 0x40, 0xf5, 0xd0, 0x25, 0x1f, 0x7e,
		0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xc2, 0x01, 0x0f, 0x01, 0x01, 0x52, 0x91,
	}
	second = Frame{
		0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0xbc, 0x0a, 0x40, 0x40, 0x2c, 0x42, 0x06, 0x60,
		0x18, 0xee, 0xf3, 0xe0, 0x2e, 0x00, 0x00, 0x18, 0x01, 0x5a, 0x00, 0x00, 0xd7, 0x48,
	}
	unknown = Frame{
		0x54, 0x52, 0x43, 0x4b, 0x0a, 0x00, 0x00, 0x00, 0x23, 0xc1, 0xab, 0x00, 0x00, 0x00, 0x00, 0x00,
		0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0xff, 0xff, 0xff, 0xff, 0x00, 0x2a, 0x6a,
	}
	saturated = Frame{
		0x54, 0x52, 0x43, 0x4b, 0x0a, 0x00, 0x00, 0x00, 0x24, 0xc1, 0xab, 0x00, 0x00, 0x00, 0x00, 0x80,
		0x96, 0x98, 0x00, 0x00, 0x00, 0x00, 0x00, 0xfe, 0xff, 0x00, 0x00, 0x00, 0xa8, 0x0d,
	}
)

const publishedLine = `{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4}`

const secondLine = `{"now":1700000000.9,"hex":"400abc","lat":10.5,"lon":-20.25,"alt_baro":12000,"gs":280,"track":90}`

func TestCRCCheckValue(t *testing.T) {
	if g := crc16IBM3740([]byte("123456789")); g != 0x29B1 {
		t.Fatalf("crc16IBM3740(123456789)=%04x", g)
	}
}

func TestPublishedFrameBytes(t *testing.T) {
	fix, ok := ParseFix([]byte(publishedLine))
	if !ok {
		t.Fatal("parse")
	}
	if fix.UnixS != 1_700_000_000 || fix.ICAO != 0x40621D {
		t.Fatalf("unix=%d icao=%06x", fix.UnixS, fix.ICAO)
	}
	if fix.LatE7 != 522_572_021 || fix.LonE7 != 39_193_726 {
		t.Fatalf("lat=%d lon=%d", fix.LatE7, fix.LonE7)
	}
	if fix.AltFt != 38000 || fix.SpeedKt != 450 || fix.TrackDeg != 271 || fix.Emergency != None {
		t.Fatalf("%+v", fix)
	}
	frame := Pack(fix)
	if frame != published {
		t.Fatalf("got %x want %x", frame, published)
	}
	if frame[27] != 0 {
		t.Fatalf("emergency byte %d", frame[27])
	}
	if le16(frame[28], frame[29]) != 0x8173 {
		t.Fatalf("crc %04x", le16(frame[28], frame[29]))
	}
	back, ok := Unpack(frame[:])
	if !ok || back != fix {
		t.Fatalf("unpack %+v ok=%v", back, ok)
	}
	flipped := frame
	flipped[29] ^= 0x5A
	if _, ok := Unpack(flipped[:]); ok {
		t.Fatal("bad crc unpacked")
	}
}

func TestDocumentJSONIsNotAFix(t *testing.T) {
	if _, ok := ParseFix([]byte(`{"now":1700000000.9,"aircraft":[{"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375}]}`)); ok {
		t.Fatal("document parsed as a fix")
	}
}

func TestNowAndSeenPos(t *testing.T) {
	fresh, ok := ParseFix([]byte(`{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"seen_pos":0}`))
	if !ok || fresh.UnixS != 1_700_000_000 {
		t.Fatalf("%+v ok=%v", fresh, ok)
	}
	old, ok := ParseFix([]byte(`{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"seen_pos":100}`))
	if !ok || old.UnixS != 1_700_000_000 {
		t.Fatalf("seen_pos still a line: %+v ok=%v", old, ok)
	}
	rejects := []string{
		`{"hex":"40621d","lat":1,"lon":2}`,
		`{"now":-1,"hex":"40621d","lat":1,"lon":2}`,
		`{"now":1700000000,"hex":"40621d","lon":2}`,
		`{"now":1700000000,"hex":"40621d","lat":1}`,
		`{"now":"1700000000","hex":"40621d","lat":1,"lon":2}`,
		`{"now":1e20,"hex":"40621d","lat":1,"lon":2}`,
		`not json`,
		`{"now":1,"aircraft":[]}`,
		`{"now":30,"hex":"40621d","squawk":"7700","emergency":"general"}`,
	}
	for _, line := range rejects {
		if _, ok := ParseFix([]byte(line)); ok {
			t.Fatalf("accepted %s", line)
		}
	}
}

func TestSentinelsAndTrackWrap(t *testing.T) {
	u, ok := ParseFix([]byte(`{"now":10,"hex":"abc123","lat":0,"lon":0,"gs":-1,"track":-5}`))
	if !ok {
		t.Fatal("parse unknown")
	}
	if u.AltFt != AltUnknown || u.SpeedKt != SpeedUnknown || u.TrackDeg != TrackUnknown {
		t.Fatalf("%+v", u)
	}
	if Pack(u) != unknown {
		t.Fatalf("unknown frame %x", Pack(u))
	}
	s, ok := ParseFix([]byte(`{"now":10,"hex":"abc124","lat":0,"lon":1,"alt_baro":"ground","gs":65534.6,"track":360}`))
	if !ok {
		t.Fatal("parse saturated")
	}
	if s.AltFt != 0 || s.SpeedKt != 65534 || s.TrackDeg != 0 {
		t.Fatalf("%+v", s)
	}
	if Pack(s) != saturated {
		t.Fatalf("saturated frame %x", Pack(s))
	}
	g, ok := ParseFix([]byte(`{"now":10,"hex":"000001","lat":1,"lon":2,"alt_geom":3280.4}`))
	if !ok || g.AltFt != 3280 || g.SpeedKt != SpeedUnknown || g.TrackDeg != TrackUnknown {
		t.Fatalf("%+v ok=%v", g, ok)
	}
}

func TestICAOAndLat(t *testing.T) {
	tilde, ok := ParseFix([]byte(`{"now":1,"hex":"~40621d","lat":1,"lon":2}`))
	if !ok || tilde.ICAO != 0x40621D {
		t.Fatalf("%+v ok=%v", tilde, ok)
	}
	for _, line := range []string{
		`{"now":1,"hex":"40G621","lat":1,"lon":2}`,
		`{"now":1,"hex":"abcdefg","lat":1,"lon":2}`,
		`{"now":1,"hex":" abc","lat":1,"lon":2}`,
		`{"now":1,"hex":"~~40621d","lat":1,"lon":2}`,
		`{"now":1,"hex":"40621d","lat":91,"lon":2}`,
	} {
		if _, ok := ParseFix([]byte(line)); ok {
			t.Fatalf("accepted %s", line)
		}
	}
}

func TestEmergencyNamesAndSquawks(t *testing.T) {
	if General != 1 || Lifeguard != 2 || MinFuel != 3 || Nordo != 4 || Unlawful != 5 || Downed != 6 || Reserved != 7 {
		t.Fatalf("emergency codes")
	}
	cases := []struct {
		extra string
		want  Emergency
	}{
		{`"squawk":"7700"`, General},
		{`"squawk":"7600"`, Nordo},
		{`"squawk":"7500"`, Unlawful},
		{`"squawk":7700`, General},
		{`"squawk":7600`, Nordo},
		{`"squawk":7500`, Unlawful},
		{`"squawk":" 7700 "`, General},
		{`"squawk":"07700"`, None},
		{`"squawk":"77000"`, None},
		{`"squawk":"0770"`, None},
		{`"emergency":"general"`, General},
		{`"emergency":"lifeguard"`, Lifeguard},
		{`"emergency":"minfuel"`, MinFuel},
		{`"emergency":"nordo"`, Nordo},
		{`"emergency":"unlawful"`, Unlawful},
		{`"emergency":"downed"`, Downed},
		{`"emergency":"GENERAL"`, General},
		{`"emergency":"unlawful","squawk":"7700"`, Unlawful},
		{`"emergency":"none","squawk":"7700"`, General},
		{`"emergency":"","squawk":"7600"`, Nordo},
		{`"emergency":"none"`, None},
		{`"squawk":"1000"`, None},
		{`"emergency":"medical"`, None},
		{`"emergency":"medical","squawk":"7700"`, General},
		{`"emergency":"reserved"`, Reserved},
		{`"emergency":"RESERVED"`, Reserved},
		{`"emergency":"reserved","squawk":"1000"`, Reserved},
		{`"emergency":"reserved","squawk":"7700"`, General},
		{`"emergency":"reserved","squawk":"7600"`, Nordo},
		{`"emergency":"reserved","squawk":"7500"`, Unlawful},
	}
	for _, tc := range cases {
		line := `{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,` + tc.extra + `}`
		fix, ok := ParseFix([]byte(line))
		if !ok {
			t.Fatalf("parse %s", tc.extra)
		}
		if fix.Emergency != tc.want {
			t.Fatalf("%s: got %d want %d", tc.extra, fix.Emergency, tc.want)
		}
		frame := Pack(fix)
		if Emergency(frame[27]) != tc.want {
			t.Fatalf("%s: byte %d", tc.extra, frame[27])
		}
		back, ok := Unpack(frame[:])
		if !ok || back.Emergency != tc.want {
			t.Fatalf("%s unpack %+v ok=%v", tc.extra, back, ok)
		}
	}

	named, ok := ParseFix([]byte(`{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"emergency":"general"}`))
	if !ok {
		t.Fatal("named")
	}
	if Pack(named) != emergencyGeneral {
		t.Fatalf("named frame %x", Pack(named))
	}
	squawked, ok := ParseFix([]byte(`{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"squawk":"7700"}`))
	if !ok {
		t.Fatal("squawk")
	}
	if Pack(squawked) != emergencyGeneral {
		t.Fatalf("squawk frame %x", Pack(squawked))
	}

	bad := emergencyGeneral
	bad[27] = 8
	c := crc16IBM3740(bad[:28])
	bad[28] = byte(c)
	bad[29] = byte(c >> 8)
	if _, ok := Unpack(bad[:]); ok {
		t.Fatal("emergency 8 unpacked")
	}
	reserved := emergencyGeneral
	reserved[27] = 7
	c = crc16IBM3740(reserved[:28])
	reserved[28] = byte(c)
	reserved[29] = byte(c >> 8)
	got, ok := Unpack(reserved[:])
	if !ok || got.Emergency != Reserved {
		t.Fatalf("reserved %+v ok=%v", got, ok)
	}
}

func TestCheckFrames(t *testing.T) {
	frames := CheckFrames()
	if frameICAO(frames[0]) != 0x400ABC || frames[0][27] != 0 {
		t.Fatalf("first %x", frames[0])
	}
	if frames[1] != published {
		t.Fatalf("check[1] is the published layout, got %x", frames[1])
	}
	if frameICAO(frames[2]) != 0x4CA123 || frames[2][27] != byte(General) {
		t.Fatalf("third %x", frames[2])
	}
	for _, frame := range frames {
		if le32(frame[4], frame[5], frame[6], frame[7]) != 1_700_000_000 {
			t.Fatalf("unix %x", frame)
		}
		if _, ok := Unpack(frame[:]); !ok {
			t.Fatalf("crc %x", frame)
		}
	}
}

func TestRadioPayloadBytes(t *testing.T) {
	want := []byte{0x1D, 0x62, 0x40, 0xF5, 0xD0, 0x25, 0x1F, 0x7E, 0x0C, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xC2, 0x01, 0x0F, 0x01, 0x00, 0x00, 0xF1, 0x53, 0x65}
	var got []byte
	got = append(got, published[8:28]...)
	got = append(got, published[4:8]...)
	if !bytes.Equal(got, want) {
		t.Fatalf("radio payload %x", got)
	}
}

func le16(a, b byte) uint16 { return uint16(a) | uint16(b)<<8 }

func le32(a, b, c, d byte) uint32 {
	return uint32(a) | uint32(b)<<8 | uint32(c)<<16 | uint32(d)<<24
}
