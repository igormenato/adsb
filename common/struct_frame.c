#include "struct_frame.h"

#include "crc16.h"

static void write_u16(uint8_t *p, uint16_t v) {
    p[0] = (uint8_t)(v & 0xFFu);
    p[1] = (uint8_t)((v >> 8) & 0xFFu);
}

static void write_u32(uint8_t *p, uint32_t v) {
    p[0] = (uint8_t)(v & 0xFFu);
    p[1] = (uint8_t)((v >> 8) & 0xFFu);
    p[2] = (uint8_t)((v >> 16) & 0xFFu);
    p[3] = (uint8_t)((v >> 24) & 0xFFu);
}

static void write_u64(uint8_t *p, uint64_t v) {
    int i;
    for (i = 0; i < 8; i++) {
        p[i] = (uint8_t)((v >> (8 * i)) & 0xFFu);
    }
}

static uint16_t read_u16(const uint8_t *p) {
    return (uint16_t)p[0] | ((uint16_t)p[1] << 8);
}

static uint32_t read_u32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) |
           ((uint32_t)p[3] << 24);
}

static uint64_t read_u64(const uint8_t *p) {
    uint64_t v = 0;
    int i;
    for (i = 0; i < 8; i++) {
        v |= (uint64_t)p[i] << (8 * i);
    }
    return v;
}

int struct_encode(const adsb_struct_t *msg, uint8_t out[ADSB_STRUCT_SIZE]) {
    uint16_t crc;

    if (msg == 0 || out == 0) {
        return STRUCT_ERR_RANGE;
    }
    if (msg->version != ADSB_STRUCT_VERSION) {
        return STRUCT_ERR_VERSION;
    }

    write_u16(out + 0, ADSB_STRUCT_MAGIC);
    out[2] = ADSB_STRUCT_VERSION;
    out[3] = msg->flags;
    write_u32(out + 4, msg->icao & 0x00FFFFFFu);
    write_u32(out + 8, (uint32_t)msg->latitude_e7);
    write_u32(out + 12, (uint32_t)msg->longitude_e7);
    write_u32(out + 16, (uint32_t)msg->altitude_ft);
    write_u16(out + 20, msg->velocity_kt);
    write_u64(out + 22, msg->timestamp_us);
    crc = crc16_ccitt_false(out, ADSB_STRUCT_CRC_LEN);
    write_u16(out + 30, crc);
    return STRUCT_OK;
}

int struct_decode(const uint8_t in[ADSB_STRUCT_SIZE], adsb_struct_t *out) {
    uint16_t expect;
    uint16_t got;

    if (in == 0 || out == 0) {
        return STRUCT_ERR_RANGE;
    }
    if (in[0] != ADSB_WIRE_MAGIC_0 || in[1] != ADSB_WIRE_MAGIC_1) {
        return STRUCT_ERR_MAGIC;
    }
    expect = crc16_ccitt_false(in, ADSB_STRUCT_CRC_LEN);
    got = read_u16(in + 30);
    if (expect != got) {
        return STRUCT_ERR_CHECKSUM;
    }
    if (in[2] != ADSB_STRUCT_VERSION) {
        return STRUCT_ERR_VERSION;
    }

    out->magic = ADSB_STRUCT_MAGIC;
    out->version = in[2];
    out->flags = in[3];
    out->icao = read_u32(in + 4) & 0x00FFFFFFu;
    out->latitude_e7 = (int32_t)read_u32(in + 8);
    out->longitude_e7 = (int32_t)read_u32(in + 12);
    out->altitude_ft = (int32_t)read_u32(in + 16);
    out->velocity_kt = read_u16(in + 20);
    out->timestamp_us = read_u64(in + 22);
    out->checksum = got;
    return STRUCT_OK;
}
