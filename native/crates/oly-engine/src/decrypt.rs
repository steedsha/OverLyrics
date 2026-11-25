//! Provider payload decryption: KRC (Kugou) and QRC (QQ Music).
//!
//! Both algorithms are adapted from Lyricify.Lyrics.Helper (see
//! THIRD_PARTY_NOTICES.md). They are not standard crypto:
//!
//! * KRC is base64 → drop four header bytes → XOR against a fixed 16-byte key →
//!   zlib inflate. The inflated text starts at `[` with no extra byte to strip.
//! * QRC uses QQ's private DES variant: the S-boxes are modified (for instance
//!   `sbox4`'s second row carries two `10`s where standard DES has `1, 13`), the
//!   PC-1/PC-2 permutations are the standard ones, and decryption is
//!   `D(K3) → E(K2) → D(K1)` over 8-byte ECB blocks.
//!
//! The goldens under `native/tests/fixtures/oracle/engine/{krc,des}.json` pin
//! both: 26 KRC payloads round-tripped through the shipped fixtures and 24 raw
//! 3DES blocks.

use std::io::Read;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

/// Kugou's obfuscation key (`KEY` in `krc.ts`).
const KRC_KEY: [u8; 16] = [
    0x40, 0x47, 0x61, 0x77, 0x5e, 0x32, 0x74, 0x47, 0x51, 0x36, 0x31, 0x2d, 0xce, 0xd2, 0x6e, 0x69,
];

/// Decrypt a base64 KRC payload into its plaintext chart, or `None` when the
/// input is not decryptable at all.
///
/// A payload that fails both inflate variants, or that is empty/not a string in
/// the reference, yields `None` — never a partial document.
pub fn decrypt_krc(b64: &str) -> Option<String> {
    if b64.is_empty() {
        return None;
    }
    let raw = BASE64.decode(b64).ok()?;
    // The first four bytes are a length/format header; the reference drops them
    // before the XOR pass.
    if raw.len() < 4 {
        return None;
    }
    let mut buf: Vec<u8> = raw[4..].to_vec();
    for (i, byte) in buf.iter_mut().enumerate() {
        *byte ^= KRC_KEY[i % KRC_KEY.len()];
    }

    let mut text = None;
    // Some payloads are zlib-wrapped, some are raw deflate. The reference tries
    // both before giving up.
    for raw_deflate in [false, true] {
        let mut out = String::new();
        let ok = if raw_deflate {
            let mut decoder = flate2::read::DeflateDecoder::new(buf.as_slice());
            decoder.read_to_string(&mut out).is_ok()
        } else {
            let mut decoder = flate2::read::ZlibDecoder::new(buf.as_slice());
            decoder.read_to_string(&mut out).is_ok()
        };
        if ok {
            text = Some(out);
            break;
        }
    }
    let text = text?;
    // A BOM shows up in a few versions; everything else is kept verbatim.
    Some(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

/// Pull the per-line translation array out of a decrypted KRC chart.
/// Extract the Chinese translation rows from a decoded KRC `[language:...]`
/// header (`type == 1`), returning `None` when no translation track exists.
pub fn krc_translations(krc: &str) -> Option<Vec<String>> {
    let start = krc.find("[language:")? + "[language:".len();
    let end_rel = krc[start..].find(']')?;
    let payload = &krc[start..start + end_rel];

    let json_bytes = BASE64.decode(payload).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&json_bytes).ok()?;
    let content = json.get("content")?.as_array()?;
    let item = content
        .iter()
        .find(|c| c.get("type").and_then(|t| t.as_i64()) == Some(1))?;
    let rows = item.get("lyricContent")?.as_array()?;
    Some(
        rows.iter()
            .map(|row| match row {
                serde_json::Value::Array(cells) => cells
                    .first()
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string(),
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            })
            .collect(),
    )
}

/// Extract per-line romanization / Jyutping rows from a decoded KRC
/// `[language:...]` header (`type == 0`), joining syllable cells with spaces.
pub fn krc_romanizations(krc: &str) -> Option<Vec<String>> {
    let start = krc.find("[language:")? + "[language:".len();
    let end_rel = krc[start..].find(']')?;
    let payload = &krc[start..start + end_rel];

    let json_bytes = BASE64.decode(payload).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&json_bytes).ok()?;
    let content = json.get("content")?.as_array()?;
    let item = content
        .iter()
        .find(|c| c.get("type").and_then(|t| t.as_i64()) == Some(0))?;
    let rows = item.get("lyricContent")?.as_array()?;
    Some(
        rows.iter()
            .map(|row| match row {
                serde_json::Value::Array(cells) => cells
                    .iter()
                    .filter_map(|c| c.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" "),
                serde_json::Value::String(s) => s.trim().to_string(),
                _ => String::new(),
            })
            .collect(),
    )
}

// ============================ QQ 3DES ============================

/// QQ's modified DES S-boxes (eight 64-entry tables).
const SBOX: [[u8; 64]; 8] = [
    [
        14, 4, 13, 1, 2, 15, 11, 8, 3, 10, 6, 12, 5, 9, 0, 7, 0, 15, 7, 4, 14, 2, 13, 1, 10, 6, 12,
        11, 9, 5, 3, 8, 4, 1, 14, 8, 13, 6, 2, 11, 15, 12, 9, 7, 3, 10, 5, 0, 15, 12, 8, 2, 4, 9,
        1, 7, 5, 11, 3, 14, 10, 0, 6, 13,
    ],
    [
        15, 1, 8, 14, 6, 11, 3, 4, 9, 7, 2, 13, 12, 0, 5, 10, 3, 13, 4, 7, 15, 2, 8, 15, 12, 0, 1,
        10, 6, 9, 11, 5, 0, 14, 7, 11, 10, 4, 13, 1, 5, 8, 12, 6, 9, 3, 2, 15, 13, 8, 10, 1, 3,
        15, 4, 2, 11, 6, 7, 12, 0, 5, 14, 9,
    ],
    [
        10, 0, 9, 14, 6, 3, 15, 5, 1, 13, 12, 7, 11, 4, 2, 8, 13, 7, 0, 9, 3, 4, 6, 10, 2, 8, 5,
        14, 12, 11, 15, 1, 13, 6, 4, 9, 8, 15, 3, 0, 11, 1, 2, 12, 5, 10, 14, 7, 1, 10, 13, 0, 6,
        9, 8, 7, 4, 15, 14, 3, 11, 5, 2, 12,
    ],
    [
        7, 13, 14, 3, 0, 6, 9, 10, 1, 2, 8, 5, 11, 12, 4, 15, 13, 8, 11, 5, 6, 15, 0, 3, 4, 7, 2,
        12, 1, 10, 14, 9, 10, 6, 9, 0, 12, 11, 7, 13, 15, 1, 3, 14, 5, 2, 8, 4, 3, 15, 0, 6, 10,
        10, 13, 8, 9, 4, 5, 11, 12, 7, 2, 14,
    ],
    [
        2, 12, 4, 1, 7, 10, 11, 6, 8, 5, 3, 15, 13, 0, 14, 9, 14, 11, 2, 12, 4, 7, 13, 1, 5, 0,
        15, 10, 3, 9, 8, 6, 4, 2, 1, 11, 10, 13, 7, 8, 15, 9, 12, 5, 6, 3, 0, 14, 11, 8, 12, 7, 1,
        14, 2, 13, 6, 15, 0, 9, 10, 4, 5, 3,
    ],
    [
        12, 1, 10, 15, 9, 2, 6, 8, 0, 13, 3, 4, 14, 7, 5, 11, 10, 15, 4, 2, 7, 12, 9, 5, 6, 1, 13,
        14, 0, 11, 3, 8, 9, 14, 15, 5, 2, 8, 12, 3, 7, 0, 4, 10, 1, 13, 11, 6, 4, 3, 2, 12, 9, 5,
        15, 10, 11, 14, 1, 7, 6, 0, 8, 13,
    ],
    [
        4, 11, 2, 14, 15, 0, 8, 13, 3, 12, 9, 7, 5, 10, 6, 1, 13, 0, 11, 7, 4, 9, 1, 10, 14, 3, 5,
        12, 2, 15, 8, 6, 1, 4, 11, 13, 12, 3, 7, 14, 10, 15, 6, 8, 0, 5, 9, 2, 6, 11, 13, 8, 1, 4,
        10, 7, 9, 5, 0, 15, 14, 2, 3, 12,
    ],
    [
        13, 2, 8, 4, 6, 15, 11, 1, 10, 9, 3, 14, 5, 0, 12, 7, 1, 15, 13, 8, 10, 3, 7, 4, 12, 5, 6,
        11, 0, 14, 9, 2, 7, 11, 4, 1, 9, 12, 14, 2, 0, 6, 10, 13, 15, 3, 5, 8, 2, 1, 14, 7, 4, 10,
        8, 13, 15, 12, 9, 0, 3, 5, 6, 11,
    ],
];

const KEY_RND_SHIFT: [u32; 16] = [1, 1, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1];

const KEY_PERM_C: [usize; 28] = [
    56, 48, 40, 32, 24, 16, 8, 0, 57, 49, 41, 33, 25, 17, 9, 1, 58, 50, 42, 34, 26, 18, 10, 2, 59,
    51, 43, 35,
];

const KEY_PERM_D: [usize; 28] = [
    62, 54, 46, 38, 30, 22, 14, 6, 61, 53, 45, 37, 29, 21, 13, 5, 60, 52, 44, 36, 28, 20, 12, 4,
    27, 19, 11, 3,
];

const KEY_COMPRESSION: [usize; 48] = [
    13, 16, 10, 23, 0, 4, 2, 27, 14, 5, 20, 9, 22, 18, 11, 3, 25, 7, 15, 6, 26, 19, 12, 1, 40, 51,
    30, 36, 46, 54, 29, 39, 50, 44, 32, 47, 43, 48, 38, 55, 33, 52, 45, 41, 49, 35, 28, 31,
];

/// Bit `b` of a big-endian 32-bit word view over `a`, shifted left by `c`.
fn bitnum(a: &[u8], b: usize, c: u32) -> u32 {
    let byte = a[(b / 32) * 4 + 3 - (b % 32) / 8];
    ((((byte >> (7 - (b % 8))) & 1) as u32) << c) as u32
}

/// Bit `b` counted from the most significant bit of `a`, shifted left by `c`.
fn bitnum_intr(a: u32, b: usize, c: u32) -> u32 {
    ((a >> (31 - b)) & 1) << c
}

/// Bit `b` counted from the least significant bit of `a`, placed at bit 31
/// then shifted right by `c`.
fn bitnum_intl(a: u32, b: usize, c: u32) -> u32 {
    ((a << b) & 0x8000_0000) >> c
}

fn sbox_bit(a: u32) -> usize {
    ((a & 0x20) | ((a & 0x1f) >> 1) | ((a & 0x01) << 4)) as usize
}

/// Six 32-bit subkey words per round, sixteen rounds.
type Schedule = [[u32; 6]; 16];

/// Expand a 24-byte three-key bundle into one 8-byte DES key schedule.
fn key_schedule(key: &[u8], decrypt: bool) -> Schedule {
    let mut schedule = [[0u32; 6]; 16];

    let mut c = 0u32;
    let mut d = 0u32;
    for (i, &perm) in KEY_PERM_C.iter().enumerate() {
        c |= bitnum(key, perm, 31 - i as u32);
    }
    for (i, &perm) in KEY_PERM_D.iter().enumerate() {
        d |= bitnum(key, perm, 31 - i as u32);
    }

    for i in 0..16 {
        let s = KEY_RND_SHIFT[i];
        c = ((c << s) | (c >> (28 - s))) & 0xffff_fff0;
        d = ((d << s) | (d >> (28 - s))) & 0xffff_fff0;

        let target = if decrypt { 15 - i } else { i };
        let mut j = 0usize;
        while j < 24 {
            schedule[target][j / 8] |= bitnum_intr(c, KEY_COMPRESSION[j], 7 - (j % 8) as u32);
            j += 1;
        }
        while j < 48 {
            schedule[target][j / 8] |= bitnum_intr(d, KEY_COMPRESSION[j] - 27, 7 - (j % 8) as u32);
            j += 1;
        }
    }
    schedule
}

/// Initial permutation of the eight plaintext bytes into two 32-bit halves.
fn initial_permutation(input: &[u8; 8]) -> [u32; 2] {
    let mut state = [0u32; 2];
    state[0] = bitnum(input, 57, 31)
        | bitnum(input, 49, 30)
        | bitnum(input, 41, 29)
        | bitnum(input, 33, 28)
        | bitnum(input, 25, 27)
        | bitnum(input, 17, 26)
        | bitnum(input, 9, 25)
        | bitnum(input, 1, 24)
        | bitnum(input, 59, 23)
        | bitnum(input, 51, 22)
        | bitnum(input, 43, 21)
        | bitnum(input, 35, 20)
        | bitnum(input, 27, 19)
        | bitnum(input, 19, 18)
        | bitnum(input, 11, 17)
        | bitnum(input, 3, 16)
        | bitnum(input, 61, 15)
        | bitnum(input, 53, 14)
        | bitnum(input, 45, 13)
        | bitnum(input, 37, 12)
        | bitnum(input, 29, 11)
        | bitnum(input, 21, 10)
        | bitnum(input, 13, 9)
        | bitnum(input, 5, 8)
        | bitnum(input, 63, 7)
        | bitnum(input, 55, 6)
        | bitnum(input, 47, 5)
        | bitnum(input, 39, 4)
        | bitnum(input, 31, 3)
        | bitnum(input, 23, 2)
        | bitnum(input, 15, 1)
        | bitnum(input, 7, 0);
    state[1] = bitnum(input, 56, 31)
        | bitnum(input, 48, 30)
        | bitnum(input, 40, 29)
        | bitnum(input, 32, 28)
        | bitnum(input, 24, 27)
        | bitnum(input, 16, 26)
        | bitnum(input, 8, 25)
        | bitnum(input, 0, 24)
        | bitnum(input, 58, 23)
        | bitnum(input, 50, 22)
        | bitnum(input, 42, 21)
        | bitnum(input, 34, 20)
        | bitnum(input, 26, 19)
        | bitnum(input, 18, 18)
        | bitnum(input, 10, 17)
        | bitnum(input, 2, 16)
        | bitnum(input, 60, 15)
        | bitnum(input, 52, 14)
        | bitnum(input, 44, 13)
        | bitnum(input, 36, 12)
        | bitnum(input, 28, 11)
        | bitnum(input, 20, 10)
        | bitnum(input, 12, 9)
        | bitnum(input, 4, 8)
        | bitnum(input, 62, 7)
        | bitnum(input, 54, 6)
        | bitnum(input, 46, 5)
        | bitnum(input, 38, 4)
        | bitnum(input, 30, 3)
        | bitnum(input, 22, 2)
        | bitnum(input, 14, 1)
        | bitnum(input, 6, 0);
    state
}

fn inverse_permutation(state: [u32; 2]) -> [u8; 8] {
    let [s0, s1] = state;
    let mut input = [0u8; 8];
    input[3] = (bitnum_intr(s1, 7, 7)
        | bitnum_intr(s0, 7, 6)
        | bitnum_intr(s1, 15, 5)
        | bitnum_intr(s0, 15, 4)
        | bitnum_intr(s1, 23, 3)
        | bitnum_intr(s0, 23, 2)
        | bitnum_intr(s1, 31, 1)
        | bitnum_intr(s0, 31, 0)) as u8;
    input[2] = (bitnum_intr(s1, 6, 7)
        | bitnum_intr(s0, 6, 6)
        | bitnum_intr(s1, 14, 5)
        | bitnum_intr(s0, 14, 4)
        | bitnum_intr(s1, 22, 3)
        | bitnum_intr(s0, 22, 2)
        | bitnum_intr(s1, 30, 1)
        | bitnum_intr(s0, 30, 0)) as u8;
    input[1] = (bitnum_intr(s1, 5, 7)
        | bitnum_intr(s0, 5, 6)
        | bitnum_intr(s1, 13, 5)
        | bitnum_intr(s0, 13, 4)
        | bitnum_intr(s1, 21, 3)
        | bitnum_intr(s0, 21, 2)
        | bitnum_intr(s1, 29, 1)
        | bitnum_intr(s0, 29, 0)) as u8;
    input[0] = (bitnum_intr(s1, 4, 7)
        | bitnum_intr(s0, 4, 6)
        | bitnum_intr(s1, 12, 5)
        | bitnum_intr(s0, 12, 4)
        | bitnum_intr(s1, 20, 3)
        | bitnum_intr(s0, 20, 2)
        | bitnum_intr(s1, 28, 1)
        | bitnum_intr(s0, 28, 0)) as u8;
    input[7] = (bitnum_intr(s1, 3, 7)
        | bitnum_intr(s0, 3, 6)
        | bitnum_intr(s1, 11, 5)
        | bitnum_intr(s0, 11, 4)
        | bitnum_intr(s1, 19, 3)
        | bitnum_intr(s0, 19, 2)
        | bitnum_intr(s1, 27, 1)
        | bitnum_intr(s0, 27, 0)) as u8;
    input[6] = (bitnum_intr(s1, 2, 7)
        | bitnum_intr(s0, 2, 6)
        | bitnum_intr(s1, 10, 5)
        | bitnum_intr(s0, 10, 4)
        | bitnum_intr(s1, 18, 3)
        | bitnum_intr(s0, 18, 2)
        | bitnum_intr(s1, 26, 1)
        | bitnum_intr(s0, 26, 0)) as u8;
    input[5] = (bitnum_intr(s1, 1, 7)
        | bitnum_intr(s0, 1, 6)
        | bitnum_intr(s1, 9, 5)
        | bitnum_intr(s0, 9, 4)
        | bitnum_intr(s1, 17, 3)
        | bitnum_intr(s0, 17, 2)
        | bitnum_intr(s1, 25, 1)
        | bitnum_intr(s0, 25, 0)) as u8;
    input[4] = (bitnum_intr(s1, 0, 7)
        | bitnum_intr(s0, 0, 6)
        | bitnum_intr(s1, 8, 5)
        | bitnum_intr(s0, 8, 4)
        | bitnum_intr(s1, 16, 3)
        | bitnum_intr(s0, 16, 2)
        | bitnum_intr(s1, 24, 1)
        | bitnum_intr(s0, 24, 0)) as u8;
    input
}

/// The DES round function with QQ's modified S-boxes.
fn f(state: u32, key: &[u32; 6]) -> u32 {
    let t1 = bitnum_intl(state, 31, 0)
        | ((state & 0xf000_0000) >> 1)
        | bitnum_intl(state, 4, 5)
        | bitnum_intl(state, 3, 6)
        | ((state & 0x0f00_0000) >> 3)
        | bitnum_intl(state, 8, 11)
        | bitnum_intl(state, 7, 12)
        | ((state & 0x00f0_0000) >> 5)
        | bitnum_intl(state, 12, 17)
        | bitnum_intl(state, 11, 18)
        | ((state & 0x000f_0000) >> 7)
        | bitnum_intl(state, 16, 23);

    let t2 = bitnum_intl(state, 15, 0)
        | ((state & 0x0000_f000) << 15)
        | bitnum_intl(state, 20, 5)
        | bitnum_intl(state, 19, 6)
        | ((state & 0x0000_0f00) << 13)
        | bitnum_intl(state, 24, 11)
        | bitnum_intl(state, 23, 12)
        | ((state & 0x0000_00f0) << 11)
        | bitnum_intl(state, 28, 17)
        | bitnum_intl(state, 27, 18)
        | ((state & 0x0000_000f) << 9)
        | bitnum_intl(state, 0, 23);

    let mut l = [
        ((t1 >> 24) & 0xff) as u8,
        ((t1 >> 16) & 0xff) as u8,
        ((t1 >> 8) & 0xff) as u8,
        ((t2 >> 24) & 0xff) as u8,
        ((t2 >> 16) & 0xff) as u8,
        ((t2 >> 8) & 0xff) as u8,
    ];
    for i in 0..6 {
        l[i] ^= key[i] as u8;
    }

    let s = ((SBOX[0][sbox_bit(((l[0] >> 2) & 0x3f) as u32)] as u32) << 28)
        | ((SBOX[1][sbox_bit((((l[0] & 0x03) << 4) | (l[1] >> 4)) as u32)] as u32) << 24)
        | ((SBOX[2][sbox_bit((((l[1] & 0x0f) << 2) | (l[2] >> 6)) as u32)] as u32) << 20)
        | ((SBOX[3][sbox_bit((l[2] & 0x3f) as u32)] as u32) << 16)
        | ((SBOX[4][sbox_bit((l[3] >> 2) as u32)] as u32) << 12)
        | ((SBOX[5][sbox_bit((((l[3] & 0x03) << 4) | (l[4] >> 4)) as u32)] as u32) << 8)
        | ((SBOX[6][sbox_bit((((l[4] & 0x0f) << 2) | (l[5] >> 6)) as u32)] as u32) << 4)
        | (SBOX[7][sbox_bit((l[5] & 0x3f) as u32)] as u32);

    bitnum_intl(s, 15, 0)
        | bitnum_intl(s, 6, 1)
        | bitnum_intl(s, 19, 2)
        | bitnum_intl(s, 20, 3)
        | bitnum_intl(s, 28, 4)
        | bitnum_intl(s, 11, 5)
        | bitnum_intl(s, 27, 6)
        | bitnum_intl(s, 16, 7)
        | bitnum_intl(s, 0, 8)
        | bitnum_intl(s, 14, 9)
        | bitnum_intl(s, 22, 10)
        | bitnum_intl(s, 25, 11)
        | bitnum_intl(s, 4, 12)
        | bitnum_intl(s, 17, 13)
        | bitnum_intl(s, 30, 14)
        | bitnum_intl(s, 9, 15)
        | bitnum_intl(s, 1, 16)
        | bitnum_intl(s, 7, 17)
        | bitnum_intl(s, 23, 18)
        | bitnum_intl(s, 13, 19)
        | bitnum_intl(s, 31, 20)
        | bitnum_intl(s, 26, 21)
        | bitnum_intl(s, 2, 22)
        | bitnum_intl(s, 8, 23)
        | bitnum_intl(s, 18, 24)
        | bitnum_intl(s, 12, 25)
        | bitnum_intl(s, 29, 26)
        | bitnum_intl(s, 5, 27)
        | bitnum_intl(s, 21, 28)
        | bitnum_intl(s, 10, 29)
        | bitnum_intl(s, 3, 30)
        | bitnum_intl(s, 24, 31)
}

fn crypt_block(input: &[u8; 8], key: &Schedule) -> [u8; 8] {
    let mut state = initial_permutation(input);
    for idx in 0..15 {
        let t = state[1];
        state[1] = f(state[1], &key[idx]) ^ state[0];
        state[0] = t;
    }
    state[0] = f(state[1], &key[15]) ^ state[0];
    inverse_permutation(state)
}

/// QQ's fixed three-key bundle (`QQ_KEY` in `qrc.ts`).
const QQ_KEY: &[u8; 24] = b"!@#)(*$%123ZXC!@!@#)(NHL";

/// Decrypt an 8-byte-aligned payload with QQ's 3DES variant.
///
/// The reference truncates any tail shorter than a block, so a 12-byte input
/// decrypts its first eight bytes and drops the rest.
pub fn triple_des_decrypt(data: &[u8]) -> Vec<u8> {
    let schedule = [
        key_schedule(&QQ_KEY[16..24], true), // D(K3)
        key_schedule(&QQ_KEY[8..16], false), // E(K2)
        key_schedule(&QQ_KEY[0..8], true),   // D(K1)
    ];

    let blocks = data.len() / 8;
    let mut out = Vec::with_capacity(blocks * 8);
    for i in 0..blocks {
        let mut block = [0u8; 8];
        block.copy_from_slice(&data[i * 8..i * 8 + 8]);
        let mut b = crypt_block(&block, &schedule[0]);
        b = crypt_block(&b, &schedule[1]);
        b = crypt_block(&b, &schedule[2]);
        out.extend_from_slice(&b);
    }
    out
}

#[cfg(test)]
mod krc_language_tests {
    use super::*;

    #[test]
    fn krc_separates_type0_romanization_from_type1_translation() {
        let payload = BASE64.encode(
            r#"{"content":[{"type":0,"lyricContent":[["gong","nei","gon"],["da","yin"]]}]}"#,
        );
        let krc = format!("[language:{payload}]\n[0,1000]共你干\n[1000,1000]突然\n");
        assert_eq!(krc_translations(&krc), None);
        assert_eq!(
            krc_romanizations(&krc),
            Some(vec!["gong nei gon".into(), "da yin".into()])
        );
    }
}
