# Editing this repo

`trck` on a Pi Zero W reads readsb `--net-json-port` and writes one 30-byte frame per accepted line. The ESP32 firmware is not in this repo.

The field layout is in `README.md`, `docs/esp32-alert.md`, and `pack_frame`.

## Encoder

`FRAME_LEN` is 30. `pack_frame` is the only encoder. You do not add a second encoder, a sample text file, or a checked-in frame blob.

You do not bring back `aircraft.json` or the file snapshot parser. `fix_from_json_line` takes one position object. You do not read positions out of an `aircraft` array.

## Live read

The live reader does not block on the UART write. Finished frames wait in a queue of 64. The frame being written is not one of the 64.

When the queue is full and a waiting frame has emergency byte 0, you drop the oldest such frame in the queue, then you enqueue the new frame. When the queue is full and no waiting frame has emergency byte 0, you drop a new frame whose emergency byte is 0. When the queue is full and no waiting frame has emergency byte 0, you drop the oldest waiting frame to enqueue a new emergency.

## Squawk

You trim the squawk text, then match `7700`, `7600`, and `7500` only as those four characters. `07700` is not `7700`. You match a JSON number by its decimal digits.

## Check

`--check` repeats `check_positions` through `pack_frame`. `--check` and `--net-json` conflict. The host test `check_matches_check_frames` runs `trck --check --uart -` and compares the first pass to `check_frames`. The process stays running after that first pass.

## Release

The release file is static `arm-unknown-linux-musleabihf`, named `trck-pi-zero-w`, built by Nix. You do not add a second board target or a QEMU suite.

## Tests

You prove a change with `cargo test --locked`. `nix develop` is the shell. `nix build` and `nix flake check` are the release checks.
