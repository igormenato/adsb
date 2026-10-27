/* UART ingest for trck. Not the ESP32-S3 OBC radio firmware.
 * Does not overlay a packed struct. Does not send the UART frame on a radio.
 *
 * Arduino-ESP32 3.3.12. Serial1 UART1 RX = GPIO 18.
 */

#include <stdio.h>

#include "trck_frame.h"

static const int kPiRxGpio = 18;
static const uint32_t kBaud = 115200;
static trck_scan_t g_scan;

static const char *kEmergency[] = {
    "none", "general", "lifeguard", "minfuel", "nordo", "unlawful", "downed", "reserved",
};

static void fmtDeg(char *out, size_t cap, int32_t e7) {
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

static void printFix(const trck_fix_t *fix) {
    char lat[16];
    char lon[16];
    char alt[16];
    fmtDeg(lat, sizeof lat, fix->lat_e7);
    fmtDeg(lon, sizeof lon, fix->lon_e7);
    if (fix->alt_ft == TRCK_ALT_UNKNOWN) {
        (void)snprintf(alt, sizeof alt, "unknown");
    } else {
        (void)snprintf(alt, sizeof alt, "%ld", (long)fix->alt_ft);
    }
    const char *name = fix->emergency < 8 ? kEmergency[fix->emergency] : "?";
    Serial.printf("icao=%06lX lat=%s lon=%s alt=%s emergency=%u %s\n", (unsigned long)fix->icao, lat,
                  lon, alt, (unsigned)fix->emergency, name);
}

void setup() {
    Serial.begin(115200);
    uint32_t start = millis();
    while (!Serial && millis() - start < 3000) {
    }

    Serial1.begin(kBaud, SERIAL_8N1, kPiRxGpio, -1);
    trck_scan_init(&g_scan);

    Serial.printf("UART ingest 115200 8N1 RX GPIO %d. Waiting for TRCK.\n", kPiRxGpio);
    Serial.println("Wire Pi GPIO 14 (pin 8) to that RX, 3.3 V, common ground.");
}

void loop() {
    int n = Serial1.read();
    if (n < 0) {
        return;
    }
    trck_fix_t fix;
    if (trck_scan_feed(&g_scan, (uint8_t)n, &fix)) {
        printFix(&fix);
    }
}
