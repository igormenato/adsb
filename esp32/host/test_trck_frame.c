#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../trck_uart/trck_uart.ino"

static const uint8_t kPublished[] = {
    0x54, 0x52, 0x43, 0x4b, 0x1d, 0x62, 0x40, 0x42, 0x41, 0x57, 0x31, 0x32, 0x33, 0x20, 0x20, 0xf5,
    0xd0, 0x25, 0x1f, 0x7e, 0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xc2, 0x01, 0x0f, 0x01, 0x00,
    0x60, 0xef,
};

static const uint8_t kCheck0[] = {
    0x54, 0x52, 0x43, 0x4b, 0xbc, 0x0a, 0x40, 0x53, 0x49, 0x4d, 0x30, 0x30, 0x30, 0x31, 0x20, 0x40,
    0x2c, 0x42, 0x06, 0x60, 0x18, 0xee, 0xf3, 0xe0, 0x2e, 0x00, 0x00, 0x18, 0x01, 0x5a, 0x00, 0x00,
    0x3e, 0x4c,
};

static const uint8_t kCheck2[] = {
    0x54, 0x52, 0x43, 0x4b, 0x23, 0xa1, 0x4c, 0x45, 0x5a, 0x59, 0x34, 0x35, 0x36, 0x20, 0x20, 0xe0,
    0xb2, 0xad, 0x1e, 0xe0, 0x55, 0xbb, 0xff, 0x40, 0x1f, 0x00, 0x00, 0xdc, 0x00, 0xb4, 0x00, 0x01,
    0xaa, 0x98,
};

static uint8_t *read_file(const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    assert(f != NULL);
    assert(fseek(f, 0, SEEK_END) == 0);
    long n = ftell(f);
    assert(n >= 0);
    assert(fseek(f, 0, SEEK_SET) == 0);
    uint8_t *buf = malloc((size_t)n);
    assert(buf != NULL);
    assert(fread(buf, 1, (size_t)n, f) == (size_t)n);
    fclose(f);
    *len = (size_t)n;
    return buf;
}

static size_t feed_bytes(TrckScan *scan, const uint8_t *bytes, size_t len, TrckFix *got, size_t cap) {
    size_t n = 0;
    for (size_t i = 0; i < len; i++) {
        TrckFix fix;
        if (trck_scan_feed(scan, bytes[i], &fix)) {
            assert(n < cap);
            got[n++] = fix;
        }
    }
    return n;
}

int main(int argc, char **argv) {
    assert(argc == 2);
    assert(trck_crc16((const uint8_t *)"123456789", 9) == 0x29B1);

    TrckFix published;
    assert(trck_unpack(kPublished, sizeof kPublished, &published));
    assert(published.icao == 0x40621Du);
    assert(memcmp(published.callsign, "BAW123  ", TRCK_CALLSIGN_LEN) == 0);
    assert(published.lat_e7 == 522572021);
    assert(published.lon_e7 == 39193726);
    assert(published.alt_ft == 38000);
    assert(published.gs_kt == 450);
    assert(published.track_deg == 271);
    assert(published.emergency == 0);

    TrckFix check0;
    assert(trck_unpack(kCheck0, sizeof kCheck0, &check0));
    assert(check0.icao == 0x400ABCu);
    assert(memcmp(check0.callsign, "SIM0001 ", TRCK_CALLSIGN_LEN) == 0);
    assert(check0.lat_e7 == 105000000);
    assert(check0.lon_e7 == -202500000);
    assert(check0.alt_ft == 12000);
    assert(check0.emergency == 0);

    TrckFix check2;
    assert(trck_unpack(kCheck2, sizeof kCheck2, &check2));
    assert(check2.icao == 0x4CA123u);
    assert(memcmp(check2.callsign, "EZY456  ", TRCK_CALLSIGN_LEN) == 0);
    assert(check2.emergency == 1);

    uint8_t bad[TRCK_FRAME_LEN];
    memcpy(bad, kPublished, TRCK_FRAME_LEN);
    bad[TRCK_FRAME_LEN - 1] ^= 0x5A;
    TrckFix dropped;
    assert(!trck_unpack(bad, TRCK_FRAME_LEN, &dropped));

    size_t raw_len = 0;
    uint8_t *raw = read_file(argv[1], &raw_len);
    assert(raw_len % TRCK_FRAME_LEN == 0);
    size_t nframes = raw_len / TRCK_FRAME_LEN;
    assert(nframes == 14);

    TrckScan scan;
    trck_scan_init(&scan);
    TrckFix got[16];
    assert(feed_bytes(&scan, raw, raw_len, got, 16) == nframes);
    assert(scan.crc_fail == 0);
    assert(scan.resync == 0);
    assert(got[0].icao == 0x406B90u);
    assert(memcmp(got[0].callsign, "EZY85MH ", TRCK_CALLSIGN_LEN) == 0);
    assert(got[0].emergency == 0);
    assert(got[10].icao == 0x40058Bu);
    assert(got[10].emergency == 1);
    assert(got[12].icao == 0x4D224Fu);
    assert(memcmp(got[12].callsign, "        ", TRCK_CALLSIGN_LEN) == 0);
    assert(got[12].gs_kt == TRCK_SPEED_UNKNOWN);
    assert(got[12].track_deg == TRCK_TRACK_UNKNOWN);
    for (size_t i = 0; i < nframes; i++) {
        TrckFix fix;
        assert(trck_unpack(raw + i * TRCK_FRAME_LEN, TRCK_FRAME_LEN, &fix));
        assert(fix.icao == got[i].icao);
    }

    trck_scan_init(&scan);
    uint8_t noisy[2 + TRCK_FRAME_LEN];
    noisy[0] = 0x00;
    noisy[1] = 0xFF;
    memcpy(noisy + 2, kPublished, TRCK_FRAME_LEN);
    assert(feed_bytes(&scan, noisy, sizeof noisy, got, 16) == 1);
    assert(got[0].icao == 0x40621Du);
    assert(scan.resync == 2);
    assert(scan.crc_fail == 0);

    trck_scan_init(&scan);
    uint8_t then_good[TRCK_FRAME_LEN * 2];
    memcpy(then_good, bad, TRCK_FRAME_LEN);
    memcpy(then_good + TRCK_FRAME_LEN, kCheck2, TRCK_FRAME_LEN);
    size_t n = feed_bytes(&scan, then_good, sizeof then_good, got, 16);
    assert(n == 1);
    assert(got[0].icao == 0x4CA123u);
    assert(scan.crc_fail >= 1);

    free(raw);
    puts("ok");
    return 0;
}
