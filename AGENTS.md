# trck

## Go

In this order:

1. `test -z "$(gofmt -l .)"`
2. `go vet ./...`
3. `go test ./...`

The Pi binaries:

`CGO_ENABLED=0 GOOS=linux GOARCH=arm GOARM=6 go build -o trck-arm ./cmd/trck`

`CGO_ENABLED=0 GOOS=linux GOARCH=arm64 go build -o trck-aarch64 ./cmd/trck`

Release assets are `trck-arm` and `trck-aarch64`.
