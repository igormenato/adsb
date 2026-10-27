#include "trck_frame.h"

#include <string.h>

static const uint8_t k_magic[4] = {'T', 'R', 'C', 'K'};

static uint16_t rd_u16(const uint8_t *p) {
    return (uint16_t)((uint16_t)p[0] | ((uint16_t)p[1] << 8));
}

static uint32_t rd_u32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static int32_t rd_i32(const uint8_t *p) {
    return (int32_t)rd_u32(p);
}

uint16_t trck_crc16(const uint8_t *data, size_t len) {
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

static bool magic_prefix(const uint8_t *buf, size_t n) {
    size_t m = n < 4 ? n : 4;
    return memcmp(buf, k_magic, m) == 0;
}

bool trck_unpack(const uint8_t *bytes, size_t len, trck_fix_t *out) {
    if (bytes == NULL || out == NULL || len != TRCK_FRAME_LEN) {
        return false;
    }
    if (memcmp(bytes, k_magic, 4) != 0) {
        return false;
    }
    uint16_t expect = trck_crc16(bytes, 28);
    uint16_t got = rd_u16(bytes + 28);
    if (expect != got) {
        return false;
    }
    uint8_t emergency = bytes[27];
    if (emergency > 7) {
        return false;
    }
    out->unix_s = rd_u32(bytes + 4);
    out->icao = (uint32_t)bytes[8] | ((uint32_t)bytes[9] << 8) | ((uint32_t)bytes[10] << 16);
    out->lat_e7 = rd_i32(bytes + 11);
    out->lon_e7 = rd_i32(bytes + 15);
    out->alt_ft = rd_i32(bytes + 19);
    out->gs_kt = rd_u16(bytes + 23);
    out->track_deg = rd_u16(bytes + 25);
    out->emergency = emergency;
    return true;
}

void trck_scan_init(trck_scan_t *scan) {
    if (scan == NULL) {
        return;
    }
    scan->n = 0;
}

static void drop_byte(trck_scan_t *scan) {
    if (scan->n == 0) {
        return;
    }
    memmove(scan->buf, scan->buf + 1, scan->n - 1);
    scan->n--;
}

bool trck_scan_feed(trck_scan_t *scan, uint8_t byte, trck_fix_t *out) {
    if (scan == NULL || out == NULL) {
        return false;
    }
    if (scan->n >= TRCK_FRAME_LEN) {
        drop_byte(scan);
    }
    scan->buf[scan->n++] = byte;
    for (;;) {
        while (scan->n > 0 && !magic_prefix(scan->buf, scan->n)) {
            drop_byte(scan);
        }
        if (scan->n < TRCK_FRAME_LEN) {
            return false;
        }
        if (trck_unpack(scan->buf, TRCK_FRAME_LEN, out)) {
            scan->n = 0;
            return true;
        }
        drop_byte(scan);
    }
}
