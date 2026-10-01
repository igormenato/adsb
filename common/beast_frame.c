#include "beast_frame.h"

#include "modes_crc.h"

#include <string.h>

static int payload_len_for(uint8_t type) {
    if (type == BEAST_TYPE_MODE_AC) {
        return 2;
    }
    if (type == BEAST_TYPE_MODE_S_SHORT) {
        return 7;
    }
    if (type == BEAST_TYPE_MODE_S_LONG) {
        return 14;
    }
    return -1;
}

int beast_encode(const beast_message_t *msg, uint8_t *out, int out_cap) {
    uint8_t raw[1 + 6 + 1 + 14];
    int plen;
    int raw_len;
    int i;
    int o;

    if (msg == 0 || out == 0) {
        return -1;
    }
    plen = payload_len_for(msg->type);
    if (plen < 0 || msg->payload_len != (uint8_t)plen) {
        return -1;
    }

    raw_len = 0;
    raw[raw_len++] = msg->type;
    memcpy(raw + raw_len, msg->mlat, 6);
    raw_len += 6;
    raw[raw_len++] = msg->signal;
    memcpy(raw + raw_len, msg->payload, (size_t)plen);
    raw_len += plen;

    o = 0;
    if (o >= out_cap) {
        return -1;
    }
    out[o++] = BEAST_ESC;
    for (i = 0; i < raw_len; i++) {
        if (o >= out_cap) {
            return -1;
        }
        out[o++] = raw[i];
        if (raw[i] == BEAST_ESC) {
            if (o >= out_cap) {
                return -1;
            }
            out[o++] = BEAST_ESC;
        }
    }
    return o;
}

void beast_parser_init(beast_parser_t *parser) {
    memset(parser, 0, sizeof(*parser));
}

static void finish_frame(beast_parser_t *parser, beast_message_t *out) {
    memset(out, 0, sizeof(*out));
    out->type = parser->data[0];
    memcpy(out->mlat, parser->data + 1, 6);
    out->signal = parser->data[7];
    out->payload_len = (uint8_t)(parser->data_expect - 8);
    memcpy(out->payload, parser->data + 8, out->payload_len);
    out->crc_ok = -1;
    if (out->payload_len == 14) {
        uint8_t df = (uint8_t)(out->payload[0] >> 3);
        if (df == 17 || df == 18) {
            out->crc_ok = (modes_crc24(out->payload, 14) == 0) ? 1 : 0;
        }
    }
    parser->frames++;
    parser->state = 0;
    parser->data_len = 0;
    parser->data_expect = 0;
}

int beast_parser_byte(beast_parser_t *parser, uint8_t byte, beast_message_t *out) {
    /* state 0 hunt, 1 type, 2 data, 3 data-escape */
    if (parser->state == 0) {
        if (byte == BEAST_ESC) {
            parser->state = 1;
        }
        return BEAST_NEED_MORE;
    }

    if (parser->state == 1) {
        int plen = payload_len_for(byte);
        if (plen >= 0) {
            parser->data[0] = byte;
            parser->data_len = 1;
            /* type + mlat + signal + payload */
            parser->data_expect = 1 + 6 + 1 + plen;
            parser->state = 2;
            return BEAST_NEED_MORE;
        }
        if (byte == BEAST_ESC) {
            /* Previous 0x1A was not a frame start. This one might be. */
            return BEAST_NEED_MORE;
        }
        parser->state = 0;
        return BEAST_RETRY;
    }

    if (parser->state == 3) {
        parser->state = 2;
        if (byte != BEAST_ESC) {
            parser->framing_errors++;
            parser->state = 1;
            parser->data_len = 0;
            parser->data_expect = 0;
            return beast_parser_byte(parser, byte, out);
        }
        byte = BEAST_ESC;
    } else if (byte == BEAST_ESC) {
        parser->state = 3;
        return BEAST_NEED_MORE;
    }

    if (parser->data_len >= (int)sizeof(parser->data)) {
        parser->framing_errors++;
        parser->state = 0;
        parser->data_len = 0;
        parser->data_expect = 0;
        return BEAST_BROKEN;
    }
    parser->data[parser->data_len++] = byte;
    if (parser->data_len < parser->data_expect) {
        return BEAST_NEED_MORE;
    }
    if (out == 0) {
        parser->state = 0;
        parser->data_len = 0;
        parser->data_expect = 0;
        return BEAST_BROKEN;
    }
    finish_frame(parser, out);
    return BEAST_FRAME;
}

int beast_address(const beast_message_t *msg, uint32_t *addr) {
    uint8_t df;

    if (msg == 0 || addr == 0 || msg->payload_len < 7) {
        return 0;
    }
    df = (uint8_t)(msg->payload[0] >> 3);
    if (df != 11 && df != 17 && df != 18) {
        return 0;
    }
    *addr = ((uint32_t)msg->payload[1] << 16) | ((uint32_t)msg->payload[2] << 8) |
            (uint32_t)msg->payload[3];
    return 1;
}
