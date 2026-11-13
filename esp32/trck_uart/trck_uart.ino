#include <stdio.h>
#include <string.h>

#include "trck_frame.h"

static const int kPiRxGpio = 18;
static const uint32_t kBaud = 115200;

static const char *kEmergency[] = {
    "none", "general", "lifeguard", "minfuel", "nordo", "unlawful", "downed", "reserved",
};

static TrckScan gScan;

static void fmt_deg(char *out, size_t cap, int32_t e7) {
    const char *sign = "";
    uint32_t abs_e7;
    if (e7 < 0) {
        sign = "-";
        abs_e7 = (uint32_t)(-(e7 + 1)) + 1u;
    } else {
        abs_e7 = (uint32_t)e7;
    }
    (void)snprintf(out, cap, "%s%lu.%07lu", sign, (unsigned long)(abs_e7 / 10000000u),
                   (unsigned long)(abs_e7 % 10000000u));
}

static void fmt_callsign(char *out, size_t cap, const uint8_t *cs) {
    size_t n = TRCK_CALLSIGN_LEN;
    while (n > 0 && cs[n - 1] == (uint8_t)' ') {
        n--;
    }
    if (n >= cap) {
        n = cap - 1;
    }
    memcpy(out, cs, n);
    out[n] = '\0';
}

static void fmt_opt_i32(char *out, size_t cap, int32_t value, int32_t unknown) {
    if (value == unknown) {
        (void)snprintf(out, cap, "unknown");
    } else {
        (void)snprintf(out, cap, "%ld", (long)value);
    }
}

static void fmt_opt_u16(char *out, size_t cap, uint16_t value, uint16_t unknown) {
    if (value == unknown) {
        (void)snprintf(out, cap, "unknown");
    } else {
        (void)snprintf(out, cap, "%u", (unsigned)value);
    }
}

static void print_fix(const TrckFix *fix, const TrckScan *scan) {
    char lat[16];
    char lon[16];
    char alt[16];
    char gs[16];
    char track[16];
    char callsign[TRCK_CALLSIGN_LEN + 1];
    fmt_deg(lat, sizeof lat, fix->lat_e7);
    fmt_deg(lon, sizeof lon, fix->lon_e7);
    fmt_opt_i32(alt, sizeof alt, fix->alt_ft, TRCK_ALT_UNKNOWN);
    fmt_opt_u16(gs, sizeof gs, fix->gs_kt, TRCK_SPEED_UNKNOWN);
    fmt_opt_u16(track, sizeof track, fix->track_deg, TRCK_TRACK_UNKNOWN);
    fmt_callsign(callsign, sizeof callsign, fix->callsign);
    const char *name = fix->emergency < 8 ? kEmergency[fix->emergency] : "?";
    Serial.printf(
        "icao=%06lX callsign=%s lat=%s lon=%s alt=%s gs=%s track=%s emergency=%u %s crc_fail=%lu "
        "resync=%lu\n",
        (unsigned long)fix->icao, callsign, lat, lon, alt, gs, track, (unsigned)fix->emergency, name,
        scan->crc_fail, scan->resync);
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
