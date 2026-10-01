#ifndef ADSB_CRC16_H
#define ADSB_CRC16_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* CRC-16/CCITT-FALSE: poly 0x1021, init 0xFFFF, xorout 0, not reflected.
   crc16_ccitt_false("123456789") == 0x29B1. */
uint16_t crc16_ccitt_false(const uint8_t *data, size_t len);

#ifdef __cplusplus
}
#endif

#endif
