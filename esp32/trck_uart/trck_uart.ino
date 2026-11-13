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
#define TCMD_LEN 7
#define TCMD_BODY_LEN 5
#define TACK_LEN 20
#define TACK_BODY_LEN 18
#define TCMD_PING 0
#define TCMD_STATUS 1
#define TCMD_REBOOT 2
#define TCMD_SHUTDOWN 3
#define TCMD_OK 0
#define TCMD_DISABLED 1
#define TCMD_UNKNOWN 2
#define TCMD_AGE_NONE 0xFFFFFFFFu

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
    uint32_t uptime_s;
    uint32_t frames_sent;
    uint32_t last_fix_age_s;
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

static inline void trck_pack_cmd(uint8_t cmd, uint8_t out[TCMD_LEN]) {
    memcpy(out, "TCMD", 4);
    out[4] = cmd;
    trck_wr_u16(out + TCMD_BODY_LEN, trck_crc16(out, TCMD_BODY_LEN));
}

static inline bool trck_unpack_cmd(const uint8_t *bytes, size_t len, uint8_t *cmd) {
    if (bytes == NULL || cmd == NULL || len != (size_t)TCMD_LEN) {
        return false;
    }
    if (memcmp(bytes, "TCMD", 4) != 0) {
        return false;
    }
    if (trck_crc16(bytes, TCMD_BODY_LEN) != trck_rd_u16(bytes + TCMD_BODY_LEN)) {
        return false;
    }
    *cmd = bytes[4];
    return true;
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
    if (bytes[5] > TCMD_UNKNOWN) {
        return false;
    }
    out->cmd = bytes[4];
    out->status = bytes[5];
    out->uptime_s = trck_rd_u32(bytes + 6);
    out->frames_sent = trck_rd_u32(bytes + 10);
    out->last_fix_age_s = trck_rd_u32(bytes + 14);
    return true;
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

#ifdef ARDUINO
static const int kPiRxGpio = 11;
static const int kPiTxGpio = 10;
static const uint32_t kBaud = 115200;

static const char *kEmergency[] = {
    "none", "general", "lifeguard", "minfuel", "nordo", "unlawful", "downed", "reserved",
};

static const char *kCmdName[] = {"ping", "status", "reboot", "shutdown"};
static const char *kCmdStatus[] = {"ok", "disabled", "unknown"};

static TrckScan gScan;
static TrckAckScan gAck;
static char gLine[24];
static size_t gLineN;

static void print_fix(const TrckFix *fix, const TrckScan *scan) {
    int n = TRCK_CALLSIGN_LEN;
    while (n > 0 && fix->callsign[n - 1] == (uint8_t)' ') {
        n--;
    }
    Serial.printf("icao=%06lX callsign=%.*s lat=%.7f lon=%.7f alt=", (unsigned long)fix->icao, n,
                  (const char *)fix->callsign, fix->lat_e7 / 1e7, fix->lon_e7 / 1e7);
    if (fix->alt_ft == TRCK_ALT_UNKNOWN) {
        Serial.print("unknown");
    } else {
        Serial.printf("%ld", (long)fix->alt_ft);
    }
    Serial.print(" gs=");
    if (fix->gs_kt == TRCK_SPEED_UNKNOWN) {
        Serial.print("unknown");
    } else {
        Serial.printf("%u", (unsigned)fix->gs_kt);
    }
    Serial.print(" track=");
    if (fix->track_deg == TRCK_TRACK_UNKNOWN) {
        Serial.print("unknown");
    } else {
        Serial.printf("%u", (unsigned)fix->track_deg);
    }
    Serial.printf(" emergency=%u %s crc_fail=%lu resync=%lu\n", (unsigned)fix->emergency,
                  kEmergency[fix->emergency], scan->crc_fail, scan->resync);
}

static void print_ack(const TrckAck *ack) {
    const char *cmd = ack->cmd <= TCMD_SHUTDOWN ? kCmdName[ack->cmd] : "unknown";
    const char *status = ack->status <= TCMD_UNKNOWN ? kCmdStatus[ack->status] : "?";
    Serial.printf("ack cmd=%s status=%s uptime=%lu frames=%lu last_fix_age=", cmd, status,
                  (unsigned long)ack->uptime_s, (unsigned long)ack->frames_sent);
    if (ack->last_fix_age_s == TCMD_AGE_NONE) {
        Serial.println("none");
    } else {
        Serial.printf("%lu\n", (unsigned long)ack->last_fix_age_s);
    }
}

static bool send_named_cmd(const char *line) {
    uint8_t cmd;
    if (strcmp(line, "ping") == 0) {
        cmd = TCMD_PING;
    } else if (strcmp(line, "status") == 0) {
        cmd = TCMD_STATUS;
    } else if (strcmp(line, "reboot") == 0) {
        cmd = TCMD_REBOOT;
    } else if (strcmp(line, "shutdown") == 0) {
        cmd = TCMD_SHUTDOWN;
    } else {
        return false;
    }
    uint8_t raw[TCMD_LEN];
    trck_pack_cmd(cmd, raw);
    Serial1.write(raw, TCMD_LEN);
    return true;
}

void setup() {
    Serial.begin(115200);
    uint32_t start = millis();
    while (!Serial && millis() - start < 3000) {
    }

    Serial1.begin(kBaud, SERIAL_8N1, kPiRxGpio, kPiTxGpio);
    trck_scan_init(&gScan);
    trck_ack_scan_init(&gAck);

    Serial.printf("UART 115200 8N1 RX GPIO %d TX GPIO %d. TRCK in, TCMD out.\n", kPiRxGpio,
                  kPiTxGpio);
    Serial.println("Wire Pi GPIO 14 (pin 8) to RX, Pi GPIO 15 (pin 10) to TX, 3.3 V, common ground.");
    Serial.println("Type ping, status, reboot, or shutdown.");
}

void loop() {
    int n = Serial1.read();
    if (n >= 0) {
        TrckFix fix;
        if (trck_scan_feed(&gScan, (uint8_t)n, &fix)) {
            print_fix(&fix, &gScan);
        }
        TrckAck ack;
        if (trck_ack_feed(&gAck, (uint8_t)n, &ack)) {
            print_ack(&ack);
        }
    }
    while (Serial.available() > 0) {
        char c = (char)Serial.read();
        if (c == '\n' || c == '\r') {
            if (gLineN == 0) {
                continue;
            }
            gLine[gLineN] = 0;
            gLineN = 0;
            for (char *p = gLine; *p != 0; p++) {
                if (*p >= 'A' && *p <= 'Z') {
                    *p = (char)(*p - 'A' + 'a');
                }
            }
            if (!send_named_cmd(gLine)) {
                Serial.println("commands: ping status reboot shutdown");
            }
        } else if (gLineN < sizeof gLine - 1) {
            gLine[gLineN++] = c;
        } else {
            gLineN = 0;
        }
    }
}
#endif
