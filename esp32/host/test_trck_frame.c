#include "trck_frame.h"

#include <assert.h>
#include <stdio.h>
#include <string.h>

/* Same bytes as tests/host.rs SECOND / check_positions()[0]. */
static const uint8_t k_check0[TRCK_FRAME_LEN] = {
    0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0xbc, 0x0a, 0x40, 0x40, 0x2c, 0x42, 0x06, 0x60,
    0x18, 0xee, 0xf3, 0xe0, 0x2e, 0x00, 0x00, 0x18, 0x01, 0x5a, 0x00, 0x00, 0xd7, 0x48,
};

/* docs/esp32-alert.md published example / check_positions()[1]. */
static const uint8_t k_check1[TRCK_FRAME_LEN] = {
    0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0x1d, 0x62, 0x40, 0xf5, 0xd0, 0x25, 0x1f, 0x7e,
    0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xc2, 0x01, 0x0f, 0x01, 0x00, 0x73, 0x81,
};

/* check_positions()[2]: 4CA123, general. */
static const uint8_t k_check2[TRCK_FRAME_LEN] = {
    0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0x23, 0xa1, 0x4c, 0xe0, 0xb2, 0xad, 0x1e, 0xe0,
    0x55, 0xbb, 0xff, 0x40, 0x1f, 0x00, 0x00, 0xdc, 0x00, 0xb4, 0x00, 0x01, 0x7f, 0xb6,
};

static const uint8_t k_unknown[TRCK_FRAME_LEN] = {
    0x54, 0x52, 0x43, 0x4b, 0x0a, 0x00, 0x00, 0x00, 0x23, 0xc1, 0xab, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0xff, 0xff, 0xff, 0xff, 0x00, 0x2a, 0x6a,
};

static void expect_fix(const uint8_t *frame, uint32_t icao, int32_t lat, int32_t lon, int32_t alt,
                       uint8_t emergency) {
    trck_fix_t fix;
    assert(trck_unpack(frame, TRCK_FRAME_LEN, &fix));
    assert(fix.unix_s == 1700000000u);
    assert(fix.icao == icao);
    assert(fix.lat_e7 == lat);
    assert(fix.lon_e7 == lon);
    assert(fix.alt_ft == alt);
    assert(fix.emergency == emergency);
}

static size_t feed_bytes(trck_scan_t *scan, const uint8_t *bytes, size_t len, trck_fix_t *got,
                         size_t cap) {
    size_t n = 0;
    for (size_t i = 0; i < len; i++) {
        trck_fix_t fix;
        if (trck_scan_feed(scan, bytes[i], &fix)) {
            assert(n < cap);
            got[n++] = fix;
        }
    }
    return n;
}

int main(void) {
    assert(trck_crc16((const uint8_t *)"123456789", 9) == 0x29B1);

    expect_fix(k_check0, 0x400ABCu, 105000000, -202500000, 12000, 0);
    expect_fix(k_check1, 0x40621Du, 522572021, 39193726, 38000, 0);
    expect_fix(k_check2, 0x4CA123u, 514700000, -4500000, 8000, 1);

    trck_fix_t unknown;
    assert(trck_unpack(k_unknown, TRCK_FRAME_LEN, &unknown));
    assert(unknown.unix_s == 10);
    assert(unknown.icao == 0xABC123u);
    assert(unknown.alt_ft == TRCK_ALT_UNKNOWN);
    assert(unknown.gs_kt == TRCK_SPEED_UNKNOWN);
    assert(unknown.track_deg == TRCK_TRACK_UNKNOWN);
    assert(unknown.emergency == 0);

    uint8_t bad[TRCK_FRAME_LEN];
    memcpy(bad, k_check1, TRCK_FRAME_LEN);
    bad[29] ^= 0x5A;
    trck_fix_t dropped;
    assert(!trck_unpack(bad, TRCK_FRAME_LEN, &dropped));

    trck_scan_t scan;
    trck_scan_init(&scan);
    trck_fix_t got[8];
    uint8_t burst[TRCK_FRAME_LEN * 3];
    memcpy(burst, k_check0, TRCK_FRAME_LEN);
    memcpy(burst + TRCK_FRAME_LEN, k_check1, TRCK_FRAME_LEN);
    memcpy(burst + TRCK_FRAME_LEN * 2, k_check2, TRCK_FRAME_LEN);
    assert(feed_bytes(&scan, burst, sizeof burst, got, 8) == 3);
    assert(got[0].icao == 0x400ABCu && got[0].emergency == 0);
    assert(got[1].icao == 0x40621Du && got[1].lat_e7 == 522572021);
    assert(got[2].icao == 0x4CA123u && got[2].emergency == 1);

    trck_scan_init(&scan);
    uint8_t noisy[2 + TRCK_FRAME_LEN];
    noisy[0] = 0x00;
    noisy[1] = 0xFF;
    memcpy(noisy + 2, k_check0, TRCK_FRAME_LEN);
    assert(feed_bytes(&scan, noisy, sizeof noisy, got, 8) == 1);
    assert(got[0].icao == 0x400ABCu);

    trck_scan_init(&scan);
    uint8_t split[3 + TRCK_FRAME_LEN];
    split[0] = 'T';
    split[1] = 'R';
    split[2] = 'C';
    memcpy(split + 3, k_check1, TRCK_FRAME_LEN);
    assert(feed_bytes(&scan, split, sizeof split, got, 8) == 1);
    assert(got[0].icao == 0x40621Du);

    trck_scan_init(&scan);
    uint8_t then_good[TRCK_FRAME_LEN * 2];
    memcpy(then_good, bad, TRCK_FRAME_LEN);
    memcpy(then_good + TRCK_FRAME_LEN, k_check2, TRCK_FRAME_LEN);
    size_t n = feed_bytes(&scan, then_good, sizeof then_good, got, 8);
    assert(n == 1);
    assert(got[0].icao == 0x4CA123u);

    puts("ok");
    return 0;
}
