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
#define TACK_LEN 9
#define TACK_BODY_LEN 7
#define TCMD_PING 0
#define TCMD_STATUS 1
#define TCMD_REBOOT 2
#define TCMD_SHUTDOWN 3
#define TCMD_PAUSE 4
#define TCMD_RESUME 5
#define TCMD_RESTART 6
#define TCMD_SIMULATION 7
#define TCMD_OK 0
#define TCMD_DISABLED 1
#define TCMD_UNKNOWN 2
#define TCMD_FAILED 3
#define TACK_HEALTH_OK 0
#define TACK_HEALTH_PAUSED 1
#define TACK_HEALTH_SIM 2
#define TACK_HEALTH_DOWN 255
#define TRCK_HB_LINE "STATUS_ADSB\r\n"
#define TRCK_HB_DOWN_MS 10000u

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

typedef struct {
    uint8_t cmd;
    uint8_t status;
    uint8_t health;
} TrckAck;

typedef struct {
    uint8_t buf[TACK_LEN];
    size_t n;
    unsigned long crc_fail;
    unsigned long resync;
} TrckAckScan;

static inline void trck_wr_u16(uint8_t *p, uint16_t v) {
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
}

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

static inline const char *trck_cmd_word(uint8_t cmd) {
    switch (cmd) {
    case TCMD_PING:
        return "PING";
    case TCMD_STATUS:
        return "STATUS";
    case TCMD_REBOOT:
        return "REBOOT";
    case TCMD_SHUTDOWN:
        return "SHUTDOWN";
    case TCMD_PAUSE:
        return "PAUSE";
    case TCMD_RESUME:
        return "RESUME";
    case TCMD_RESTART:
        return "RESTART";
    case TCMD_SIMULATION:
        return "SIMULATION";
    default:
        return NULL;
    }
}

static inline size_t trck_pack_cmd_line(uint8_t cmd, char *out, size_t cap) {
    const char *word = trck_cmd_word(cmd);
    if (word == NULL || out == NULL) {
        return 0;
    }
    size_t n = strlen(word);
    if (cap < n + 2) {
        return 0;
    }
    memcpy(out, word, n);
    out[n] = '\r';
    out[n + 1] = '\n';
    return n + 2;
}

static inline bool trck_unpack_ack(const uint8_t *bytes, size_t len, TrckAck *out) {
    if (bytes == NULL || out == NULL || len != (size_t)TACK_LEN) {
        return false;
    }
    if (memcmp(bytes, "TACK", 4) != 0) {
        return false;
    }
    if (trck_crc16(bytes, TACK_BODY_LEN) != trck_rd_u16(bytes + TACK_BODY_LEN)) {
        return false;
    }
    if (bytes[5] > TCMD_FAILED) {
        return false;
    }
    if (bytes[6] > TACK_HEALTH_SIM) {
        return false;
    }
    out->cmd = bytes[4];
    out->status = bytes[5];
    out->health = bytes[6];
    return true;
}

enum {
    TRCK_HB_SILENT = 0,
    TRCK_HB_PRINT = 1,
    TRCK_HB_DOWN = 2,
};

typedef struct {
    uint8_t have;
    uint8_t health;
    uint8_t down;
    uint32_t last_ms;
} TrckHb;

static inline bool trck_ack_matches(uint8_t pending, uint8_t ack_cmd) {
    return pending == ack_cmd;
}

static inline const char *trck_health_word(uint8_t health) {
    switch (health) {
    case TACK_HEALTH_OK:
        return "ok";
    case TACK_HEALTH_PAUSED:
        return "paused";
    case TACK_HEALTH_SIM:
        return "sim";
    case TACK_HEALTH_DOWN:
        return "down";
    default:
        return "?";
    }
}

static inline uint8_t trck_health(const TrckHb *h) {
    if (h == NULL || !h->have || h->down) {
        return TACK_HEALTH_DOWN;
    }
    return h->health;
}

static inline void trck_hb_init(TrckHb *h) {
    h->have = 0;
    h->health = TACK_HEALTH_DOWN;
    h->down = 0;
    h->last_ms = 0;
}

static inline void trck_hb_touch(TrckHb *h, uint32_t now_ms) {
    h->have = 1;
    h->down = 0;
    h->last_ms = now_ms;
}

static inline int trck_hb_on_status(TrckHb *h, uint8_t health, uint32_t now_ms) {
    int out = (!h->have || h->health != health) ? TRCK_HB_PRINT : TRCK_HB_SILENT;
    h->health = health;
    trck_hb_touch(h, now_ms);
    return out;
}

static inline int trck_hb_poll(TrckHb *h, uint32_t now_ms) {
    if (!h->have || h->down) {
        return TRCK_HB_SILENT;
    }
    if ((uint32_t)(now_ms - h->last_ms) < TRCK_HB_DOWN_MS) {
        return TRCK_HB_SILENT;
    }
    h->down = 1;
    return TRCK_HB_DOWN;
}

static inline void trck_ack_scan_init(TrckAckScan *scan) {
    scan->n = 0;
    scan->crc_fail = 0;
    scan->resync = 0;
}

static inline bool trck_ack_magic_prefix(const uint8_t *buf, size_t n) {
    size_t m = n < 4 ? n : 4;
    return memcmp(buf, "TACK", m) == 0;
}

static inline void trck_ack_drop_byte(TrckAckScan *scan) {
    if (scan->n == 0) {
        return;
    }
    memmove(scan->buf, scan->buf + 1, scan->n - 1);
    scan->n--;
    scan->resync++;
}

static inline bool trck_ack_feed(TrckAckScan *scan, uint8_t byte, TrckAck *out) {
    if (scan->n >= (size_t)TACK_LEN) {
        trck_ack_drop_byte(scan);
    }
    scan->buf[scan->n++] = byte;
    for (;;) {
        while (scan->n > 0 && !trck_ack_magic_prefix(scan->buf, scan->n)) {
            trck_ack_drop_byte(scan);
        }
        if (scan->n < (size_t)TACK_LEN) {
            return false;
        }
        if (trck_unpack_ack(scan->buf, (size_t)TACK_LEN, out)) {
            scan->n = 0;
            return true;
        }
        if (trck_crc16(scan->buf, TACK_BODY_LEN) != trck_rd_u16(scan->buf + TACK_BODY_LEN)) {
            scan->crc_fail++;
        }
        trck_ack_drop_byte(scan);
    }
}

#endif
