#!/bin/sh
set -eu

zigver=0.17.1
zigdir="${HOME}/zig"
bindir="${HOME}/.local/bin"
here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
zig_cc="$here/zig-cc"

mkdir -p "$zigdir" "$bindir"
if [ ! -x "$zigdir/zig" ]; then
  curl -fsSL -o /tmp/zig.tar.xz "https://ziglang.org/download/${zigver}/zig-x86_64-linux-${zigver}.tar.xz"
  tar -xJf /tmp/zig.tar.xz -C "$zigdir" --strip-components=1
fi

printf '%s\n' '#!/bin/sh' "exec \"$zig_cc\" arm-linux-musleabihf \"\$@\"" >"$bindir/arm-linux-musleabihf-gcc"
printf '%s\n' '#!/bin/sh' "exec \"$zigdir/zig\" ar \"\$@\"" >"$bindir/arm-linux-musleabihf-ar"
chmod +x \
  "$zig_cc" \
  "$bindir/arm-linux-musleabihf-gcc" \
  "$bindir/arm-linux-musleabihf-ar"

if [ -n "${GITHUB_PATH:-}" ]; then
  echo "$zigdir" >>"$GITHUB_PATH"
  echo "$bindir" >>"$GITHUB_PATH"
fi
