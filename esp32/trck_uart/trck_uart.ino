/* UART ingest for trck. Not the ESP32-S3 OBC radio firmware.
 * Does not overlay a packed struct. Does not send the UART frame on a radio.
 *
 * Arduino-ESP32 3.3.12. Serial1 UART1 RX = GPIO 18.
 */

#include <stdint.h>
#include <stdio.h>
#include <string.h>

static const int kPiRxGpio = 18;
static const uint32_t kBaud = 115200;
static const uint8_t kMagic[4] = {'T', 'R', 'C', 'K'};
static const int kFrameLen = 30;
static const int32_t kAltUnknown = INT32_MIN;

static const char *kEmergency[] = {
    "none", "general", "lifeguard", "minfuel", "nordo", "unlawful", "downed", "reserved",
};

struct TrckFix {
    uint32_t unix_s;
    uint32_t icao;
    int32_t lat_e7;
    int32_t lon_e7;
    int32_t alt_ft;
    uint16_t gs_kt;
    uint16_t track_deg;
    uint8_t emergency;
};

struct TrckScan {
    uint8_t buf[30];
    size_t n;
};

static TrckScan gScan;

static uint16_t rdU16(const uint8_t *p) {
    return (uint16_t)((uint16_t)p[0] | ((uint16_t)p[1] << 8));
}

static uint32_t rdU32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static int32_t rdI32(const uint8_t *p) {
    return (int32_t)rdU32(p);
}

/* CRC-16/IBM-3740 (CRC-16/CCITT-FALSE). "123456789" -> 0x29B1. */
static uint16_t trckCrc16(const uint8_t *data, size_t len) {
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

static bool magicPrefix(const uint8_t *buf, size_t n) {
    size_t m = n < 4 ? n : 4;
    return memcmp(buf, kMagic, m) == 0;
}

static bool trckUnpack(const uint8_t *bytes, size_t len, TrckFix *out) {
    if (bytes == NULL || out == NULL || len != (size_t)kFrameLen) {
        return false;
    }
    if (memcmp(bytes, kMagic, 4) != 0) {
        return false;
    }
    uint16_t expect = trckCrc16(bytes, 28);
    uint16_t got = rdU16(bytes + 28);
    if (expect != got) {
        return false;
    }
    uint8_t emergency = bytes[27];
    if (emergency > 7) {
        return false;
    }
    out->unix_s = rdU32(bytes + 4);
    out->icao = (uint32_t)bytes[8] | ((uint32_t)bytes[9] << 8) | ((uint32_t)bytes[10] << 16);
    out->lat_e7 = rdI32(bytes + 11);
    out->lon_e7 = rdI32(bytes + 15);
    out->alt_ft = rdI32(bytes + 19);
    out->gs_kt = rdU16(bytes + 23);
    out->track_deg = rdU16(bytes + 25);
    out->emergency = emergency;
    return true;
}

static void trckScanInit(TrckScan *scan) {
    scan->n = 0;
}

static void dropByte(TrckScan *scan) {
    if (scan->n == 0) {
        return;
    }
    memmove(scan->buf, scan->buf + 1, scan->n - 1);
    scan->n--;
}

static bool trckScanFeed(TrckScan *scan, uint8_t byte, TrckFix *out) {
    if (scan->n >= (size_t)kFrameLen) {
        dropByte(scan);
    }
    scan->buf[scan->n++] = byte;
    for (;;) {
        while (scan->n > 0 && !magicPrefix(scan->buf, scan->n)) {
            dropByte(scan);
        }
        if (scan->n < (size_t)kFrameLen) {
            return false;
        }
        if (trckUnpack(scan->buf, (size_t)kFrameLen, out)) {
            scan->n = 0;
            return true;
        }
        dropByte(scan);
    }
}

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

static void printFix(const TrckFix *fix) {
    char lat[16];
    char lon[16];
    char alt[16];
    fmtDeg(lat, sizeof lat, fix->lat_e7);
    fmtDeg(lon, sizeof lon, fix->lon_e7);
    if (fix->alt_ft == kAltUnknown) {
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
    trckScanInit(&gScan);

    Serial.printf("UART ingest 115200 8N1 RX GPIO %d. Waiting for TRCK.\n", kPiRxGpio);
    Serial.println("Wire Pi GPIO 14 (pin 8) to that RX, 3.3 V, common ground.");
}

void loop() {
    int n = Serial1.read();
    if (n < 0) {
        return;
    }
    TrckFix fix;
    if (trckScanFeed(&gScan, (uint8_t)n, &fix)) {
        printFix(&fix);
    }
}
