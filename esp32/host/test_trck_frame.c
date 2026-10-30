/* Host checks of the same CRC/scan as esp32/trck_uart/trck_uart.ino. */

#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static const uint8_t kMagic[4] = {'T', 'R', 'C', 'K'};
static const int kFrameLen = 30;
static const int32_t kAltUnknown = INT32_MIN;
static const uint16_t kSpeedUnknown = 0xFFFFu;
static const uint16_t kTrackUnknown = 0xFFFFu;

typedef struct {
    uint32_t unix_s;
    uint32_t icao;
    int32_t lat_e7;
    int32_t lon_e7;
    int32_t alt_ft;
    uint16_t gs_kt;
    uint16_t track_deg;
    uint8_t emergency;
} TrckFix;

typedef struct {
    uint8_t buf[30];
    size_t n;
} TrckScan;

static uint16_t rdU16(const uint8_t *p) {
    return (uint16_t)((uint16_t)p[0] | ((uint16_t)p[1] << 8));
}

static uint32_t rdU32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static int32_t rdI32(const uint8_t *p) {
    return (int32_t)rdU32(p);
}

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

static const uint8_t kCheck0[] = {
    0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0xbc, 0x0a, 0x40, 0x40, 0x2c, 0x42, 0x06, 0x60,
    0x18, 0xee, 0xf3, 0xe0, 0x2e, 0x00, 0x00, 0x18, 0x01, 0x5a, 0x00, 0x00, 0xd7, 0x48,
};

static const uint8_t kCheck1[] = {
    0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0x1d, 0x62, 0x40, 0xf5, 0xd0, 0x25, 0x1f, 0x7e,
    0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xc2, 0x01, 0x0f, 0x01, 0x00, 0x73, 0x81,
};

static const uint8_t kCheck2[] = {
    0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0x23, 0xa1, 0x4c, 0xe0, 0xb2, 0xad, 0x1e, 0xe0,
    0x55, 0xbb, 0xff, 0x40, 0x1f, 0x00, 0x00, 0xdc, 0x00, 0xb4, 0x00, 0x01, 0x7f, 0xb6,
};

static const uint8_t kUnknown[] = {
    0x54, 0x52, 0x43, 0x4b, 0x0a, 0x00, 0x00, 0x00, 0x23, 0xc1, 0xab, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0xff, 0xff, 0xff, 0xff, 0x00, 0x2a, 0x6a,
};

static void expectFix(const uint8_t *frame, uint32_t icao, int32_t lat, int32_t lon, int32_t alt,
                      uint8_t emergency) {
    TrckFix fix;
    assert(trckUnpack(frame, (size_t)kFrameLen, &fix));
    assert(fix.unix_s == 1700000000u);
    assert(fix.icao == icao);
    assert(fix.lat_e7 == lat);
    assert(fix.lon_e7 == lon);
    assert(fix.alt_ft == alt);
    assert(fix.emergency == emergency);
}

static size_t feedBytes(TrckScan *scan, const uint8_t *bytes, size_t len, TrckFix *got, size_t cap) {
    size_t n = 0;
    for (size_t i = 0; i < len; i++) {
        TrckFix fix;
        if (trckScanFeed(scan, bytes[i], &fix)) {
            assert(n < cap);
            got[n++] = fix;
        }
    }
    return n;
}

int main(void) {
    assert(trckCrc16((const uint8_t *)"123456789", 9) == 0x29B1);

    expectFix(kCheck0, 0x400ABCu, 105000000, -202500000, 12000, 0);
    expectFix(kCheck1, 0x40621Du, 522572021, 39193726, 38000, 0);
    expectFix(kCheck2, 0x4CA123u, 514700000, -4500000, 8000, 1);

    TrckFix unknown;
    assert(trckUnpack(kUnknown, (size_t)kFrameLen, &unknown));
    assert(unknown.unix_s == 10);
    assert(unknown.icao == 0xABC123u);
    assert(unknown.alt_ft == kAltUnknown);
    assert(unknown.gs_kt == kSpeedUnknown);
    assert(unknown.track_deg == kTrackUnknown);
    assert(unknown.emergency == 0);

    uint8_t bad[30];
    memcpy(bad, kCheck1, (size_t)kFrameLen);
    bad[29] ^= 0x5A;
    TrckFix dropped;
    assert(!trckUnpack(bad, (size_t)kFrameLen, &dropped));

    TrckScan scan;
    trckScanInit(&scan);
    TrckFix got[8];
    uint8_t burst[90];
    memcpy(burst, kCheck0, (size_t)kFrameLen);
    memcpy(burst + kFrameLen, kCheck1, (size_t)kFrameLen);
    memcpy(burst + kFrameLen * 2, kCheck2, (size_t)kFrameLen);
    assert(feedBytes(&scan, burst, sizeof burst, got, 8) == 3);
    assert(got[0].icao == 0x400ABCu && got[0].emergency == 0);
    assert(got[1].icao == 0x40621Du && got[1].lat_e7 == 522572021);
    assert(got[2].icao == 0x4CA123u && got[2].emergency == 1);

    trckScanInit(&scan);
    uint8_t noisy[32];
    noisy[0] = 0x00;
    noisy[1] = 0xFF;
    memcpy(noisy + 2, kCheck0, (size_t)kFrameLen);
    assert(feedBytes(&scan, noisy, sizeof noisy, got, 8) == 1);
    assert(got[0].icao == 0x400ABCu);

    trckScanInit(&scan);
    uint8_t split[33];
    split[0] = 'T';
    split[1] = 'R';
    split[2] = 'C';
    memcpy(split + 3, kCheck1, (size_t)kFrameLen);
    assert(feedBytes(&scan, split, sizeof split, got, 8) == 1);
    assert(got[0].icao == 0x40621Du);

    trckScanInit(&scan);
    uint8_t thenGood[60];
    memcpy(thenGood, bad, (size_t)kFrameLen);
    memcpy(thenGood + kFrameLen, kCheck2, (size_t)kFrameLen);
    size_t n = feedBytes(&scan, thenGood, sizeof thenGood, got, 8);
    assert(n == 1);
    assert(got[0].icao == 0x4CA123u);

    puts("ok");
    return 0;
}
