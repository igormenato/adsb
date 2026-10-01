#ifndef ADSB_STRUCT_FRAME_H
#define ADSB_STRUCT_FRAME_H

#include "adsb_struct.h"

#ifdef __cplusplus
extern "C" {
#endif

enum {
    STRUCT_OK = 0,
    STRUCT_ERR_CHECKSUM = -1,
    STRUCT_ERR_VERSION = -2,
    STRUCT_ERR_MAGIC = -3,
    STRUCT_ERR_RANGE = -4
};

/* Serialize msg. Checksum is computed; msg is not modified.
   out must hold ADSB_STRUCT_SIZE bytes. Returns 0, or STRUCT_ERR_RANGE. */
int struct_encode(const adsb_struct_t *msg, uint8_t out[ADSB_STRUCT_SIZE]);

/* Validate magic, checksum, and version, then fill *out with host integers. */
int struct_decode(const uint8_t in[ADSB_STRUCT_SIZE], adsb_struct_t *out);

#ifdef __cplusplus
}
#endif

#endif
