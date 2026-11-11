#!/bin/sh
set -eu

zigver=0.17.1
zigdir="${HOME}/zig"
bindir="${HOME}/.local/bin"

mkdir -p "$zigdir" "$bindir"
if [ ! -x "$zigdir/zig" ]; then
  curl -fsSL -o /tmp/zig.tar.xz "https://ziglang.org/download/${zigver}/zig-x86_64-linux-${zigver}.tar.xz"
  tar -xJf /tmp/zig.tar.xz -C "$zigdir" --strip-components=1
fi

printf '%s\n' '#!/bin/sh' 'exec zig cc -target arm-linux-musleabihf "$@"' >"$bindir/arm-linux-musleabihf-gcc"
printf '%s\n' '#!/bin/sh' 'exec zig ar "$@"' >"$bindir/arm-linux-musleabihf-ar"
printf '%s\n' '#!/bin/sh' 'exec zig cc -target aarch64-linux-musl "$@"' >"$bindir/aarch64-linux-musl-gcc"
printf '%s\n' '#!/bin/sh' 'exec zig ar "$@"' >"$bindir/aarch64-linux-musl-ar"
chmod +x \
  "$bindir/arm-linux-musleabihf-gcc" \
  "$bindir/arm-linux-musleabihf-ar" \
  "$bindir/aarch64-linux-musl-gcc" \
  "$bindir/aarch64-linux-musl-ar"

if [ -n "${GITHUB_PATH:-}" ]; then
  echo "$zigdir" >>"$GITHUB_PATH"
  echo "$bindir" >>"$GITHUB_PATH"
fi
