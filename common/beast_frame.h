#ifndef ADSB_BEAST_FRAME_H
#define ADSB_BEAST_FRAME_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define BEAST_ESC 0x1Au
#define BEAST_TYPE_MODE_AC 0x31u
#define BEAST_TYPE_MODE_S_SHORT 0x32u
#define BEAST_TYPE_MODE_S_LONG 0x33u

enum {
    BEAST_NEED_MORE = 0,
    BEAST_FRAME = 1,
    /* Byte was not consumed. State is back at hunt. */
    BEAST_RETRY = -1,
    /* A started frame was abandoned. Byte was not consumed. */
    BEAST_BROKEN = -2
};

typedef struct {
    uint8_t type;
    uint8_t mlat[6];
    uint8_t signal;
    uint8_t payload[14];
    uint8_t payload_len;
    int8_t crc_ok; /* 1 valid DF17/18, 0 bad DF17/18, -1 not checked */
} beast_message_t;

typedef struct {
    int state;
    uint8_t data[22];
    int data_len;
    int data_expect;
    uint32_t framing_errors;
    uint32_t frames;
} beast_parser_t;

/* Escape and write one Beast frame. Returns the wire length, or -1. */
int beast_encode(const beast_message_t *msg, uint8_t *out, int out_cap);

void beast_parser_init(beast_parser_t *parser);
int beast_parser_byte(beast_parser_t *parser, uint8_t byte, beast_message_t *out);

/* DF11/17/18 address. Returns 1 and writes the low 24 bits, else 0. */
int beast_address(const beast_message_t *msg, uint32_t *addr);

#ifdef __cplusplus
}
#endif

#endif
