#include "modes_crc.h"

uint32_t modes_crc24(const uint8_t *msg, int len) {
    uint32_t crc = 0;
    int i;
    int bit;

    for (i = 0; i < len; i++) {
        crc ^= (uint32_t)msg[i] << 16;
        for (bit = 0; bit < 8; bit++) {
            if (crc & 0x800000u) {
                crc = ((crc << 1) ^ 0xFFF409u) & 0xFFFFFFu;
            } else {
                crc = (crc << 1) & 0xFFFFFFu;
            }
        }
    }
    return crc;
}
