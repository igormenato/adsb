# trck

## Nix

`nix develop` opens the shell.

`nix flake check` builds the Pi Zero binary and runs the host tests.

`nix build` writes `result/bin/trck-pi-zero-w`.

## Zig

In that shell, in this order:

1. `zig fmt --check src`
2. `zig test src/main.zig -target native-linux-musl -lc`
3. `nix build`
