/* UART ingest for trck. Not the ESP32-S3 OBC radio firmware.
 * Does not overlay a packed struct. Does not send the UART frame on a radio.
 */

#include <stdio.h>

#include "driver/gpio.h"
#include "driver/uart.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "trck_frame.h"

static const char *TAG = "trck";

#define PI_UART UART_NUM_1
#define PI_UART_RX_GPIO GPIO_NUM_18
#define PI_UART_BAUD 115200
#define PI_UART_RX_BUF 1024

static const char *k_emergency[] = {
    "none", "general", "lifeguard", "minfuel", "nordo", "unlawful", "downed", "reserved",
};

static void fmt_deg(char *out, size_t cap, int32_t e7) {
    const char *sign = "";
    uint32_t abs_e7;
    if (e7 < 0) {
        sign = "-";
        abs_e7 = (uint32_t)(-(e7 + 1)) + 1u;
    } else {
        abs_e7 = (uint32_t)e7;
    }
    (void)snprintf(out, cap, "%s%lu.%07lu", sign, (unsigned long)(abs_e7 / 10000000u),
                   (unsigned long)(abs_e7 % 10000000u));
}

static void log_fix(const trck_fix_t *fix) {
    char lat[16];
    char lon[16];
    char alt[16];
    fmt_deg(lat, sizeof lat, fix->lat_e7);
    fmt_deg(lon, sizeof lon, fix->lon_e7);
    if (fix->alt_ft == TRCK_ALT_UNKNOWN) {
        (void)snprintf(alt, sizeof alt, "unknown");
    } else {
        (void)snprintf(alt, sizeof alt, "%ld", (long)fix->alt_ft);
    }
    const char *name = fix->emergency < 8 ? k_emergency[fix->emergency] : "?";
    ESP_LOGI(TAG, "icao=%06lX lat=%s lon=%s alt=%s emergency=%u %s", (unsigned long)fix->icao, lat,
             lon, alt, (unsigned)fix->emergency, name);
}

static void init_uart(void) {
    uart_config_t cfg = {
        .baud_rate = PI_UART_BAUD,
        .data_bits = UART_DATA_8_BITS,
        .parity = UART_PARITY_DISABLE,
        .stop_bits = UART_STOP_BITS_1,
        .flow_ctrl = UART_HW_FLOWCTRL_DISABLE,
        .source_clk = UART_SCLK_DEFAULT,
    };
    ESP_ERROR_CHECK(uart_driver_install(PI_UART, PI_UART_RX_BUF, 0, 0, NULL, 0));
    ESP_ERROR_CHECK(uart_param_config(PI_UART, &cfg));
    ESP_ERROR_CHECK(uart_set_pin(PI_UART, UART_PIN_NO_CHANGE, PI_UART_RX_GPIO, UART_PIN_NO_CHANGE,
                                 UART_PIN_NO_CHANGE));
}

void app_main(void) {
    init_uart();
    ESP_LOGI(TAG, "UART ingest 115200 8N1 RX GPIO %d. Waiting for TRCK.", (int)PI_UART_RX_GPIO);
    ESP_LOGI(TAG, "Wire Pi GPIO 14 (pin 8) to that RX, 3.3 V, common ground.");

    trck_scan_t scan;
    trck_scan_init(&scan);
    for (;;) {
        uint8_t byte;
        int n = uart_read_bytes(PI_UART, &byte, 1, portMAX_DELAY);
        if (n != 1) {
            continue;
        }
        trck_fix_t fix;
        if (trck_scan_feed(&scan, byte, &fix)) {
            log_fix(&fix);
        }
    }
}
