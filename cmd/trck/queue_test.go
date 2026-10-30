package main

import "testing"

func frameFor(icao uint32, emergency bool) Frame {
	line := `{"now":1700000000,"hex":"` + hex6(icao) + `","lat":1,"lon":2`
	if emergency {
		line += `,"squawk":"7700"`
	}
	line += `}`
	fix, ok := ParseFix([]byte(line))
	if !ok {
		panic(line)
	}
	if (fix.Emergency != None) != emergency {
		panic(line)
	}
	return Pack(fix)
}

func hex6(n uint32) string {
	const digits = "0123456789abcdef"
	var b [6]byte
	for i := 5; i >= 0; i-- {
		b[i] = digits[n&0xf]
		n >>= 4
	}
	return string(b[:])
}

func TestEmergencySurvivesFullQueue(t *testing.T) {
	q := NewQueue()
	for icao := uint32(0); icao < queueCap; icao++ {
		q.Push(frameFor(icao, false))
	}
	q.Push(frameFor(0x7700, true))
	got := q.snapshot()
	if len(got) != queueCap {
		t.Fatalf("len %d", len(got))
	}
	last := got[len(got)-1]
	if last[27] != byte(General) || frameICAO(last) != 0x7700 {
		t.Fatalf("kept %x", last)
	}
	if le32(last[4], last[5], last[6], last[7]) != 1_700_000_000 {
		t.Fatalf("unix %x", last)
	}
	var saw0, saw1 bool
	for _, f := range got {
		if frameICAO(f) == 0 {
			saw0 = true
		}
		if frameICAO(f) == 1 {
			saw1 = true
		}
	}
	if saw0 {
		t.Fatal("ordinary icao 0 should have been dropped")
	}
	if !saw1 {
		t.Fatal("icao 1 should remain")
	}

	q = NewQueue()
	for icao := uint32(1); icao <= queueCap; icao++ {
		q.Push(frameFor(icao, true))
	}
	q.Push(frameFor(0x100, false))
	got = q.snapshot()
	if len(got) != queueCap {
		t.Fatalf("len %d", len(got))
	}
	for _, f := range got {
		if f[27] == 0 {
			t.Fatalf("ordinary kept %x", f)
		}
		if frameICAO(f) == 0x100 {
			t.Fatal("ordinary 0x100 replaced an emergency")
		}
	}
}
