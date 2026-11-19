//! Bake the unique 2-bit PI syndrome table (dump1090-fa `prepareErrorTable(..., 2, 4)`).

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::Path;

const LONG_BITS: usize = 112;
const POLY: u32 = 0x00FF_F409;

fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = (i as u32) << 16;
        for _ in 0..8 {
            c = if c & 0x0080_0000 != 0 {
                (c << 1) ^ POLY
            } else {
                c << 1
            };
        }
        *slot = c & 0x00FF_FFFF;
    }
    table
}

fn modes_checksum(table: &[u32; 256], message: &[u8; 14]) -> u32 {
    let mut rem = 0u32;
    for &byte in &message[..11] {
        rem = (rem << 8) ^ table[(u32::from(byte) ^ ((rem & 0x00FF_0000) >> 16)) as usize];
        rem &= 0x00FF_FFFF;
    }
    rem ^ (u32::from(message[11]) << 16) ^ (u32::from(message[12]) << 8) ^ u32::from(message[13])
}

fn long_syndromes() -> [u32; LONG_BITS] {
    let crc = crc_table();
    let mut table = [0u32; LONG_BITS];
    for (bit, slot) in table.iter_mut().enumerate() {
        let mut msg = [0u8; 14];
        msg[bit / 8] ^= 1 << (7 - (bit % 8));
        *slot = modes_checksum(&crc, &msg);
    }
    table
}

fn two_bit_long(s: &[u32; LONG_BITS]) -> Vec<(u32, u8, u8)> {
    let one: HashSet<u32> = s.iter().copied().collect();
    let mut map: HashMap<u32, Option<(u8, u8)>> = HashMap::new();
    for i in 0..LONG_BITS {
        for j in (i + 1)..LONG_BITS {
            let syn = s[i] ^ s[j];
            if syn == 0 || one.contains(&syn) {
                continue;
            }
            match map.get_mut(&syn) {
                None => {
                    map.insert(syn, Some((i as u8, j as u8)));
                }
                Some(slot) => *slot = None,
            }
        }
    }
    for i in 0..LONG_BITS {
        for j in (i + 1)..LONG_BITS {
            let sj = s[i] ^ s[j];
            for k in (j + 1)..LONG_BITS {
                let sk = sj ^ s[k];
                if let Some(slot) = map.get_mut(&sk) {
                    *slot = None;
                }
                for &sl in s.iter().skip(k + 1) {
                    if let Some(slot) = map.get_mut(&(sk ^ sl)) {
                        *slot = None;
                    }
                }
            }
        }
    }
    let mut rows: Vec<(u32, u8, u8)> = map
        .into_iter()
        .filter_map(|(syn, pair)| pair.map(|(a, b)| (syn, a, b)))
        .collect();
    rows.sort_unstable_by_key(|r| r.0);
    rows
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let rows = two_bit_long(&long_syndromes());
    let mut out = format!("static TWO_BIT_LONG: [(u32, u8, u8); {}] = [\n", rows.len());
    for &(s, a, b) in &rows {
        out.push_str(&format!("    (0x{s:06X}, {a}, {b}),\n"));
    }
    out.push_str("];\n");
    let path = Path::new(&env::var("OUT_DIR").expect("OUT_DIR")).join("two_bit_long.rs");
    fs::write(path, out).expect("write two_bit_long.rs");
}
