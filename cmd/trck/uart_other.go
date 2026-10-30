//go:build !linux

package main

import (
	"fmt"
	"io"
)

func openUART(path string, baud uint) (io.WriteCloser, error) {
	return nil, fmt.Errorf("cannot open %s: uart devices need linux", path)
}
