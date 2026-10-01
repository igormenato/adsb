/* Host-side driver for the shared Beast and struct framers.
   The Python test compiles this and checks that both languages agree. */

#include "beast_frame.h"
#include "crc16.h"
#include "link_parser.h"
#include "modes_crc.h"
#include "struct_frame.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int hex_nibble(int ch) {
    if (ch >= '0' && ch <= '9') {
        return ch - '0';
    }
    if (ch >= 'a' && ch <= 'f') {
        return ch - 'a' + 10;
    }
    if (ch >= 'A' && ch <= 'F') {
        return ch - 'A' + 10;
    }
    return -1;
}

static int read_hex_stdin(uint8_t **out, size_t *out_len) {
    size_t cap = 256;
    size_t len = 0;
    int high = -1;
    int ch;
    uint8_t *buf = (uint8_t *)malloc(cap);

    if (buf == 0) {
        return -1;
    }
    while ((ch = getchar()) != EOF) {
        int nibble;
        if (ch == ' ' || ch == '\n' || ch == '\r' || ch == '\t') {
            continue;
        }
        nibble = hex_nibble(ch);
        if (nibble < 0) {
            fprintf(stderr, "bad hex character\n");
            free(buf);
            return -1;
        }
        if (high < 0) {
            high = nibble;
            continue;
        }
        if (len == cap) {
            size_t next = cap * 2;
            uint8_t *grown = (uint8_t *)realloc(buf, next);
            if (grown == 0) {
                free(buf);
                return -1;
            }
            buf = grown;
            cap = next;
        }
        buf[len++] = (uint8_t)((high << 4) | nibble);
        high = -1;
    }
    if (high >= 0) {
        fprintf(stderr, "odd number of hex digits\n");
        free(buf);
        return -1;
    }
    *out = buf;
    *out_len = len;
    return 0;
}

static void print_hex(const uint8_t *data, int len) {
    int i;
    for (i = 0; i < len; i++) {
        printf("%02x", data[i]);
    }
    printf("\n");
}

static void print_beast(const beast_message_t *msg) {
    int i;
    printf("B type=%c mlat=", (char)msg->type);
    for (i = 0; i < 6; i++) {
        printf("%02x", msg->mlat[i]);
    }
    printf(" signal=%02x payload=", msg->signal);
    for (i = 0; i < msg->payload_len; i++) {
        printf("%02x", msg->payload[i]);
    }
    printf(" crc=%d\n", (int)msg->crc_ok);
}

static void print_struct(const adsb_struct_t *msg) {
    printf("S flags=%02x icao=%06x lat=%ld lon=%ld alt=%ld vel=%u ts=%llu\n",
           msg->flags, msg->icao, (long)msg->latitude_e7, (long)msg->longitude_e7,
           (long)msg->altitude_ft, (unsigned)msg->velocity_kt,
           (unsigned long long)msg->timestamp_us);
}

static int beast_decode_buffer(const uint8_t *data, size_t len) {
    beast_parser_t parser;
    size_t i;

    beast_parser_init(&parser);
    for (i = 0; i < len; i++) {
        int spins = 0;
        for (;;) {
            beast_message_t msg;
            int rc = beast_parser_byte(&parser, data[i], &msg);
            if (rc == BEAST_FRAME) {
                print_beast(&msg);
                break;
            }
            if (rc == BEAST_RETRY || rc == BEAST_BROKEN) {
                if (++spins > 4) {
                    fprintf(stderr, "beast parser stuck\n");
                    return 1;
                }
                continue;
            }
            break;
        }
    }
    return 0;
}

static void on_beast(const beast_message_t *msg, void *user) {
    (void)user;
    print_beast(msg);
}

static void on_struct(const adsb_struct_t *msg, void *user) {
    (void)user;
    print_struct(msg);
}

static void on_error(int code, void *user) {
    (void)user;
    if (code == LINK_ERR_BEAST_FRAMING) {
        printf("E beast_framing\n");
    } else if (code == LINK_ERR_STRUCT_CHECKSUM) {
        printf("E struct_checksum\n");
    } else if (code == LINK_ERR_STRUCT_VERSION) {
        printf("E struct_version\n");
    }
}

static int cmd_layout(void) {
    printf("size %u\n", (unsigned)sizeof(adsb_struct_t));
    printf("magic %u\n", (unsigned)offsetof(adsb_struct_t, magic));
    printf("version %u\n", (unsigned)offsetof(adsb_struct_t, version));
    printf("flags %u\n", (unsigned)offsetof(adsb_struct_t, flags));
    printf("icao %u\n", (unsigned)offsetof(adsb_struct_t, icao));
    printf("latitude_e7 %u\n", (unsigned)offsetof(adsb_struct_t, latitude_e7));
    printf("longitude_e7 %u\n", (unsigned)offsetof(adsb_struct_t, longitude_e7));
    printf("altitude_ft %u\n", (unsigned)offsetof(adsb_struct_t, altitude_ft));
    printf("velocity_kt %u\n", (unsigned)offsetof(adsb_struct_t, velocity_kt));
    printf("timestamp_us %u\n", (unsigned)offsetof(adsb_struct_t, timestamp_us));
    printf("checksum %u\n", (unsigned)offsetof(adsb_struct_t, checksum));
    printf("magic_value %04x\n", (unsigned)ADSB_STRUCT_MAGIC);
    printf("version_value %u\n", (unsigned)ADSB_STRUCT_VERSION);
    printf("wire0 %02x\n", ADSB_WIRE_MAGIC_0);
    printf("wire1 %02x\n", ADSB_WIRE_MAGIC_1);
    return 0;
}

static int parse_hex(const char *text, uint8_t *out, int out_cap) {
    int n = 0;
    int high = -1;
    const char *p;

    for (p = text; *p != 0; p++) {
        int nibble;
        if (*p == ' ' || *p == ':') {
            continue;
        }
        nibble = hex_nibble((unsigned char)*p);
        if (nibble < 0) {
            return -1;
        }
        if (high < 0) {
            high = nibble;
            continue;
        }
        if (n >= out_cap) {
            return -1;
        }
        out[n++] = (uint8_t)((high << 4) | nibble);
        high = -1;
    }
    if (high >= 0) {
        return -1;
    }
    return n;
}

static int cmd_beast_encode(int argc, char **argv) {
    beast_message_t msg;
    uint8_t wire[64];
    int plen;
    int n;

    if (argc != 6) {
        fprintf(stderr, "usage: frame_tool beast-encode TYPE MLAT SIGNAL PAYLOAD\n");
        return 1;
    }
    memset(&msg, 0, sizeof(msg));
    if (argv[2][0] == 0 || argv[2][1] != 0) {
        fprintf(stderr, "type must be 1, 2, or 3\n");
        return 1;
    }
    msg.type = (uint8_t)argv[2][0];
    if (parse_hex(argv[3], msg.mlat, 6) != 6) {
        fprintf(stderr, "mlat must be 6 bytes\n");
        return 1;
    }
    if (parse_hex(argv[4], &msg.signal, 1) != 1) {
        fprintf(stderr, "signal must be 1 byte\n");
        return 1;
    }
    plen = parse_hex(argv[5], msg.payload, 14);
    if (plen < 0) {
        fprintf(stderr, "bad payload hex\n");
        return 1;
    }
    msg.payload_len = (uint8_t)plen;
    n = beast_encode(&msg, wire, (int)sizeof(wire));
    if (n < 0) {
        fprintf(stderr, "beast encode failed\n");
        return 1;
    }
    print_hex(wire, n);
    return 0;
}

static int cmd_beast_decode(void) {
    uint8_t *buf = 0;
    size_t len = 0;
    int rc;

    if (read_hex_stdin(&buf, &len) != 0) {
        return 1;
    }
    rc = beast_decode_buffer(buf, len);
    free(buf);
    return rc;
}

static int cmd_struct_encode(int argc, char **argv) {
    adsb_struct_t msg;
    uint8_t wire[ADSB_STRUCT_SIZE];

    if (argc != 9) {
        fprintf(stderr, "usage: frame_tool struct-encode FLAGS ICAO LAT LON ALT VEL TS\n");
        return 1;
    }
    memset(&msg, 0, sizeof(msg));
    msg.version = ADSB_STRUCT_VERSION;
    msg.flags = (uint8_t)strtoul(argv[2], 0, 0);
    msg.icao = (uint32_t)strtoul(argv[3], 0, 0);
    msg.latitude_e7 = (int32_t)strtol(argv[4], 0, 0);
    msg.longitude_e7 = (int32_t)strtol(argv[5], 0, 0);
    msg.altitude_ft = (int32_t)strtol(argv[6], 0, 0);
    msg.velocity_kt = (uint16_t)strtoul(argv[7], 0, 0);
    msg.timestamp_us = (uint64_t)strtoull(argv[8], 0, 0);
    if (struct_encode(&msg, wire) != STRUCT_OK) {
        fprintf(stderr, "struct encode failed\n");
        return 1;
    }
    print_hex(wire, (int)ADSB_STRUCT_SIZE);
    return 0;
}

static int cmd_struct_decode(void) {
    uint8_t *buf = 0;
    size_t len = 0;
    adsb_struct_t msg;
    int rc;

    if (read_hex_stdin(&buf, &len) != 0) {
        return 1;
    }
    if (len != ADSB_STRUCT_SIZE) {
        fprintf(stderr, "ERR length\n");
        free(buf);
        return 1;
    }
    rc = struct_decode(buf, &msg);
    free(buf);
    if (rc == STRUCT_OK) {
        print_struct(&msg);
        return 0;
    }
    if (rc == STRUCT_ERR_VERSION) {
        fprintf(stderr, "ERR version\n");
    } else if (rc == STRUCT_ERR_MAGIC) {
        fprintf(stderr, "ERR magic\n");
    } else {
        fprintf(stderr, "ERR checksum\n");
    }
    return 1;
}

static int cmd_link_decode(void) {
    uint8_t *buf = 0;
    size_t len = 0;
    link_parser_t parser;

    if (read_hex_stdin(&buf, &len) != 0) {
        return 1;
    }
    link_parser_init(&parser, on_beast, on_struct, on_error, 0);
    link_parser_feed(&parser, buf, (int)len);
    printf("STATS beast_ok=%u beast_crc_bad=%u beast_framing=%u struct_ok=%u "
           "struct_checksum=%u struct_version=%u junk=%u\n",
           parser.stats.beast_ok, parser.stats.beast_crc_bad, parser.stats.beast_framing_errors,
           parser.stats.struct_ok, parser.stats.struct_checksum_errors,
           parser.stats.struct_version_errors, parser.stats.junk_bytes);
    free(buf);
    return 0;
}

static int expect_eq(const char *name, unsigned long long got, unsigned long long want) {
    if (got != want) {
        fprintf(stderr, "%s: got %llu want %llu\n", name, got, want);
        return 1;
    }
    return 0;
}

static int self_check(void) {
    static const uint8_t squitter[] = {0x8D, 0x40, 0x62, 0x1D, 0x58, 0xC3, 0x82, 0xD6,
                                       0x90, 0xC8, 0xAC, 0x28, 0x63, 0xA7};
    static const uint8_t beast_wire[] = {0x1A, 0x32, 0x08, 0x3E, 0x27, 0xB6, 0xCB, 0x6A, 0x1A,
                                         0x1A, 0x00, 0xA1, 0x84, 0x1A, 0x1A, 0xC3, 0xB3, 0x1D};
    beast_message_t msg;
    beast_parser_t parser;
    adsb_struct_t packed;
    adsb_struct_t decoded;
    uint8_t wire[64];
    uint8_t struct_wire[ADSB_STRUCT_SIZE];
    int i;
    int rc;
    int failures = 0;

    failures += expect_eq("crc16", crc16_ccitt_false((const uint8_t *)"123456789", 9), 0x29B1);
    failures += expect_eq("modes", modes_crc24(squitter, 14), 0);
    failures += expect_eq("size", sizeof(adsb_struct_t), 32);
    failures += expect_eq("ts_off", offsetof(adsb_struct_t, timestamp_us), 22);

    memset(&msg, 0, sizeof(msg));
    msg.type = BEAST_TYPE_MODE_S_SHORT;
    msg.mlat[0] = 0x08;
    msg.mlat[1] = 0x3e;
    msg.mlat[2] = 0x27;
    msg.mlat[3] = 0xb6;
    msg.mlat[4] = 0xcb;
    msg.mlat[5] = 0x6a;
    msg.signal = 0x1a;
    msg.payload_len = 7;
    msg.payload[0] = 0x00;
    msg.payload[1] = 0xa1;
    msg.payload[2] = 0x84;
    msg.payload[3] = 0x1a;
    msg.payload[4] = 0xc3;
    msg.payload[5] = 0xb3;
    msg.payload[6] = 0x1d;
    rc = beast_encode(&msg, wire, (int)sizeof(wire));
    if (rc != (int)sizeof(beast_wire) || memcmp(wire, beast_wire, sizeof(beast_wire)) != 0) {
        fprintf(stderr, "beast encode mismatch\n");
        failures++;
    }

    beast_parser_init(&parser);
    rc = 0;
    for (i = 0; i < (int)sizeof(beast_wire); i++) {
        int spins = 0;
        for (;;) {
            int step = beast_parser_byte(&parser, beast_wire[i], &msg);
            if (step == BEAST_FRAME) {
                rc = 1;
                break;
            }
            if (step == BEAST_RETRY || step == BEAST_BROKEN) {
                if (++spins > 4) {
                    break;
                }
                continue;
            }
            break;
        }
    }
    if (rc != 1 || msg.signal != 0x1a || msg.payload[3] != 0x1a || msg.payload_len != 7) {
        fprintf(stderr, "beast decode mismatch\n");
        failures++;
    }

    memset(&packed, 0, sizeof(packed));
    packed.version = ADSB_STRUCT_VERSION;
    packed.flags = ADSB_FLAG_POSITION | ADSB_FLAG_ALTITUDE;
    packed.icao = 0x40621D;
    packed.latitude_e7 = 522572021;
    packed.longitude_e7 = 39193726;
    packed.altitude_ft = 38000;
    packed.velocity_kt = ADSB_VEL_INVALID;
    packed.timestamp_us = 1457996402000000ull;
    if (struct_encode(&packed, struct_wire) != STRUCT_OK ||
        struct_decode(struct_wire, &decoded) != STRUCT_OK ||
        decoded.icao != packed.icao || decoded.latitude_e7 != packed.latitude_e7 ||
        decoded.altitude_ft != 38000 || decoded.timestamp_us != packed.timestamp_us) {
        fprintf(stderr, "struct roundtrip mismatch\n");
        failures++;
    }
    struct_wire[31] ^= 0xFF;
    if (struct_decode(struct_wire, &decoded) != STRUCT_ERR_CHECKSUM) {
        fprintf(stderr, "struct checksum was not rejected\n");
        failures++;
    }

    if (failures != 0) {
        fprintf(stderr, "self-check failed (%d)\n", failures);
        return 1;
    }
    printf("self-check ok\n");
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: frame_tool self-check|layout|beast-encode|beast-decode|"
                        "struct-encode|struct-decode|link-decode\n");
        return 1;
    }
    if (strcmp(argv[1], "self-check") == 0) {
        return self_check();
    }
    if (strcmp(argv[1], "layout") == 0) {
        return cmd_layout();
    }
    if (strcmp(argv[1], "beast-encode") == 0) {
        return cmd_beast_encode(argc, argv);
    }
    if (strcmp(argv[1], "beast-decode") == 0) {
        return cmd_beast_decode();
    }
    if (strcmp(argv[1], "struct-encode") == 0) {
        return cmd_struct_encode(argc, argv);
    }
    if (strcmp(argv[1], "struct-decode") == 0) {
        return cmd_struct_decode();
    }
    if (strcmp(argv[1], "link-decode") == 0) {
        return cmd_link_decode();
    }
    fprintf(stderr, "unknown command %s\n", argv[1]);
    return 1;
}
