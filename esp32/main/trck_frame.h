#ifndef TRCK_FRAME_H
#define TRCK_FRAME_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define TRCK_FRAME_LEN 30
#define TRCK_ALT_UNKNOWN INT32_MIN
#define TRCK_SPEED_UNKNOWN 0xFFFFu
#define TRCK_TRACK_UNKNOWN 0xFFFFu

/* One decoded aircraft. Fields are copied out of the UART bytes. */
typedef struct {
    uint32_t unix_s;
    uint32_t icao;
    int32_t lat_e7;
    int32_t lon_e7;
    int32_t alt_ft;
    uint16_t gs_kt;
    uint16_t track_deg;
    uint8_t emergency;
} trck_fix_t;

typedef struct {
    uint8_t buf[TRCK_FRAME_LEN];
    size_t n;
} trck_scan_t;

/* CRC-16/IBM-3740 (CRC-16/CCITT-FALSE). crc16_ccitt_false("123456789") == 0x29B1. */
uint16_t trck_crc16(const uint8_t *data, size_t len);

/* Copy each field from `bytes`. Does not overlay a packed struct. */
bool trck_unpack(const uint8_t *bytes, size_t len, trck_fix_t *out);

void trck_scan_init(trck_scan_t *scan);
bool trck_scan_feed(trck_scan_t *scan, uint8_t byte, trck_fix_t *out);

#endif
