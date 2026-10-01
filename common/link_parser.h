#ifndef ADSB_LINK_PARSER_H
#define ADSB_LINK_PARSER_H

#include "beast_frame.h"
#include "struct_frame.h"

#ifdef __cplusplus
extern "C" {
#endif

enum {
    LINK_ERR_BEAST_FRAMING = 1,
    LINK_ERR_STRUCT_CHECKSUM = 2,
    LINK_ERR_STRUCT_VERSION = 3
};

typedef struct {
    uint32_t beast_ok;
    uint32_t beast_crc_bad;
    uint32_t beast_framing_errors;
    uint32_t struct_ok;
    uint32_t struct_checksum_errors;
    uint32_t struct_version_errors;
    uint32_t junk_bytes;
} link_stats_t;

typedef void (*link_beast_cb)(const beast_message_t *msg, void *user);
typedef void (*link_struct_cb)(const adsb_struct_t *msg, void *user);
typedef void (*link_err_cb)(int code, void *user);

typedef struct {
    int mode;
    beast_parser_t beast;
    uint8_t window[ADSB_STRUCT_SIZE];
    int filled;
    uint8_t replay[128];
    int replay_len;
    int replay_i;
    link_stats_t stats;
    link_beast_cb on_beast;
    link_struct_cb on_struct;
    link_err_cb on_error;
    void *user;
} link_parser_t;

void link_parser_init(link_parser_t *parser, link_beast_cb on_beast,
                      link_struct_cb on_struct, link_err_cb on_error, void *user);
void link_parser_feed(link_parser_t *parser, const uint8_t *data, int len);

#ifdef __cplusplus
}
#endif

#endif
