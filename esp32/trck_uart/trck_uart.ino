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
#define TACK_LEN 21
#define TACK_BODY_LEN 19
#define TCMD_PING 0
#define TCMD_STATUS 1
#define TCMD_REBOOT 2
#define TCMD_SHUTDOWN 3
#define TCMD_PAUSE 4
#define TCMD_RESUME 5
#define TCMD_RESTART 6
#define TCMD_OK 0
#define TCMD_DISABLED 1
#define TCMD_UNKNOWN 2
#define TCMD_FAILED 3
#define TCMD_AGE_NONE 0xFFFFFFFFu
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
    uint32_t uptime_s;
    uint32_t frames_sent;
    uint32_t last_fix_age_s;
    uint8_t paused;
} TrckAck;

typedef struct {
    uint8_t buf[TACK_LEN];
    size_t n;
    unsigned long crc_fail;
    unsigned long resync;
} TrckAckScan;

#ifdef ARDUINO
struct UartEvt;
#endif

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
    if (bytes[18] > 1) {
        return false;
    }
    out->cmd = bytes[4];
    out->status = bytes[5];
    out->uptime_s = trck_rd_u32(bytes + 6);
    out->frames_sent = trck_rd_u32(bytes + 10);
    out->last_fix_age_s = trck_rd_u32(bytes + 14);
    out->paused = bytes[18];
    return true;
}

enum {
    TRCK_HB_SILENT = 0,
    TRCK_HB_PRINT = 1,
    TRCK_HB_DOWN = 2,
};

typedef struct {
    uint8_t have;
    uint8_t paused;
    uint8_t down;
    uint32_t last_ms;
} TrckHb;

static inline bool trck_ack_matches(uint8_t pending, uint8_t ack_cmd) {
    return pending == ack_cmd;
}

static inline void trck_hb_init(TrckHb *h) {
    h->have = 0;
    h->paused = 0;
    h->down = 0;
    h->last_ms = 0;
}

static inline void trck_hb_touch(TrckHb *h, uint32_t now_ms) {
    h->have = 1;
    h->down = 0;
    h->last_ms = now_ms;
}

static inline int trck_hb_on_status(TrckHb *h, uint8_t paused, uint32_t now_ms) {
    int out = (!h->have || h->paused != paused) ? TRCK_HB_PRINT : TRCK_HB_SILENT;
    h->paused = paused;
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

#ifdef ARDUINO
#include <atomic>

static const int kPiRxGpio = 11;
static const int kPiTxGpio = 10;
static const uint32_t kBaud = 115200;
static const uint32_t kAckWaitMs = 1000;
static const uint32_t kHbMs = 3000;
static const uint32_t kRxBuf = 2048;
static const uint16_t kRxStack = 2048;
static const UBaseType_t kRxCore = 0;
static const UBaseType_t kQLen = 16;

static const char *kEmergency[] = {
    "none", "general", "lifeguard", "minfuel", "nordo", "unlawful", "downed", "reserved",
};

static const char *kCmdName[] = {"ping", "status", "reboot", "shutdown", "pause", "resume", "restart"};
static const char *kCmdStatus[] = {"ok", "disabled", "unknown", "failed"};

enum { kEvtFix = 0, kEvtAck = 1 };

struct UartEvt {
    uint8_t kind;
    TrckFix fix;
    TrckAck ack;
};

static QueueHandle_t gQ;
static char gLine[24];
static size_t gLineN;
static std::atomic<unsigned long> gFrames{0};
static std::atomic<unsigned long> gCrcFail{0};
static std::atomic<unsigned long> gResync{0};
static std::atomic<unsigned long> gQDrop{0};
static uint32_t gHbAt;
static TrckHb gWatch;

static void print_fix(const TrckFix *fix) {
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
                  kEmergency[fix->emergency], gCrcFail.load(std::memory_order_relaxed),
                  gResync.load(std::memory_order_relaxed));
}

static void print_ack(const TrckAck *ack) {
    const char *cmd = ack->cmd <= TCMD_RESTART ? kCmdName[ack->cmd] : "unknown";
    const char *status = ack->status <= TCMD_FAILED ? kCmdStatus[ack->status] : "?";
    const char *state = ack->paused ? "paused" : "running";
    Serial.printf("ack cmd=%s status=%s state=%s uptime=%lu frames=%lu last_fix_age=", cmd, status,
                  state, (unsigned long)ack->uptime_s, (unsigned long)ack->frames_sent);
    if (ack->last_fix_age_s == TCMD_AGE_NONE) {
        Serial.println("none");
    } else {
        Serial.printf("%lu\n", (unsigned long)ack->last_fix_age_s);
    }
}

static void print_evt(const struct UartEvt *ev) {
    if (ev->kind == kEvtFix) {
        print_fix(&ev->fix);
    } else {
        print_ack(&ev->ack);
    }
}

static void print_stats(void) {
    Serial.printf("frames=%lu crc_fail=%lu resync=%lu qdrop=%lu\n",
                  gFrames.load(std::memory_order_relaxed), gCrcFail.load(std::memory_order_relaxed),
                  gResync.load(std::memory_order_relaxed), gQDrop.load(std::memory_order_relaxed));
}

static void write_cmd(uint8_t cmd) {
    char raw[16];
    size_t n = trck_pack_cmd_line(cmd, raw, sizeof raw);
    if (n == 0) {
        return;
    }
    Serial1.write((const uint8_t *)raw, n);
}

static void maybe_hb(void) {
    uint32_t now = millis();
    if ((uint32_t)(now - gHbAt) < kHbMs) {
        return;
    }
    Serial1.print(TRCK_HB_LINE);
    gHbAt = now;
}

static void maybe_down(void) {
    if (trck_hb_poll(&gWatch, millis()) == TRCK_HB_DOWN) {
        Serial.println("trck down");
    }
}

static void on_ack(const TrckAck *ack, bool user) {
    uint32_t now = millis();
    if (ack->cmd == TCMD_STATUS) {
        int act = trck_hb_on_status(&gWatch, ack->paused, now);
        if (user || act == TRCK_HB_PRINT) {
            print_ack(ack);
        }
        return;
    }
    trck_hb_touch(&gWatch, now);
    if (user) {
        print_ack(ack);
    }
}

static bool wait_ack(uint8_t cmd, uint32_t ms) {
    uint32_t start = millis();
    UartEvt ev;
    while ((uint32_t)(millis() - start) < ms) {
        maybe_hb();
        maybe_down();
        uint32_t left = ms - (uint32_t)(millis() - start);
        TickType_t ticks = pdMS_TO_TICKS(left);
        if (ticks == 0) {
            ticks = 1;
        }
        if (xQueueReceive(gQ, &ev, ticks) != pdTRUE) {
            continue;
        }
        if (ev.kind != kEvtAck) {
            print_evt(&ev);
            continue;
        }
        bool match = trck_ack_matches(cmd, ev.ack.cmd);
        on_ack(&ev.ack, match);
        if (match) {
            return true;
        }
    }
    return false;
}

static bool cmd_no_resend(uint8_t cmd) {
    return cmd == TCMD_REBOOT || cmd == TCMD_SHUTDOWN || cmd == TCMD_RESTART;
}

static void run_cmd(uint8_t cmd) {
    write_cmd(cmd);
    if (wait_ack(cmd, kAckWaitMs)) {
        return;
    }
    if (cmd_no_resend(cmd)) {
        write_cmd(TCMD_STATUS);
        if (wait_ack(TCMD_STATUS, kAckWaitMs)) {
            return;
        }
    } else {
        write_cmd(cmd);
        if (wait_ack(cmd, kAckWaitMs)) {
            return;
        }
    }
    Serial.println("no reply");
}

static bool parse_cmd(const char *line, uint8_t *cmd) {
    if (strcmp(line, "ping") == 0) {
        *cmd = TCMD_PING;
    } else if (strcmp(line, "status") == 0) {
        *cmd = TCMD_STATUS;
    } else if (strcmp(line, "reboot") == 0) {
        *cmd = TCMD_REBOOT;
    } else if (strcmp(line, "shutdown") == 0) {
        *cmd = TCMD_SHUTDOWN;
    } else if (strcmp(line, "pause") == 0) {
        *cmd = TCMD_PAUSE;
    } else if (strcmp(line, "resume") == 0) {
        *cmd = TCMD_RESUME;
    } else if (strcmp(line, "restart") == 0) {
        *cmd = TCMD_RESTART;
    } else {
        return false;
    }
    return true;
}

static void uart_rx_task(void *arg) {
    (void)arg;
    TrckScan scan;
    TrckAckScan ack_scan;
    trck_scan_init(&scan);
    trck_ack_scan_init(&ack_scan);
    for (;;) {
        int n = Serial1.read();
        if (n < 0) {
            vTaskDelay(1);
            continue;
        }
        UartEvt ev;
        TrckFix fix;
        if (trck_scan_feed(&scan, (uint8_t)n, &fix)) {
            gFrames.fetch_add(1, std::memory_order_relaxed);
            ev.kind = kEvtFix;
            ev.fix = fix;
            if (xQueueSend(gQ, &ev, 0) != pdTRUE) {
                gQDrop.fetch_add(1, std::memory_order_relaxed);
            }
        }
        TrckAck ack;
        if (trck_ack_feed(&ack_scan, (uint8_t)n, &ack)) {
            ev.kind = kEvtAck;
            ev.ack = ack;
            if (xQueueSend(gQ, &ev, 0) != pdTRUE) {
                gQDrop.fetch_add(1, std::memory_order_relaxed);
            }
        }
        gCrcFail.store(scan.crc_fail, std::memory_order_relaxed);
        gResync.store(scan.resync, std::memory_order_relaxed);
    }
}

void setup() {
    Serial.begin(115200);
    uint32_t start = millis();
    while (!Serial && millis() - start < 3000) {
    }

    Serial1.setRxBufferSize(kRxBuf);
    Serial1.begin(kBaud, SERIAL_8N1, kPiRxGpio, kPiTxGpio);

    gQ = xQueueCreate(kQLen, sizeof(UartEvt));
    xTaskCreatePinnedToCore(uart_rx_task, "uart1", kRxStack, NULL, 2, NULL, kRxCore);

    gHbAt = millis();
    trck_hb_init(&gWatch);
    Serial.printf("UART 115200 8N1 RX GPIO %d TX GPIO %d. TRCK in, ASCII cmd out.\n", kPiRxGpio,
                  kPiTxGpio);
    Serial.println("Wire Pi GPIO 14 (pin 8) to RX, Pi GPIO 15 (pin 10) to TX, 3.3 V, common ground.");
    Serial.println("Type ping, status, reboot, shutdown, pause, resume, restart, or stats.");
}

void loop() {
    maybe_hb();
    maybe_down();
    UartEvt ev;
    if (xQueueReceive(gQ, &ev, pdMS_TO_TICKS(10)) == pdTRUE) {
        if (ev.kind == kEvtAck) {
            on_ack(&ev.ack, false);
        } else {
            print_evt(&ev);
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
            uint8_t cmd;
            if (strcmp(gLine, "stats") == 0) {
                print_stats();
            } else if (parse_cmd(gLine, &cmd)) {
                run_cmd(cmd);
            } else {
                Serial.println("commands: ping status reboot shutdown pause resume restart stats");
            }
        } else if (gLineN < sizeof gLine - 1) {
            gLine[gLineN++] = c;
        } else {
            gLineN = 0;
        }
    }
}
#endif
