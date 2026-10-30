# trck

`trck` writes one UART frame for each position readsb accepts on `--net-json-port`.

## Build

Run the host checks in this order.

1. `test -z "$(gofmt -l .)"`
2. `go vet ./...`
3. `go test ./...`

Build the Pi binaries.

```
CGO_ENABLED=0 GOOS=linux GOARCH=arm GOARM=6 go build -o trck-arm ./cmd/trck
CGO_ENABLED=0 GOOS=linux GOARCH=arm64 go build -o trck-aarch64 ./cmd/trck
```

`trck-arm` is linux/arm with GOARM=6, for the Zero W. `trck-aarch64` is linux/arm64, for the Pi 3B on a 64-bit OS.

## Run

Disable the serial login shell and leave the UART enabled. The console shares the port if you do not.

```
readsb --net-json-port 30005
./trck --net-json 127.0.0.1:30005
```

`30005` is an example. Pass the same host and port readsb is listening on.

## Flags

`--net-json HOST:PORT` is the live input. Required unless `--check`.

`--uart` defaults to `/dev/serial0`. Pass `--uart -` to write frames to stdout.

`--baud` defaults to 115200.

`--check` sends known frames instead of reading readsb, to check the UART. It repeats those frames every second.

## Frame

The layout is in [ESP32 satellite telemetry](docs/esp32-alert.md).
