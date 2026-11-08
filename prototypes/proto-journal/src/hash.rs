//! The two hashes a journal file uses for its DATA and FIELD objects
//! (https://systemd.io/JOURNAL_FILE_FORMAT/, "Data Objects"): siphash24
//! keyed by the file id when `HEADER_INCOMPATIBLE_KEYED_HASH` is set,
//! Jenkins lookup3 otherwise.

use siphasher::sip::SipHasher24;
use std::hash::Hasher;

/// siphash24 of `data`, keyed by the 16-byte file id, as systemd's
/// `siphash24(data, len, file_id.bytes)`: the key's two halves are read
/// little-endian, which is what `SipHasher24::new_with_key` does
/// (https://docs.rs/siphasher/1.0.2/siphasher/sip/struct.SipHasher24.html).
pub fn keyed(file_id: &[u8; 16], data: &[u8]) -> u64 {
    let mut h = SipHasher24::new_with_key(file_id);
    h.write(data);
    h.finish()
}

/// systemd's `jenkins_hash64`: lookup3's `hashlittle2` with both seeds 0,
/// returning `(c << 32) | b`.
///
/// Why our own: `jenkins_hash` 0.2.0 has only lookup2. `hashers` 1.0.1's
/// `jenkins::Lookup3Hasher` does implement hashlittle2, but it has had no
/// release since 2018, returns the two halves swapped against systemd's
/// `(c << 32) | b`, and returns 0 for empty input instead of lookup3's
/// value. Thirty lines of a public-domain function, checked against the
/// reference vectors and against every hash journald stored, are less risk
/// than that. This is a port of the byte-at-a-time branch of
/// `jenkins_hashlittle2` in systemd v255
/// src/libsystemd/sd-journal/lookup3.c (Bob Jenkins, public domain); on a
/// little-endian host the word-reading branches give the same result. Tested
/// against the hashes stored in real unkeyed journal files (hash-check).
pub fn jenkins64(data: &[u8]) -> u64 {
    let len = data.len();
    // The reference adds the length as uint32_t: lengths wrap at 2^32.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "lookup3 adds (uint32_t)length"
    )]
    let init = 0xdead_beef_u32.wrapping_add(len as u32);
    let (mut a, mut b, mut c) = (init, init, init);

    let mut k = data;
    while k.len() > 12 {
        a = a.wrapping_add(word(&k[0..4]));
        b = b.wrapping_add(word(&k[4..8]));
        c = c.wrapping_add(word(&k[8..12]));
        mix(&mut a, &mut b, &mut c);
        k = &k[12..];
    }
    if k.is_empty() {
        return (u64::from(c) << 32) | u64::from(b);
    }
    // The last block, zero-padded: the reference's fall-through switch adds
    // exactly the bytes present.
    let mut tail = [0u8; 12];
    tail[..k.len()].copy_from_slice(k);
    a = a.wrapping_add(word(&tail[0..4]));
    b = b.wrapping_add(word(&tail[4..8]));
    c = c.wrapping_add(word(&tail[8..12]));
    finalize(&mut a, &mut b, &mut c);
    (u64::from(c) << 32) | u64::from(b)
}

fn word(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn mix(a: &mut u32, b: &mut u32, c: &mut u32) {
    *a = a.wrapping_sub(*c);
    *a ^= c.rotate_left(4);
    *c = c.wrapping_add(*b);
    *b = b.wrapping_sub(*a);
    *b ^= a.rotate_left(6);
    *a = a.wrapping_add(*c);
    *c = c.wrapping_sub(*b);
    *c ^= b.rotate_left(8);
    *b = b.wrapping_add(*a);
    *a = a.wrapping_sub(*c);
    *a ^= c.rotate_left(16);
    *c = c.wrapping_add(*b);
    *b = b.wrapping_sub(*a);
    *b ^= a.rotate_left(19);
    *a = a.wrapping_add(*c);
    *c = c.wrapping_sub(*b);
    *c ^= b.rotate_left(4);
    *b = b.wrapping_add(*a);
}

fn finalize(a: &mut u32, b: &mut u32, c: &mut u32) {
    *c ^= *b;
    *c = c.wrapping_sub(b.rotate_left(14));
    *a ^= *c;
    *a = a.wrapping_sub(c.rotate_left(11));
    *b ^= *a;
    *b = b.wrapping_sub(a.rotate_left(25));
    *c ^= *b;
    *c = c.wrapping_sub(b.rotate_left(16));
    *a ^= *c;
    *a = a.wrapping_sub(c.rotate_left(4));
    *b ^= *a;
    *b = b.wrapping_sub(a.rotate_left(14));
    *c ^= *b;
    *c = c.wrapping_sub(b.rotate_left(24));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// lookup3.c's own self-test (driver5): `hashlittle2("", 0, &c=0, &b=0)`
    /// gives c = b = 0xdeadbeef, and "Four score and seven years ago"
    /// with both seeds 0 gives c = 0x17770551, b = 0xce7226e6.
    #[test]
    fn lookup3_reference_vectors() {
        assert_eq!(jenkins64(b""), 0xdead_beef_dead_beef);
        assert_eq!(
            jenkins64(b"Four score and seven years ago"),
            (0x1777_0551_u64 << 32) | 0xce72_26e6
        );
    }
}
