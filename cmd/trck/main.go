package main

import (
	"bufio"
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"net"
	"os"
	"os/signal"
	"syscall"
	"time"
)

const (
	defaultUART   = "/dev/serial0"
	defaultBaud   = 115200
	maxLine       = 8192
	connectWait   = 5 * time.Second
	retryMin      = time.Second
	retryMax      = 30 * time.Second
	checkInterval = time.Second
)

func main() {
	os.Exit(trckMain(os.Args[1:]))
}

func trckMain(args []string) int {
	fs := flag.NewFlagSet("trck", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	netJSON := fs.String("net-json", "", "Host and port of readsb --net-json-port. Required unless --check.")
	uart := fs.String("uart", defaultUART, "UART device, or - for stdout.")
	baud := fs.Uint("baud", defaultBaud, "UART baud rate.")
	check := fs.Bool("check", false, "Send the known frames instead of reading readsb, to check the UART.")
	fs.Usage = func() {
		fmt.Fprintf(fs.Output(), "Send each readsb position to a UART as it arrives.\n\n")
		fs.PrintDefaults()
	}
	if err := fs.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return 0
		}
		return 2
	}
	if *check == (*netJSON != "") {
		fmt.Fprintln(os.Stderr, "pass --net-json HOST:PORT, or --check")
		return 1
	}
	if !*check {
		if _, _, err := net.SplitHostPort(*netJSON); err != nil {
			fmt.Fprintf(os.Stderr, "net-json %s: expected host:port\n", *netJSON)
			return 1
		}
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	sink, err := openSink(*uart, *baud)
	if err != nil {
		fmt.Fprintf(os.Stderr, "cannot open %s: %v\n", *uart, err)
		return 1
	}
	defer sink.Close()

	if *check {
		fmt.Fprintf(os.Stderr, "check uart=%s baud=%d\n", *uart, *baud)
		if err := runCheck(ctx, sink); err != nil {
			return writeExit(err)
		}
		fmt.Fprintln(os.Stderr, "stopped")
		return 0
	}

	fmt.Fprintf(os.Stderr, "net-json=%s uart=%s baud=%d\n", *netJSON, *uart, *baud)
	if err := runPort(ctx, *netJSON, sink); err != nil {
		return writeExit(err)
	}
	fmt.Fprintln(os.Stderr, "stopped")
	return 0
}

func errUnsupportedBaud(baud uint) error {
	return fmt.Errorf("unsupported baud %d", baud)
}

type stdoutSink struct{}

func (stdoutSink) Write(p []byte) (int, error) { return os.Stdout.Write(p) }
func (stdoutSink) Close() error                { return nil }

func openSink(path string, baud uint) (io.WriteCloser, error) {
	if path == "-" {
		return stdoutSink{}, nil
	}
	return openUART(path, baud)
}

func runCheck(ctx context.Context, w io.Writer) error {
	frames := CheckFrames()
	fixes := CheckFixes()
	writeBurst := func() error {
		for i, frame := range frames {
			logFix(fixes[i])
			if err := writeFull(w, frame[:]); err != nil {
				return err
			}
		}
		return nil
	}
	if err := writeBurst(); err != nil {
		return err
	}
	ticker := time.NewTicker(checkInterval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return nil
		case <-ticker.C:
			if err := writeBurst(); err != nil {
				return err
			}
		}
	}
}

func runPort(ctx context.Context, addr string, w io.Writer) error {
	delay := retryMin
	for ctx.Err() == nil {
		conn, err := dial(ctx, addr)
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			fmt.Fprintf(os.Stderr, "net-json %s: %v\n", addr, err)
			if !sleepCtx(ctx, delay) {
				return nil
			}
			delay = grow(delay)
			continue
		}
		fmt.Fprintf(os.Stderr, "net-json %s connected\n", addr)
		err = pump(ctx, conn, w)
		conn.Close()
		if ctx.Err() != nil {
			return nil
		}
		if isUARTWrite(err) {
			return err
		}
		fmt.Fprintf(os.Stderr, "net-json %s disconnected\n", addr)
		delay = retryMin
		if !sleepCtx(ctx, delay) {
			return nil
		}
		delay = grow(delay)
	}
	return nil
}

func dial(ctx context.Context, addr string) (net.Conn, error) {
	d := net.Dialer{Timeout: connectWait, KeepAlive: 30 * time.Second}
	conn, err := d.DialContext(ctx, "tcp", addr)
	if err != nil {
		return nil, err
	}
	if tc, ok := conn.(*net.TCPConn); ok {
		_ = tc.SetKeepAlive(true)
		_ = tc.SetKeepAlivePeriod(30 * time.Second)
	}
	return conn, nil
}

type uartError struct{ error }

func isUARTWrite(err error) bool {
	var u uartError
	return errors.As(err, &u)
}

func pump(ctx context.Context, conn net.Conn, w io.Writer) error {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	go func() {
		<-ctx.Done()
		_ = conn.SetDeadline(time.Now())
	}()

	q := NewQueue()
	errc := make(chan error, 2)

	go func() {
		err := readLoop(ctx, conn, q)
		q.Close()
		if err != nil && ctx.Err() == nil {
			errc <- err
			return
		}
		errc <- nil
	}()
	go func() {
		err := writeLoop(ctx, q, w)
		cancel()
		if err != nil {
			errc <- uartError{err}
			return
		}
		errc <- nil
	}()

	var uartErr error
	for i := 0; i < 2; i++ {
		if err := <-errc; err != nil {
			if isUARTWrite(err) {
				uartErr = err
			}
		}
	}
	return uartErr
}

func readLoop(ctx context.Context, r io.Reader, q *Queue) error {
	sc := bufio.NewScanner(r)
	sc.Buffer(make([]byte, 0, maxLine), maxLine)
	for sc.Scan() {
		if ctx.Err() != nil {
			return nil
		}
		fix, ok := ParseFix(sc.Bytes())
		if !ok {
			continue
		}
		frame := Pack(fix)
		if dropped, did := q.Push(frame); did {
			logDrop(dropped)
		}
		logFix(fix)
	}
	if err := sc.Err(); err != nil && ctx.Err() == nil {
		return err
	}
	return nil
}

func writeLoop(ctx context.Context, q *Queue, w io.Writer) error {
	for {
		frame, ok := q.PopWait(ctx)
		if !ok {
			return nil
		}
		if err := writeFull(w, frame[:]); err != nil {
			return err
		}
	}
}

func writeFull(w io.Writer, p []byte) error {
	for len(p) > 0 {
		n, err := w.Write(p)
		if n > 0 {
			p = p[n:]
		}
		if err != nil {
			return err
		}
		if n == 0 {
			return io.ErrShortWrite
		}
	}
	return nil
}

func writeExit(err error) int {
	if err == nil || errors.Is(err, context.Canceled) {
		fmt.Fprintln(os.Stderr, "stopped")
		return 0
	}
	fmt.Fprintf(os.Stderr, "uart write failed: %v\n", err)
	return 1
}

func grow(d time.Duration) time.Duration {
	next := d * 2
	if next > retryMax {
		return retryMax
	}
	return next
}

func sleepCtx(ctx context.Context, d time.Duration) bool {
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-t.C:
		return true
	}
}

func logFix(fix Fix) {
	fmt.Fprintf(os.Stderr, "fix t=%d icao=%06x\n", fix.UnixS, fix.ICAO)
}

func logDrop(frame Frame) {
	fmt.Fprintf(os.Stderr, "dropped frame icao=%06x emergency=%d\n", frameICAO(frame), frame[27])
}
