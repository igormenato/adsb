#include <Arduino.h>
#include <atomic>
#include <cstring>

#include "trck_frame.h"

static const int kPiRxGpio = 11;
static const int kPiTxGpio = 10;
static const uint32_t kBaud = 115200;
static const uint32_t kAckWaitMs = 1000;
static const uint32_t kRxBuf = 2048;
static const uint16_t kRxStack = 2048;
static const UBaseType_t kRxCore = 0;
static const UBaseType_t kQLen = 16;

static const char *kEmergency[] = {
    "none", "general", "lifeguard", "minfuel", "nordo", "unlawful", "downed", "reserved",
};

static const char *kCmdName[] = {"ping", "status", "reboot", "shutdown", "pause", "resume", "restart"};
static const char *kCmdStatus[] = {"ok", "disabled", "unknown"};

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
    const char *status = ack->status <= TCMD_UNKNOWN ? kCmdStatus[ack->status] : "?";
    const char *state = ack->paused ? "paused" : "running";
    Serial.printf("ack cmd=%s status=%s state=%s uptime=%lu frames=%lu last_fix_age=", cmd, status,
                  state, (unsigned long)ack->uptime_s, (unsigned long)ack->frames_sent);
    if (ack->last_fix_age_s == TCMD_AGE_NONE) {
        Serial.println("none");
    } else {
        Serial.printf("%lu\n", (unsigned long)ack->last_fix_age_s);
    }
}

static void print_evt(const UartEvt *ev) {
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
    uint8_t raw[TCMD_LEN];
    trck_pack_cmd(cmd, raw);
    Serial1.write(raw, TCMD_LEN);
}

static bool wait_ack(uint8_t cmd, uint32_t ms) {
    uint32_t start = millis();
    UartEvt ev;
    while ((uint32_t)(millis() - start) < ms) {
        uint32_t left = ms - (uint32_t)(millis() - start);
        TickType_t ticks = pdMS_TO_TICKS(left);
        if (ticks == 0) {
            ticks = 1;
        }
        if (xQueueReceive(gQ, &ev, ticks) != pdTRUE) {
            continue;
        }
        print_evt(&ev);
        if (ev.kind == kEvtAck && ev.ack.cmd == cmd) {
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

    Serial.printf("UART 115200 8N1 RX GPIO %d TX GPIO %d. TRCK in, TCMD out.\n", kPiRxGpio,
                  kPiTxGpio);
    Serial.println("Wire Pi GPIO 14 (pin 8) to RX, Pi GPIO 15 (pin 10) to TX, 3.3 V, common ground.");
    Serial.println("Type ping, status, reboot, shutdown, pause, resume, restart, or stats.");
}

void loop() {
    UartEvt ev;
    if (xQueueReceive(gQ, &ev, pdMS_TO_TICKS(10)) == pdTRUE) {
        print_evt(&ev);
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
