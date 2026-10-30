package main

import (
	"bytes"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
	"testing"
	"time"
)

var trckBin string

func TestMain(m *testing.M) {
	os.Exit(runTests(m))
}

func runTests(m *testing.M) int {
	dir, err := os.MkdirTemp("", "trckbin")
	if err != nil {
		return 1
	}
	defer os.RemoveAll(dir)
	trckBin = filepath.Join(dir, "trck")
	build := exec.Command("go", "build", "-o", trckBin, ".")
	build.Stderr = os.Stderr
	if err := build.Run(); err != nil {
		return 1
	}
	return m.Run()
}

func startNet(t *testing.T, addr string) *syncBuf {
	t.Helper()
	return startArgs(t, "--net-json", addr, "--uart", "-")
}

func startArgs(t *testing.T, args ...string) *syncBuf {
	t.Helper()
	cmd := exec.Command(trckBin, args...)
	cmd.Stderr = io.Discard
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	done := make(chan struct{})
	go func() {
		_ = cmd.Wait()
		close(done)
	}()
	t.Cleanup(func() {
		_ = cmd.Process.Kill()
		<-done
	})
	t.Cleanup(func() {
		select {
		case <-done:
			if !t.Failed() {
				t.Error("trck exited")
			}
		default:
		}
	})
	return collect(t, stdout)
}

func collect(t *testing.T, r io.Reader) *syncBuf {
	t.Helper()
	buf := &syncBuf{}
	go func() {
		tmp := make([]byte, 256)
		for {
			n, err := r.Read(tmp)
			if n > 0 {
				buf.append(tmp[:n])
			}
			if err != nil {
				return
			}
		}
	}()
	return buf
}

type syncBuf struct {
	mu   sync.Mutex
	data []byte
}

func (b *syncBuf) append(p []byte) {
	b.mu.Lock()
	b.data = append(b.data, p...)
	b.mu.Unlock()
}

func (b *syncBuf) bytes() []byte {
	b.mu.Lock()
	defer b.mu.Unlock()
	out := make([]byte, len(b.data))
	copy(out, b.data)
	return out
}

func waitLen(t *testing.T, buf *syncBuf, n int, d time.Duration) []byte {
	t.Helper()
	deadline := time.Now().Add(d)
	for {
		got := buf.bytes()
		if len(got) >= n || time.Now().After(deadline) {
			return got
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func listen(t *testing.T) (net.Listener, string) {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { ln.Close() })
	return ln, ln.Addr().String()
}

func TestNothingWrittenUntilALineArrives(t *testing.T) {
	ln, addr := listen(t)
	ready := make(chan struct{})
	goHold := make(chan struct{})
	go serve(ln, func(c net.Conn) bool {
		close(ready)
		<-goHold
		_, _ = c.Write([]byte(publishedLine + "\n"))
		time.Sleep(2 * time.Second)
		return false
	})
	buf := startNet(t, addr)
	select {
	case <-ready:
	case <-time.After(2 * time.Second):
		t.Fatal("connect")
	}
	time.Sleep(200 * time.Millisecond)
	if got := buf.bytes(); len(got) != 0 {
		t.Fatalf("bytes before a line: %x", got)
	}
	close(goHold)
	got := waitLen(t, buf, FrameSize, 2*time.Second)
	if !bytes.Equal(got, published[:]) {
		t.Fatalf("got %x", got)
	}
}

func TestOneFramePerValidLine(t *testing.T) {
	ln, addr := listen(t)
	payload := "not json\n" +
		`{"now":1}` + "\n" +
		`{"now":-1,"hex":"abc123","lat":1,"lon":2}` + "\n" +
		`{"hex":"abc123","lat":1,"lon":2}` + "\n" +
		`{"now":1.5,"hex":"abc123","lon":2}` + "\n" +
		`{"now":"1700000000","hex":"abc123","lat":1,"lon":2}` + "\n" +
		publishedLine + "\n" +
		`{"now":1700000000,"aircraft":[]}` + "\n" +
		secondLine + "\n"
	go serve(ln, func(c net.Conn) bool {
		_, _ = c.Write([]byte(payload))
		time.Sleep(2 * time.Second)
		return false
	})
	buf := startNet(t, addr)
	got := waitLen(t, buf, FrameSize*2, 2*time.Second)
	if len(got) != FrameSize*2 {
		t.Fatalf("got %d bytes %x", len(got), got)
	}
	if !bytes.Equal(got[:FrameSize], published[:]) || !bytes.Equal(got[FrameSize:], second[:]) {
		t.Fatalf("got %x", got)
	}
	time.Sleep(200 * time.Millisecond)
	if len(buf.bytes()) != FrameSize*2 {
		t.Fatalf("extra bytes %d", len(buf.bytes()))
	}
}

func TestSameICAOTwice(t *testing.T) {
	ln, addr := listen(t)
	go serve(ln, func(c net.Conn) bool {
		_, _ = c.Write([]byte(publishedLine + "\n" + publishedLine + "\n"))
		time.Sleep(2 * time.Second)
		return false
	})
	got := waitLen(t, startNet(t, addr), FrameSize*2, 2*time.Second)
	want := append(published[:], published[:]...)
	if !bytes.Equal(got, want) {
		t.Fatalf("got %x", got)
	}
}

func TestSplitLineIsOneFrame(t *testing.T) {
	ln, addr := listen(t)
	ready := make(chan struct{})
	goHold := make(chan struct{})
	line := []byte(publishedLine)
	tail := append(append([]byte{}, line[len(line)/2:]...), '\n')
	go serve(ln, func(c net.Conn) bool {
		_, _ = c.Write(line[:len(line)/2])
		close(ready)
		<-goHold
		_, _ = c.Write(tail)
		time.Sleep(2 * time.Second)
		return false
	})
	buf := startNet(t, addr)
	select {
	case <-ready:
	case <-time.After(2 * time.Second):
		t.Fatal("connect")
	}
	time.Sleep(150 * time.Millisecond)
	if got := buf.bytes(); len(got) != 0 {
		t.Fatalf("partial line wrote a frame: %x", got)
	}
	close(goHold)
	got := waitLen(t, buf, FrameSize, 2*time.Second)
	if !bytes.Equal(got, published[:]) {
		t.Fatalf("got %x", got)
	}
}

func TestEmergencyOnTheWire(t *testing.T) {
	ln, addr := listen(t)
	unlawful := `{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"emergency":"unlawful","squawk":"7700"}`
	go serve(ln, func(c net.Conn) bool {
		_, _ = c.Write([]byte(`{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"squawk":"7700"}` + "\n" + unlawful + "\n"))
		time.Sleep(2 * time.Second)
		return false
	})
	got := waitLen(t, startNet(t, addr), FrameSize*2, 2*time.Second)
	if !bytes.Equal(got[:FrameSize], emergencyGeneral[:]) {
		t.Fatalf("first %x", got[:FrameSize])
	}
	secondFix, ok := Unpack(got[FrameSize:])
	if !ok || secondFix.Emergency != Unlawful {
		t.Fatalf("second %+v ok=%v", secondFix, ok)
	}
	if got[FrameSize+27] != byte(Unlawful) {
		t.Fatalf("byte %d", got[FrameSize+27])
	}
}

func TestDeadSocketWritesNothingUntilNextLine(t *testing.T) {
	ln, addr := listen(t)
	first := true
	go serve(ln, func(c net.Conn) bool {
		if first {
			first = false
			return true
		}
		_, _ = c.Write([]byte(publishedLine + "\n"))
		time.Sleep(2 * time.Second)
		return false
	})
	buf := startNet(t, addr)
	time.Sleep(400 * time.Millisecond)
	if got := buf.bytes(); len(got) != 0 {
		t.Fatalf("dead socket wrote %x", got)
	}
	got := waitLen(t, buf, FrameSize, 4*time.Second)
	if !bytes.Equal(got, published[:]) {
		t.Fatalf("got %x", got)
	}
}

func TestRefusedPortWritesNothing(t *testing.T) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	addr := ln.Addr().String()
	ln.Close()
	buf := startNet(t, addr)
	time.Sleep(700 * time.Millisecond)
	if got := buf.bytes(); len(got) != 0 {
		t.Fatalf("refused wrote %x", got)
	}
}

func TestCheckAndNetJSONIsAnError(t *testing.T) {
	cmd := exec.Command(trckBin, "--check", "--net-json", "127.0.0.1:9", "--uart", "-")
	cmd.Stderr = io.Discard
	out, err := cmd.Output()
	if err == nil {
		t.Fatal("accepted both flags")
	}
	if len(out) != 0 {
		t.Fatalf("wrote %x", out)
	}
}

func TestCheckMatchesCheckFrames(t *testing.T) {
	frames := CheckFrames()
	var want []byte
	for _, f := range frames {
		want = append(want, f[:]...)
	}
	buf := startArgs(t, "--check", "--uart", "-")
	got := waitLen(t, buf, len(want), 2*time.Second)
	if len(got) < len(want) {
		t.Fatalf("short output %d", len(got))
	}
	if !bytes.Equal(got[:len(want)], want) {
		t.Fatalf("got %x want %x", got[:len(want)], want)
	}
}

func serve(ln net.Listener, fn func(net.Conn) bool) {
	for {
		c, err := ln.Accept()
		if err != nil {
			return
		}
		next := fn(c)
		if !next {
			time.Sleep(2 * time.Second)
			c.Close()
			return
		}
		c.Close()
	}
}
