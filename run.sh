#!/bin/sh
set -eu
cd "$(dirname "$0")"

if [ -x target/aarch64-unknown-linux-gnu/release/adsb-uart-sender ]; then
  bin=target/aarch64-unknown-linux-gnu/release/adsb-uart-sender
elif [ -x target/release/adsb-uart-sender ]; then
  bin=target/release/adsb-uart-sender
elif [ -x ./adsb-uart-sender ]; then
  bin=./adsb-uart-sender
else
  echo "adsb-uart-sender binary not found. Build it on another machine with: cargo build --release --target aarch64-unknown-linux-gnu" >&2
  exit 1
fi

exec "$bin" "$@"
