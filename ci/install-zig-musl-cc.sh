#!/bin/sh
set -eu

zigver=0.17.1
zigdir="${HOME}/zig"
bindir="${HOME}/.local/bin"
here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
zig_cc="$here/zig-cc"

mkdir -p "$zigdir" "$bindir"

zigbin=$(command -v zig || true)
if [ -z "$zigbin" ] && [ -x "$zigdir/zig" ]; then
  zigbin="$zigdir/zig"
fi
if [ -z "$zigbin" ]; then
  curl -fsSL -o /tmp/zig.tar.xz "https://ziglang.org/download/${zigver}/zig-x86_64-linux-${zigver}.tar.xz"
  tar -xJf /tmp/zig.tar.xz -C "$zigdir" --strip-components=1
  zigbin="$zigdir/zig"
fi

printf '%s\n' '#!/bin/sh' "exec \"$zig_cc\" arm-linux-musleabihf \"\$@\"" >"$bindir/arm-linux-musleabihf-gcc"
printf '%s\n' '#!/bin/sh' "exec \"$zigbin\" ar \"\$@\"" >"$bindir/arm-linux-musleabihf-ar"
printf '%s\n' '#!/bin/sh' "exec \"$zig_cc\" aarch64-linux-musl \"\$@\"" >"$bindir/aarch64-linux-musl-gcc"
printf '%s\n' '#!/bin/sh' "exec \"$zigbin\" ar \"\$@\"" >"$bindir/aarch64-linux-musl-ar"
chmod +x \
  "$zig_cc" \
  "$bindir/arm-linux-musleabihf-gcc" \
  "$bindir/arm-linux-musleabihf-ar" \
  "$bindir/aarch64-linux-musl-gcc" \
  "$bindir/aarch64-linux-musl-ar"

if [ -n "${GITHUB_PATH:-}" ]; then
  echo "$bindir" >>"$GITHUB_PATH"
fi
