#!/usr/bin/env bash
# Run the Writer::enqueue model checker.
# TLC 1.7.4 (tla2tools.jar). Java 21 is enough.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
TOOLS="$DIR/.tools"
JAR="$TOOLS/tla2tools.jar"
PIN_VER="1.7.4"
PIN_SHA1="bee4a54f3ee3d4afc347c3240ec2d9e93b075104"
URL="https://github.com/tlaplus/tlaplus/releases/download/v${PIN_VER}/tla2tools.jar"

mkdir -p "$TOOLS"
if [[ ! -f "$JAR" ]]; then
  curl -fsSL -o "$JAR" "$URL"
fi
got="$(sha1sum "$JAR" | awk '{print $1}')"
if [[ "$got" != "$PIN_SHA1" ]]; then
  echo "tla2tools.jar sha1 ${got}, expected ${PIN_SHA1}" >&2
  exit 1
fi

cd "$DIR"
exec java -XX:+UseParallelGC -jar "$JAR" -workers 1 -config WriterQueue.cfg WriterQueue.tla
