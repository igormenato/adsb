"""Decode DF17/DF18 Beast payloads into one TrackStruct per squitter.

Position uses globally unambiguous CPR, so latitude and longitude stay
invalid until an even and an odd frame for that address arrive within
10 seconds. Altitude and velocity are filled only when that squitter
carries them. This is not a fused track.
"""

from __future__ import annotations

import math

from beast import BeastMessage
from struct_frame import (
    ADSB_FLAG_ALTITUDE,
    ADSB_FLAG_POSITION,
    ADSB_FLAG_VELOCITY,
    TrackStruct,
)

CPR_PAIR_WINDOW_S = 10.0
FT_PER_M = 3.280839895
ADSB_VEL_MAX = 65534

# dump1090 longitude-zone table, symmetric about the equator.
_NL_LIMITS = (
    (10.47047130, 59),
    (14.82817437, 58),
    (18.18681846, 57),
    (21.02939493, 56),
    (23.54504487, 55),
    (25.82924707, 54),
    (27.93898710, 53),
    (29.91135686, 52),
    (31.77209708, 51),
    (33.53993436, 50),
    (35.22899598, 49),
    (36.85025108, 48),
    (38.41241892, 47),
    (39.92256684, 46),
    (41.38651832, 45),
    (42.80914012, 44),
    (44.19454951, 43),
    (45.54626723, 42),
    (46.86733252, 41),
    (48.16039128, 40),
    (49.42776439, 39),
    (50.67150166, 38),
    (51.89342469, 37),
    (53.09516153, 36),
    (54.27817472, 35),
    (55.44378444, 34),
    (56.59318756, 33),
    (57.72747354, 32),
    (58.84763776, 31),
    (59.95459277, 30),
    (61.04917774, 29),
    (62.13216659, 28),
    (63.20427479, 27),
    (64.26616523, 26),
    (65.31845310, 25),
    (66.36171008, 24),
    (67.39646774, 23),
    (68.42322022, 22),
    (69.44242631, 21),
    (70.45451075, 20),
    (71.45986473, 19),
    (72.45884545, 18),
    (73.45177442, 17),
    (74.43893416, 16),
    (75.42056257, 15),
    (76.39684391, 14),
    (77.36789461, 13),
    (78.33374083, 12),
    (79.29428225, 11),
    (80.24923213, 10),
    (81.19801349, 9),
    (82.13956981, 8),
    (83.07199445, 7),
    (83.99173563, 6),
    (84.89166191, 5),
    (85.75541621, 4),
    (86.53536998, 3),
    (87.0, 2),
)


def nl(lat: float) -> int:
    lat = abs(lat)
    for limit, zones in _NL_LIMITS:
        if lat < limit:
            return zones
    return 1


def deg_e7(degrees: float) -> int:
    scaled = math.floor(abs(degrees) * 10_000_000 + 0.5)
    return -scaled if degrees < 0 else scaled


class CprCache:
    def __init__(self) -> None:
        self._even: dict[int, tuple[int, int, float]] = {}
        self._odd: dict[int, tuple[int, int, float]] = {}

    def update(
        self, icao: int, odd: bool, cpr_lat: int, cpr_lon: int, now_s: float
    ) -> tuple[float, float] | None:
        (self._odd if odd else self._even)[icao] = (cpr_lat, cpr_lon, now_s)
        even = self._even.get(icao)
        odd_frame = self._odd.get(icao)
        if even is None or odd_frame is None:
            return None
        if abs(even[2] - odd_frame[2]) > CPR_PAIR_WINDOW_S:
            return None
        use_even = even[2] >= odd_frame[2]
        return cpr_global(even[0], even[1], odd_frame[0], odd_frame[1], use_even)


def cpr_global(
    even_lat: int, even_lon: int, odd_lat: int, odd_lon: int, use_even: bool
) -> tuple[float, float] | None:
    j = math.floor(((59 * even_lat) - (60 * odd_lat)) / 131072.0 + 0.5)
    lat_even = (360.0 / 60.0) * ((j % 60) + even_lat / 131072.0)
    lat_odd = (360.0 / 59.0) * ((j % 59) + odd_lat / 131072.0)
    if lat_even >= 270.0:
        lat_even -= 360.0
    if lat_odd >= 270.0:
        lat_odd -= 360.0
    if nl(lat_even) != nl(lat_odd):
        return None

    lat = lat_even if use_even else lat_odd
    zones = nl(lat)
    m = math.floor((even_lon * (zones - 1) - odd_lon * zones) / 131072.0 + 0.5)
    if use_even:
        ni = max(zones, 1)
        lon = (360.0 / ni) * ((m % ni) + even_lon / 131072.0)
    else:
        ni = max(zones - 1, 1)
        lon = (360.0 / ni) * ((m % ni) + odd_lon / 131072.0)
    lon -= math.floor((lon + 180.0) / 360.0) * 360.0
    if not (-90.0 <= lat <= 90.0 and -180.0 <= lon <= 180.0):
        return None
    return lat, lon


def _baro_alt_ft(alt12: int) -> int | None:
    # Q is the 8th bit of the 12-bit field. Q=0 is Gillham coding, not decoded here.
    if ((alt12 >> 4) & 1) == 0:
        return None
    n = ((alt12 >> 5) << 4) | (alt12 & 0xF)
    return n * 25 - 1000


def _ground_speed_kt(me: bytes) -> int | None:
    subtype = me[0] & 0x07
    if subtype not in (1, 2):
        return None
    v_ew = ((me[1] & 0x03) << 8) | me[2]
    v_ns = ((me[3] & 0x7F) << 3) | (me[4] >> 5)
    if v_ew == 0 or v_ns == 0:
        return None
    v_ew -= 1
    v_ns -= 1
    if subtype == 2:
        v_ew *= 4
        v_ns *= 4
    speed = math.floor(math.hypot(v_ew, v_ns) + 0.5)
    if speed >= ADSB_VEL_MAX:
        return ADSB_VEL_MAX
    return speed


def decode_adsb(msg: BeastMessage, cache: CprCache, now_s: float, now_us: int) -> TrackStruct | None:
    if msg.type != 0x33 or len(msg.payload) != 14 or msg.crc_ok != 1:
        return None
    df = msg.payload[0] >> 3
    if df not in (17, 18):
        return None
    icao = (msg.payload[1] << 16) | (msg.payload[2] << 8) | msg.payload[3]
    track = TrackStruct.empty(icao, now_us)
    me = msg.payload[4:11]
    tc = me[0] >> 3

    if 9 <= tc <= 18 or 20 <= tc <= 22:
        alt12 = ((me[1] << 4) | (me[2] >> 4)) & 0xFFF
        if 9 <= tc <= 18:
            alt = _baro_alt_ft(alt12)
        else:
            alt = math.floor(alt12 * FT_PER_M + 0.5)
        if alt is not None:
            track.altitude_ft = alt
            track.flags |= ADSB_FLAG_ALTITUDE
        odd = (me[2] >> 2) & 1
        cpr_lat = ((me[2] & 0x03) << 15) | (me[3] << 7) | (me[4] >> 1)
        cpr_lon = ((me[4] & 0x01) << 16) | (me[5] << 8) | me[6]
        position = cache.update(icao, bool(odd), cpr_lat, cpr_lon, now_s)
        if position is not None:
            track.latitude_e7 = deg_e7(position[0])
            track.longitude_e7 = deg_e7(position[1])
            track.flags |= ADSB_FLAG_POSITION
    elif tc == 19:
        speed = _ground_speed_kt(me)
        if speed is not None:
            track.velocity_kt = speed
            track.flags |= ADSB_FLAG_VELOCITY
    return track
