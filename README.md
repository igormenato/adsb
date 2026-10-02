# adsb

## readsb

```
readsb --device-type rtlsdr --net --write-json /run/readsb
```

`--write-json` writes `aircraft.json` in that directory.

## UART

Disable the serial login shell and leave the UART enabled (`raspi-config` → Interface Options → Serial Port), or the console will share the port.

## Run

```
cargo build --release
./target/release/adsb-uart-sender
```
