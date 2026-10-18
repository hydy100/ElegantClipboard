// SPDX-License-Identifier: LGPL-3.0-only
// Rust adaptation of rany2/edge-tts src/edge_tts/drm.py (v2 token algorithm).
// Source: https://github.com/rany2/edge-tts/blob/master/src/edge_tts/drm.py
// License and attribution: ../../../../licenses/edge-tts-LGPL-3.0.txt
// All protocol arithmetic is explicitly modulo 2^32, including debug builds.
use std::fmt::Write;

fn sigma0(x: u32) -> u32 {
    x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3)
}
fn sigma1(x: u32) -> u32 {
    x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)
}

fn expand(words: &mut [u32; 64], start: usize) {
    for i in start..64 {
        words[i] = sigma1(words[i - 2])
            .wrapping_add(words[i - 7])
            .wrapping_add(sigma0(words[i - 15]))
            .wrapping_add(words[i - 16]);
    }
}

fn rounds(state: [u32; 8], words: &[u32; 64], start: usize) -> [u32; 8] {
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
    for i in start..64 {
        let constant = if start == 14 && i == 14 {
            0xC9DC06A7
        } else {
            SHA256_ROUND_CONSTANTS[i]
        };
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add(g ^ (e & (f ^ g)))
            .wrapping_add(constant)
            .wrapping_add(words[i]);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) | (c & (a | b)));
        [a, b, c, d, e, f, g, h] = [t1.wrapping_add(t2), a, b, c, d.wrapping_add(t1), e, f, g];
    }
    [a, b, c, d, e, f, g, h]
}

fn digest(data: &[u8]) -> [u8; 32] {
    // Resume the service-specific first SHA block, then process normal blocks.
    let mut words = [0u32; 64];
    words[14] = u32::from_be_bytes([0, data[0], data[1], data[2]]);
    words[15] = u32::from_be_bytes(data[3..7].try_into().unwrap());
    for i in 16..30 {
        let mut value = SHA256_SCHEDULE[i - 16];
        if i >= 16 {
            value = value.wrapping_add(sigma1(words[i - 2]) ^ if i == 16 { 0x000B92C0 } else { 0 });
        }
        if i >= 21 {
            value = value.wrapping_add(words[i - 7]);
        }
        if i >= 29 {
            value =
                value.wrapping_add(sigma0(words[i - 15]) ^ if i == 29 { 0x059C0B80 } else { 0 });
        }
        words[i] = value;
    }
    words[30] = sigma1(words[28])
        .wrapping_add(words[23])
        .wrapping_add(sigma0(words[15]))
        .wrapping_add(words[14])
        .wrapping_add(0xC9DC06A7)
        .wrapping_sub(SHA256_ROUND_CONSTANTS[14]);
    expand(&mut words, 31);
    let working = rounds(SHA256_STATE, &words, 14);
    let mut state = std::array::from_fn(|i| SHA256_INITIAL[i].wrapping_add(working[i]));
    let length = 57 + data.len();
    let mut padded = data[7..].to_vec();
    padded.push(0x80);
    padded.resize(padded.len() + (119 - length % 64) % 64, 0);
    padded.extend_from_slice(&((length as u64) * 8).to_be_bytes());
    for block in padded.chunks_exact(64) {
        for (i, word) in block.chunks_exact(4).enumerate() {
            words[i] = u32::from_be_bytes(word.try_into().unwrap());
        }
        expand(&mut words, 16);
        let working = rounds(state, &words, 0);
        state = std::array::from_fn(|i| state[i].wrapping_add(working[i]));
    }
    let mut result = [0u8; 32];
    for (chunk, value) in result.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    result
}

fn cs64(data: &[u8], digest: &[u8; 32]) -> [u8; 8] {
    let a = u32::from_le_bytes(digest[..4].try_into().unwrap()) | 1;
    let b = u32::from_le_bytes(digest[4..8].try_into().unwrap()) | 1;
    let factors = [a.wrapping_mul(0x82893933), b.wrapping_mul(0x82893933)];
    let mut terms: Vec<_> = CS64_COEFFICIENTS
        .iter()
        .enumerate()
        .map(|(i, (v, _))| v.wrapping_mul(factors[i % 2]))
        .collect();
    let mut offsets: Vec<_> = CS64_COEFFICIENTS.iter().map(|(_, v)| *v).collect();
    let mut padded = vec![0u8];
    padded.extend_from_slice(data);
    padded.resize(padded.len().div_ceil(8) * 8, 0);
    for (i, chunk) in padded.chunks_exact(4).enumerate() {
        let word = u32::from_le_bytes(chunk.try_into().unwrap());
        let mut term = word.wrapping_mul(if i % 2 == 0 { a } else { b });
        let mut offset =
            word.rotate_left(16)
                .wrapping_mul(if i % 2 == 0 { 0xEF0569FB } else { 0xC31713DB });
        if i == 0 {
            term = term.wrapping_add(0xF2F90B1Au32.wrapping_mul(factors[0]));
            offset = offset.wrapping_add(0x0B1A0000);
        }
        terms.push(term);
        offsets.push(offset);
    }
    let (mut swap_acc, mut swap_last, mut state, mut acc) = (0u32, 0u32, 0u32, 0u32);
    for i in (0..terms.len()).step_by(2) {
        let (x, y) = (terms[i], terms[i + 1]);
        let q = offsets[i]
            .wrapping_add(x)
            .rotate_left(16)
            .wrapping_mul(0x76F50F34);
        swap_acc =
            swap_acc.wrapping_add(q.rotate_left(16).wrapping_mul(0xC3EFEA97).wrapping_add(q));
        let q = offsets[i + 1]
            .wrapping_add(y)
            .rotate_left(16)
            .wrapping_mul(0x1881CE3C);
        swap_last = q.rotate_left(16).wrapping_mul(0x35BD1EC9).wrapping_add(q);
        swap_acc = swap_acc.wrapping_add(swap_last);
        state = state.wrapping_mul(a).wrapping_add(x);
        for coefficient in [0xCF98B111, 0x87085B9F, 0x12CEB96D, 0x257E1D83] {
            state = state.rotate_left(16).wrapping_mul(coefficient);
        }
        acc = acc.wrapping_add(state);
        let mut t = state.wrapping_mul(b).wrapping_add(y);
        for coefficient in [0xA27416F5, 0xD38396FF, 0x7C932B89, 0xBFA49F69] {
            t = t.rotate_left(16).wrapping_mul(coefficient);
        }
        state = t;
        acc = acc.wrapping_add(state);
    }
    let mut result = [0u8; 8];
    result[..4].copy_from_slice(&(swap_last ^ state).to_le_bytes());
    result[4..].copy_from_slice(&(swap_acc ^ acc).to_le_bytes());
    result
}

pub(super) fn generate(unix_seconds: i64) -> String {
    let seconds = unix_seconds + 11_644_473_600;
    let ticks = seconds.div_euclid(300) * 300 * 10_000_000;
    let data = format!("{ticks}{TOKEN}");
    let hash = digest(data.as_bytes());
    let checksum = cs64(data.as_bytes(), &hash);
    let mut result = String::with_capacity(81);
    for byte in checksum {
        write!(&mut result, "{byte:02X}").unwrap();
    }
    result.push('-');
    for byte in hash {
        write!(&mut result, "{byte:02X}").unwrap();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reference_v2_vectors() {
        for (time, expected) in [
            (
                0,
                "52BC7A17B50DFEC9-28C4394775C8751CD2F6859C030C06DB1489ACAB3B3A7651831FC26895177ED6",
            ),
            (
                1700000000,
                "2CD23A2CA0B2595A-F3C846C370E05F9CC83ACC5F995C5F5D87A4D0F14CE8936A0F902E64099E38A2",
            ),
            (
                1700000100,
                "0919602B5750900A-EC9DD394F23CB5635CF62A0E8E24136DCD32B26D10C543B10082E482D7516397",
            ),
            (
                1792300000,
                "F73DDD420DC92325-99D8468B2E25E9AF441D1B6B1CAADFB28628D348EB052DA57DB20027C2EBF842",
            ),
            (
                2000000000,
                "2D62557F1142BC5D-FD9C30C68FF194A5EE6B45FEFC5E2BA1D1239F88A90569AB55CFFFAA7799F40E",
            ),
        ] {
            assert_eq!(generate(time), expected, "timestamp {time}");
        }
    }

    #[test]
    fn five_minute_buckets_and_v2_version_are_consistent() {
        assert_eq!(generate(1700000000), generate(1700000099));
        assert_ne!(generate(1700000099), generate(1700000100));
        assert_eq!(generate(1700000000).len(), 81);
        assert!(VERSION.starts_with("2-"));
    }
}

pub(super) const TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
pub(super) const VERSION: &str = "2-143.0.3650.75";
const SHA256_INITIAL: [u32; 8] = [
    0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A, 0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19,
];
const SHA256_STATE: [u32; 8] = [
    0xFD9FBEA2, 0x91243CE7, 0xBE499094, 0xCD2E1E0A, 0x6C006F49, 0x04BD35AB, 0xFEB0A7E4, 0x03EFFBD2,
];
const SHA256_SCHEDULE: [u32; 14] = [
    0xEB24BF9E, 0x1558691F, 0xA0C09EE2, 0x0CFA32BA, 0xA622EE8C, 0xD1DD9491, 0xC0DD25A0, 0xF9E70AB6,
    0xF0C31573, 0x7058CE4E, 0x7188E606, 0x00E5AB33, 0xD258B060, 0x72766564,
];
const SHA256_ROUND_CONSTANTS: [u32; 64] = [
    0x428A2F98, 0x71374491, 0xB5C0FBCF, 0xE9B5DBA5, 0x3956C25B, 0x59F111F1, 0x923F82A4, 0xAB1C5ED5,
    0xD807AA98, 0x12835B01, 0x243185BE, 0x550C7DC3, 0x72BE5D74, 0x80DEB1FE, 0x9BDC06A7, 0xC19BF174,
    0xE49B69C1, 0xEFBE4786, 0x0FC19DC6, 0x240CA1CC, 0x2DE92C6F, 0x4A7484AA, 0x5CB0A9DC, 0x76F988DA,
    0x983E5152, 0xA831C66D, 0xB00327C8, 0xBF597FC7, 0xC6E00BF3, 0xD5A79147, 0x06CA6351, 0x14292967,
    0x27B70A85, 0x2E1B2138, 0x4D2C6DFC, 0x53380D13, 0x650A7354, 0x766A0ABB, 0x81C2C92E, 0x92722C85,
    0xA2BFE8A1, 0xA81A664B, 0xC24B8B70, 0xC76C51A3, 0xD192E819, 0xD6990624, 0xF40E3585, 0x106AA070,
    0x19A4C116, 0x1E376C08, 0x2748774C, 0x34B0BCB5, 0x391C0CB3, 0x4ED8AA4A, 0x5B9CCA4F, 0x682E6FF3,
    0x748F82EE, 0x78A5636F, 0x84C87814, 0x8CC70208, 0x90BEFFFA, 0xA4506CEB, 0xBEF9A3F7, 0xC67178F2,
];
const CS64_COEFFICIENTS: [(u32, u32); 14] = [
    (0xE82E91B1, 0xB32700D0),
    (0x44DE24C6, 0x88DFF51D),
    (0x3BEB65BC, 0x29514038),
    (0xC89C5933, 0xB328CBDF),
    (0x8D1EC211, 0xD405B4D5),
    (0x314EF5D5, 0x26E55F3C),
    (0xE92491B1, 0x607801C6),
    (0xF0A5B9D5, 0x82B2C1FB),
    (0x06064CF3, 0xFA51A3DA),
    (0xDA79FA60, 0x207CC464),
    (0xBED20560, 0x3C3C74F3),
    (0x89F9C9F8, 0xD2D14B61),
    (0x35FA38C6, 0xBB04A2C1),
    (0xCB49E3C6, 0xC5626167),
];
