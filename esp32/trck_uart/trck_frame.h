#ifndef TRCK_FRAME_H
#define TRCK_FRAME_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#define TRCK_FRAME_LEN 34
#define TRCK_BODY_LEN 32
#define TRCK_CALLSIGN_LEN 8
#define TRCK_ALT_UNKNOWN INT32_MIN
#define TRCK_SPEED_UNKNOWN ((uint16_t)0xFFFF)
#define TRCK_TRACK_UNKNOWN ((uint16_t)0xFFFF)

typedef struct {
    uint32_t icao;
    uint8_t callsign[TRCK_CALLSIGN_LEN];
    int32_t lat_e7;
    int32_t lon_e7;
    int32_t alt_ft;
    uint16_t gs_kt;
    uint16_t track_deg;
    uint8_t emergency;
} TrckFix;

typedef struct {
    uint8_t buf[TRCK_FRAME_LEN];
    size_t n;
    unsigned long crc_fail;
    unsigned long resync;
} TrckScan;

static inline uint16_t trck_rd_u16(const uint8_t *p) {
    return (uint16_t)((uint16_t)p[0] | ((uint16_t)p[1] << 8));
}

static inline uint32_t trck_rd_u32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static inline int32_t trck_rd_i32(const uint8_t *p) {
    return (int32_t)trck_rd_u32(p);
}

static inline uint16_t trck_crc16(const uint8_t *data, size_t len) {
    uint16_t crc = 0xFFFFu;
    for (size_t i = 0; i < len; i++) {
        crc ^= (uint16_t)data[i] << 8;
        for (int bit = 0; bit < 8; bit++) {
            if ((crc & 0x8000u) != 0) {
                crc = (uint16_t)((crc << 1) ^ 0x1021u);
            } else {
                crc = (uint16_t)(crc << 1);
            }
        }
    }
    return crc;
}

static inline bool trck_unpack(const uint8_t *bytes, size_t len, TrckFix *out) {
    if (bytes == NULL || out == NULL || len != (size_t)TRCK_FRAME_LEN) {
        return false;
    }
    if (memcmp(bytes, "TRCK", 4) != 0) {
        return false;
    }
    if (trck_crc16(bytes, TRCK_BODY_LEN) != trck_rd_u16(bytes + TRCK_BODY_LEN)) {
        return false;
    }
    if (bytes[31] > 7) {
        return false;
    }
    out->icao = (uint32_t)bytes[4] | ((uint32_t)bytes[5] << 8) | ((uint32_t)bytes[6] << 16);
    memcpy(out->callsign, bytes + 7, TRCK_CALLSIGN_LEN);
    out->lat_e7 = trck_rd_i32(bytes + 15);
    out->lon_e7 = trck_rd_i32(bytes + 19);
    out->alt_ft = trck_rd_i32(bytes + 23);
    out->gs_kt = trck_rd_u16(bytes + 27);
    out->track_deg = trck_rd_u16(bytes + 29);
    out->emergency = bytes[31];
    return true;
}

static inline void trck_scan_init(TrckScan *scan) {
    scan->n = 0;
    scan->crc_fail = 0;
    scan->resync = 0;
}

static inline bool trck_magic_prefix(const uint8_t *buf, size_t n) {
    size_t m = n < 4 ? n : 4;
    return memcmp(buf, "TRCK", m) == 0;
}

static inline void trck_drop_byte(TrckScan *scan) {
    if (scan->n == 0) {
        return;
    }
    memmove(scan->buf, scan->buf + 1, scan->n - 1);
    scan->n--;
    scan->resync++;
}

static inline bool trck_scan_feed(TrckScan *scan, uint8_t byte, TrckFix *out) {
    if (scan->n >= (size_t)TRCK_FRAME_LEN) {
        trck_drop_byte(scan);
    }
    scan->buf[scan->n++] = byte;
    for (;;) {
        while (scan->n > 0 && !trck_magic_prefix(scan->buf, scan->n)) {
            trck_drop_byte(scan);
        }
        if (scan->n < (size_t)TRCK_FRAME_LEN) {
            return false;
        }
        if (trck_unpack(scan->buf, (size_t)TRCK_FRAME_LEN, out)) {
            scan->n = 0;
            return true;
        }
        if (trck_crc16(scan->buf, TRCK_BODY_LEN) != trck_rd_u16(scan->buf + TRCK_BODY_LEN)) {
            scan->crc_fail++;
        }
        trck_drop_byte(scan);
    }
}

#endif
