#include "trck_frame.h"

static const int kPiRxGpio = 18;
static const uint32_t kBaud = 115200;

static const char *kEmergency[] = {
    "none", "general", "lifeguard", "minfuel", "nordo", "unlawful", "downed", "reserved",
};

static TrckScan gScan;

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

void setup() {
    Serial.begin(115200);
    uint32_t start = millis();
    while (!Serial && millis() - start < 3000) {
    }

    Serial1.begin(kBaud, SERIAL_8N1, kPiRxGpio, -1);
    trck_scan_init(&gScan);

    Serial.printf("UART ingest 115200 8N1 RX GPIO %d. Waiting for TRCK.\n", kPiRxGpio);
    Serial.println("Wire Pi GPIO 14 (pin 8) to that RX, 3.3 V, common ground.");
}

void loop() {
    int n = Serial1.read();
    if (n < 0) {
        return;
    }
    TrckFix fix;
    if (trck_scan_feed(&gScan, (uint8_t)n, &fix)) {
        print_fix(&fix, &gScan);
    }
}
