#include "link_parser.h"

#include <string.h>

enum {
    MODE_HUNT = 0,
    MODE_BEAST = 1,
    MODE_STRUCT_MAGIC = 2,
    MODE_STRUCT_BODY = 3
};

void link_parser_init(link_parser_t *parser, link_beast_cb on_beast,
                      link_struct_cb on_struct, link_err_cb on_error, void *user) {
    memset(parser, 0, sizeof(*parser));
    beast_parser_init(&parser->beast);
    parser->on_beast = on_beast;
    parser->on_struct = on_struct;
    parser->on_error = on_error;
    parser->user = user;
}

static void replay_now(link_parser_t *parser, const uint8_t *bytes, int n) {
    int left = parser->replay_len - parser->replay_i;
    uint8_t tmp[128];

    if (n < 0) {
        n = 0;
    }
    if (n + left > (int)sizeof(tmp)) {
        parser->stats.beast_framing_errors++;
        parser->replay_len = 0;
        parser->replay_i = 0;
        parser->mode = MODE_HUNT;
        return;
    }
    if (n > 0) {
        memcpy(tmp, bytes, (size_t)n);
    }
    if (left > 0) {
        memcpy(tmp + n, parser->replay + parser->replay_i, (size_t)left);
    }
    memcpy(parser->replay, tmp, (size_t)(n + left));
    parser->replay_len = n + left;
    parser->replay_i = 0;
}

static void fail_struct(link_parser_t *parser, int code) {
    uint8_t tail[ADSB_STRUCT_SIZE - 1];
    int tail_len = parser->filled - 1;

    if (code == LINK_ERR_STRUCT_CHECKSUM) {
        parser->stats.struct_checksum_errors++;
    } else if (code == LINK_ERR_STRUCT_VERSION) {
        parser->stats.struct_version_errors++;
    }
    if (parser->on_error != 0) {
        parser->on_error(code, parser->user);
    }
    if (tail_len < 0) {
        tail_len = 0;
    }
    if (tail_len > 0) {
        memcpy(tail, parser->window + 1, (size_t)tail_len);
    }
    parser->filled = 0;
    parser->mode = MODE_HUNT;
    if (tail_len > 0) {
        replay_now(parser, tail, tail_len);
    }
}

static void link_step(link_parser_t *parser, uint8_t byte) {
    if (parser->mode == MODE_BEAST) {
        beast_message_t msg;
        uint32_t framing_before = parser->beast.framing_errors;
        int rc = beast_parser_byte(&parser->beast, byte, &msg);
        if (parser->beast.framing_errors != framing_before) {
            parser->stats.beast_framing_errors +=
                parser->beast.framing_errors - framing_before;
            if (parser->on_error != 0) {
                parser->on_error(LINK_ERR_BEAST_FRAMING, parser->user);
            }
        }
        if (rc == BEAST_FRAME) {
            parser->stats.beast_ok++;
            if (msg.crc_ok == 0) {
                parser->stats.beast_crc_bad++;
            }
            if (parser->on_beast != 0) {
                parser->on_beast(&msg, parser->user);
            }
            parser->mode = MODE_HUNT;
            return;
        }
        if (rc == BEAST_BROKEN) {
            /* framing_errors was already folded into stats above. */
            parser->mode = MODE_HUNT;
            replay_now(parser, &byte, 1);
            return;
        }
        if (rc == BEAST_RETRY) {
            parser->mode = MODE_HUNT;
            replay_now(parser, &byte, 1);
        }
        return;
    }

    if (parser->mode == MODE_STRUCT_MAGIC) {
        if (byte == ADSB_WIRE_MAGIC_1) {
            parser->window[0] = ADSB_WIRE_MAGIC_0;
            parser->window[1] = ADSB_WIRE_MAGIC_1;
            parser->filled = 2;
            parser->mode = MODE_STRUCT_BODY;
            return;
        }
        parser->stats.junk_bytes++;
        parser->mode = MODE_HUNT;
        replay_now(parser, &byte, 1);
        return;
    }

    if (parser->mode == MODE_STRUCT_BODY) {
        adsb_struct_t decoded;
        int rc;

        parser->window[parser->filled++] = byte;
        if (parser->filled < (int)ADSB_STRUCT_SIZE) {
            return;
        }
        rc = struct_decode(parser->window, &decoded);
        if (rc == STRUCT_OK) {
            parser->filled = 0;
            parser->mode = MODE_HUNT;
            parser->stats.struct_ok++;
            if (parser->on_struct != 0) {
                parser->on_struct(&decoded, parser->user);
            }
            return;
        }
        if (rc == STRUCT_ERR_VERSION) {
            fail_struct(parser, LINK_ERR_STRUCT_VERSION);
        } else {
            fail_struct(parser, LINK_ERR_STRUCT_CHECKSUM);
        }
        return;
    }

    if (byte == BEAST_ESC) {
        beast_message_t ignored;
        beast_parser_init(&parser->beast);
        parser->mode = MODE_BEAST;
        (void)beast_parser_byte(&parser->beast, byte, &ignored);
        return;
    }
    if (byte == ADSB_WIRE_MAGIC_0) {
        parser->mode = MODE_STRUCT_MAGIC;
        return;
    }
    parser->stats.junk_bytes++;
}

void link_parser_feed(link_parser_t *parser, const uint8_t *data, int len) {
    int index = 0;
    int guard = 0;

    if (parser == 0 || (data == 0 && len > 0) || len < 0) {
        return;
    }
    while ((index < len || parser->replay_i < parser->replay_len) && guard < 1000000) {
        uint8_t byte;
        guard++;
        if (parser->replay_i < parser->replay_len) {
            byte = parser->replay[parser->replay_i++];
        } else {
            byte = data[index++];
        }
        link_step(parser, byte);
    }
    if (guard >= 1000000) {
        parser->stats.beast_framing_errors++;
        parser->replay_i = 0;
        parser->replay_len = 0;
        parser->mode = MODE_HUNT;
    }
}
