#!/bin/sh
# Fail if the Arduino sketch and the PlatformIO copy differ.
set -eu
root=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
diff -u "$root/trck_uart/trck_uart.ino" "$root/pio/src/trck_uart.ino"
