//! Emergency alerts from readsb, written ahead of the snapshot.

use std::collections::{BTreeMap, BTreeSet};

use crate::frame::{crc16_ccitt_false, Aircraft, Snapshot};
use crate::snapshot::{snapshot_with_emergency, SnapshotError};

/// ADS-B emergency/priority status. Squawk 7700, 7600, and 7500 map to general, nordo, and unlawful.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EmergencyKind {
    General = 1,
    Lifeguard = 2,
    MinFuel = 3,
    Nordo = 4,
    Unlawful = 5,
    Downed = 6,
    Reserved = 7,
}

impl EmergencyKind {
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::General,
            2 => Self::Lifeguard,
            3 => Self::MinFuel,
            4 => Self::Nordo,
            5 => Self::Unlawful,
            6 => Self::Downed,
            7 => Self::Reserved,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Lifeguard => "lifeguard",
            Self::MinFuel => "minfuel",
            Self::Nordo => "nordo",
            Self::Unlawful => "unlawful",
            Self::Downed => "downed",
            Self::Reserved => "reserved",
        }
    }
}

pub const ALERT_MAGIC: &[u8; 4] = b"ALRT";
pub const ALERT_VERSION: u8 = 1;
pub const ALERT_LEN: usize = 30;

/// One emergency report. `cleared` is the single message sent when the alert ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub icao: u32,
    pub kind: EmergencyKind,
    pub latitude_e7: i32,
    pub longitude_e7: i32,
    pub altitude_ft: i32,
    pub unix_s: u32,
    pub cleared: bool,
}

/// The snapshot for this cycle, plus the alert messages to write before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlertCycle {
    pub snapshot: Snapshot,
    pub alerts: Vec<Alert>,
}

#[derive(Clone, Copy, Debug)]
struct Fix {
    kind: EmergencyKind,
    latitude_e7: i32,
    longitude_e7: i32,
    altitude_ft: i32,
}

impl Fix {
    fn from_aircraft(kind: EmergencyKind, aircraft: &Aircraft) -> Self {
        Self {
            kind,
            latitude_e7: aircraft.latitude_e7,
            longitude_e7: aircraft.longitude_e7,
            altitude_ft: aircraft.altitude_ft,
        }
    }
}

/// Confirms an emergency across two consecutive cycles, then repeats until it ends.
#[derive(Debug, Default)]
pub struct AlertTracker {
    pending: BTreeSet<u32>,
    active: BTreeMap<u32, Fix>,
}

impl AlertTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// One read of `aircraft.json`. A document that fails to parse does not change the tracker.
    pub fn cycle(&mut self, bytes: &[u8]) -> Result<AlertCycle, SnapshotError> {
        let parsed = snapshot_with_emergency(bytes)?;
        let alerts = self.update(&parsed.snapshot, &parsed.emergency);
        Ok(AlertCycle {
            snapshot: parsed.snapshot,
            alerts,
        })
    }

    fn update(&mut self, snapshot: &Snapshot, emergency: &[Option<EmergencyKind>]) -> Vec<Alert> {
        let mut emergency_fix: BTreeMap<u32, Fix> = BTreeMap::new();
        let mut position: BTreeMap<u32, (i32, i32, i32)> = BTreeMap::new();
        let mut order = Vec::new();
        for (aircraft, kind) in snapshot.aircraft.iter().zip(emergency.iter()) {
            if position.contains_key(&aircraft.icao) {
                continue;
            }
            position.insert(
                aircraft.icao,
                (
                    aircraft.latitude_e7,
                    aircraft.longitude_e7,
                    aircraft.altitude_ft,
                ),
            );
            order.push(aircraft.icao);
            if let Some(kind) = kind {
                emergency_fix.insert(aircraft.icao, Fix::from_aircraft(*kind, aircraft));
            }
        }

        let mut alerts = Vec::new();
        let mut next_pending = BTreeSet::new();
        for icao in order {
            let Some(fix) = emergency_fix.get(&icao).copied() else {
                continue;
            };
            if self.active.contains_key(&icao) || self.pending.contains(&icao) {
                self.active.insert(icao, fix);
                alerts.push(alert_from(icao, fix, snapshot.unix_s));
            } else {
                next_pending.insert(icao);
            }
        }
        self.pending = next_pending;

        let ended: Vec<u32> = self
            .active
            .keys()
            .copied()
            .filter(|icao| !emergency_fix.contains_key(icao))
            .collect();
        for icao in ended {
            let Some(previous) = self.active.remove(&icao) else {
                continue;
            };
            let (latitude_e7, longitude_e7, altitude_ft) =
                position.get(&icao).copied().unwrap_or((
                    previous.latitude_e7,
                    previous.longitude_e7,
                    previous.altitude_ft,
                ));
            alerts.push(Alert {
                icao,
                kind: previous.kind,
                latitude_e7,
                longitude_e7,
                altitude_ft,
                unix_s: snapshot.unix_s,
                cleared: true,
            });
        }
        alerts
    }
}

fn alert_from(icao: u32, fix: Fix, unix_s: u32) -> Alert {
    Alert {
        icao,
        kind: fix.kind,
        latitude_e7: fix.latitude_e7,
        longitude_e7: fix.longitude_e7,
        altitude_ft: fix.altitude_ft,
        unix_s,
        cleared: false,
    }
}

/// Fixed 30-byte packet. Little-endian. Magic is the ASCII bytes `ALRT`.
pub fn pack_alert(alert: &Alert) -> Vec<u8> {
    let mut raw = Vec::with_capacity(ALERT_LEN);
    raw.extend_from_slice(ALERT_MAGIC);
    raw.push(ALERT_VERSION);
    raw.push(u8::from(alert.cleared));
    raw.push(alert.kind.code());
    raw.push(0);
    raw.extend_from_slice(&(alert.icao & 0x00ff_ffff).to_le_bytes());
    raw.extend_from_slice(&alert.latitude_e7.to_le_bytes());
    raw.extend_from_slice(&alert.longitude_e7.to_le_bytes());
    raw.extend_from_slice(&alert.altitude_ft.to_le_bytes());
    raw.extend_from_slice(&alert.unix_s.to_le_bytes());
    let crc = crc16_ccitt_false(&raw);
    raw.extend_from_slice(&crc.to_le_bytes());
    raw
}

/// One JSON object and a trailing newline. The line starts with `{"alert":`.
pub fn pack_alert_json(alert: &Alert) -> Vec<u8> {
    let mut raw = Vec::with_capacity(160);
    raw.extend_from_slice(b"{\"alert\":{\"icao\":");
    push_num(&mut raw, alert.icao);
    raw.extend_from_slice(b",\"emergency\":\"");
    raw.extend_from_slice(alert.kind.as_str().as_bytes());
    raw.extend_from_slice(b"\",\"latitude_e7\":");
    push_num(&mut raw, alert.latitude_e7);
    raw.extend_from_slice(b",\"longitude_e7\":");
    push_num(&mut raw, alert.longitude_e7);
    raw.extend_from_slice(b",\"altitude_ft\":");
    push_num(&mut raw, alert.altitude_ft);
    raw.extend_from_slice(b",\"unix_s\":");
    push_num(&mut raw, alert.unix_s);
    raw.extend_from_slice(b",\"cleared\":");
    raw.extend_from_slice(if alert.cleared { b"true" } else { b"false" });
    raw.extend_from_slice(b"}}\n");
    raw
}

fn push_num(raw: &mut Vec<u8>, value: impl std::fmt::Display) {
    let text = value.to_string();
    raw.extend_from_slice(text.as_bytes());
}

pub fn unpack_alert(bytes: &[u8]) -> Option<Alert> {
    if bytes.len() != ALERT_LEN || &bytes[0..4] != ALERT_MAGIC {
        return None;
    }
    if bytes[4] != ALERT_VERSION || bytes[7] != 0 {
        return None;
    }
    let cleared = match bytes[5] {
        0 => false,
        1 => true,
        _ => return None,
    };
    let kind = EmergencyKind::from_code(bytes[6])?;
    let expect = crc16_ccitt_false(&bytes[..ALERT_LEN - 2]);
    let got = u16::from_le_bytes([bytes[28], bytes[29]]);
    if expect != got {
        return None;
    }
    Some(Alert {
        icao: u32::from_le_bytes(bytes[8..12].try_into().ok()?) & 0x00ff_ffff,
        kind,
        latitude_e7: i32::from_le_bytes(bytes[12..16].try_into().ok()?),
        longitude_e7: i32::from_le_bytes(bytes[16..20].try_into().ok()?),
        altitude_ft: i32::from_le_bytes(bytes[20..24].try_into().ok()?),
        unix_s: u32::from_le_bytes(bytes[24..28].try_into().ok()?),
        cleared,
    })
}
