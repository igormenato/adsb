#ifndef ADSB_MODES_CRC_H
#define ADSB_MODES_CRC_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Mode S CRC-24 (poly 0xFFF409) over a full short or long squitter.
   A valid DF17/DF18 codeword returns 0. */
uint32_t modes_crc24(const uint8_t *msg, int len);

#ifdef __cplusplus
}
#endif

#endif
