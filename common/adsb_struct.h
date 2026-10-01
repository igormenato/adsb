#ifndef ADSB_STRUCT_H
#define ADSB_STRUCT_H

/*
 * Packed ADS-B track message. This header is the layout source of truth.
 *
 * Wire format: 32 bytes, little-endian, no padding. The checksum is
 * CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF, xorout 0x0000, not reflected)
 * over the first 30 bytes. Python in pi/struct_frame.py uses "<HBBIiiiHQH"
 * and must match these offsets; test/test_bench.py checks that.
 *
 * One message is one decoded ADS-B extended squitter, not a fused track.
 * Flags tell the receiver which kinematic fields this squitter carried.
 * Cleared fields use the sentinels below (0 is a valid latitude, altitude,
 * and a valid east/north component, so it is not a sentinel).
 *
 *   latitude_e7, longitude_e7   degrees * 1e7
 *   altitude_ft                 feet (baro, or GNSS height converted to feet)
 *   velocity_kt                 ground speed in knots
 *   timestamp_us                Unix epoch microseconds, set by the Pi
 *   icao                        ADS-B address in the low 24 bits
 */

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define ADSB_STRUCT_MAGIC 0xAD5Bu
#define ADSB_STRUCT_VERSION 1u
#define ADSB_STRUCT_SIZE 32u
#define ADSB_STRUCT_CRC_LEN 30u

#define ADSB_WIRE_MAGIC_0 ((uint8_t)(ADSB_STRUCT_MAGIC & 0xFFu))
#define ADSB_WIRE_MAGIC_1 ((uint8_t)((ADSB_STRUCT_MAGIC >> 8) & 0xFFu))

#define ADSB_FLAG_POSITION 0x01u
#define ADSB_FLAG_ALTITUDE 0x02u
#define ADSB_FLAG_VELOCITY 0x04u

#define ADSB_LATLON_INVALID ((int32_t)0x80000000)
#define ADSB_ALT_INVALID ((int32_t)0x80000000)
#define ADSB_VEL_INVALID ((uint16_t)0xFFFFu)

#if defined(__cplusplus)
#define ADSB_STATIC_ASSERT(cond, msg) static_assert(cond, msg)
#else
#define ADSB_STATIC_ASSERT(cond, msg) _Static_assert(cond, msg)
#endif

typedef struct __attribute__((packed)) {
    uint16_t magic;
    uint8_t version;
    uint8_t flags;
    uint32_t icao;
    int32_t latitude_e7;
    int32_t longitude_e7;
    int32_t altitude_ft;
    uint16_t velocity_kt;
    uint64_t timestamp_us;
    uint16_t checksum;
} adsb_struct_t;

ADSB_STATIC_ASSERT(sizeof(adsb_struct_t) == ADSB_STRUCT_SIZE, "adsb_struct size");
ADSB_STATIC_ASSERT(offsetof(adsb_struct_t, magic) == 0, "magic offset");
ADSB_STATIC_ASSERT(offsetof(adsb_struct_t, version) == 2, "version offset");
ADSB_STATIC_ASSERT(offsetof(adsb_struct_t, flags) == 3, "flags offset");
ADSB_STATIC_ASSERT(offsetof(adsb_struct_t, icao) == 4, "icao offset");
ADSB_STATIC_ASSERT(offsetof(adsb_struct_t, latitude_e7) == 8, "lat offset");
ADSB_STATIC_ASSERT(offsetof(adsb_struct_t, longitude_e7) == 12, "lon offset");
ADSB_STATIC_ASSERT(offsetof(adsb_struct_t, altitude_ft) == 16, "alt offset");
ADSB_STATIC_ASSERT(offsetof(adsb_struct_t, velocity_kt) == 20, "vel offset");
ADSB_STATIC_ASSERT(offsetof(adsb_struct_t, timestamp_us) == 22, "timestamp offset");
ADSB_STATIC_ASSERT(offsetof(adsb_struct_t, checksum) == 30, "checksum offset");

#ifdef __cplusplus
}
#endif

#endif
