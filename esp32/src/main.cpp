// Bench receiver for the Pi sender. One UART stream, two framings:
//   0x1A ...  raw Beast from readsb, forwarded unchanged
//   0x5B 0xAD packed struct from common/adsb_struct.h
// USB serial logs what arrived. This is a PlatformIO Arduino sketch
// because the repository had no embedded tree to extend.

#include <Arduino.h>

#include <string.h>

#include "link_parser.h"

static const int ADSB_RX_PIN = 16;  // UART2 RX, GPIO16 on a classic ESP32
static const int ADSB_TX_PIN = 17;  // unused; the Pi only transmits
static const uint32_t ADSB_UART_BAUD = 115200;
static const uint32_t LOG_BAUD = 115200;

static link_parser_t link;

static void print_u64(uint64_t value) {
    char buf[21];
    int i = (int)sizeof(buf) - 1;
    buf[i] = '\0';
    if (value == 0) {
        Serial.print('0');
        return;
    }
    while (value > 0 && i > 0) {
        buf[--i] = (char)('0' + (value % 10));
        value /= 10;
    }
    Serial.print(buf + i);
}

static void print_e7(int32_t value) {
    uint32_t mag;
    if (value < 0) {
        Serial.print('-');
        mag = (uint32_t)(-(int64_t)value);
    } else {
        mag = (uint32_t)value;
    }
    Serial.printf("%lu.%07lu", (unsigned long)(mag / 10000000u), (unsigned long)(mag % 10000000u));
}

static uint64_t load_timestamp(const adsb_struct_t *msg) {
    uint64_t value;
    memcpy(&value, &msg->timestamp_us, sizeof(value));
    return value;
}

static void on_beast(const beast_message_t *msg, void *user) {
    uint32_t addr = 0;
    (void)user;
    Serial.printf("beast type=%c crc=%d count=%lu frame_err=%lu crc_err=%lu",
                  (char)msg->type, (int)msg->crc_ok, (unsigned long)link.stats.beast_ok,
                  (unsigned long)link.stats.beast_framing_errors,
                  (unsigned long)link.stats.beast_crc_bad);
    if (msg->payload_len >= 1) {
        Serial.printf(" df=%u", (unsigned)(msg->payload[0] >> 3));
    }
    if (beast_address(msg, &addr)) {
        Serial.printf(" icao=%06lX", (unsigned long)addr);
    }
    Serial.println();
}

static void on_struct(const adsb_struct_t *msg, void *user) {
    uint64_t timestamp = load_timestamp(msg);
    (void)user;
    Serial.printf("struct icao=%06lX flags=0x%02X count=%lu crc_err=%lu ",
                  (unsigned long)msg->icao, msg->flags, (unsigned long)link.stats.struct_ok,
                  (unsigned long)link.stats.struct_checksum_errors);
    if (msg->flags & ADSB_FLAG_POSITION) {
        Serial.print("lat=");
        print_e7(msg->latitude_e7);
        Serial.print(" lon=");
        print_e7(msg->longitude_e7);
        Serial.print(' ');
    } else {
        Serial.print("lat=- lon=- ");
    }
    if (msg->flags & ADSB_FLAG_ALTITUDE) {
        Serial.printf("alt=%ldft ", (long)msg->altitude_ft);
    } else {
        Serial.print("alt=- ");
    }
    if (msg->flags & ADSB_FLAG_VELOCITY) {
        Serial.printf("vel=%ukt ", (unsigned)msg->velocity_kt);
    } else {
        Serial.print("vel=- ");
    }
    Serial.print("t=");
    print_u64(timestamp);
    Serial.println();
}

static void on_error(int code, void *user) {
    (void)user;
    if (code == LINK_ERR_BEAST_FRAMING) {
        Serial.printf("err beast_framing frame_err=%lu\n",
                      (unsigned long)link.stats.beast_framing_errors);
    } else if (code == LINK_ERR_STRUCT_CHECKSUM) {
        Serial.printf("err struct_checksum crc_err=%lu\n",
                      (unsigned long)link.stats.struct_checksum_errors);
    } else if (code == LINK_ERR_STRUCT_VERSION) {
        Serial.printf("err struct_version ver_err=%lu\n",
                      (unsigned long)link.stats.struct_version_errors);
    }
}

void setup() {
    Serial.begin(LOG_BAUD);
    Serial2.begin(ADSB_UART_BAUD, SERIAL_8N1, ADSB_RX_PIN, ADSB_TX_PIN);
    link_parser_init(&link, on_beast, on_struct, on_error, nullptr);
    Serial.println("adsb bench esp32: raw Beast (0x1A) and struct (0x5B 0xAD)");
    Serial.printf("uart2 rx=%d tx=%d baud=%lu\n", ADSB_RX_PIN, ADSB_TX_PIN,
                  (unsigned long)ADSB_UART_BAUD);
}

void loop() {
    static uint32_t last_beat = 0;
    while (Serial2.available() > 0) {
        int value = Serial2.read();
        if (value < 0) {
            break;
        }
        uint8_t byte = (uint8_t)value;
        link_parser_feed(&link, &byte, 1);
    }
    if (millis() - last_beat >= 5000) {
        last_beat = millis();
        Serial.printf("beat beast=%lu struct=%lu frame_err=%lu beast_crc=%lu struct_crc=%lu ver_err=%lu junk=%lu\n",
                      (unsigned long)link.stats.beast_ok, (unsigned long)link.stats.struct_ok,
                      (unsigned long)link.stats.beast_framing_errors,
                      (unsigned long)link.stats.beast_crc_bad,
                      (unsigned long)link.stats.struct_checksum_errors,
                      (unsigned long)link.stats.struct_version_errors,
                      (unsigned long)link.stats.junk_bytes);
    }
}
