# trck

## Nix

`nix develop` opens the shell.

`nix flake check` builds the Pi Zero binary and runs the host tests.

`nix build` writes `result/bin/trck-pi-zero-w`.

## Zig

In that shell, in this order:

1. `zig fmt --check src build.zig`
2. `zig build test -Dtarget=native-linux-musl`
3. `nix build`
