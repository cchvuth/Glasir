//! FNV-1a 32-bit hash.
//!
//! Offset basis: `0x811c9dc5`, prime: `0x01000193`.
//!
//! See <https://en.wikipedia.org/wiki/Fowler%E2%80%93Noll%E2%80%93Vo_hash_function>

/// Returns the unsigned 32-bit FNV-1a hash of `input`.
///
/// The input is hashed by its UTF-16 code units (matching JS `charCodeAt`), so
/// the result is identical across the language ports even for non-ASCII or
/// non-BMP seeds.
pub fn hash(input: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;

    for code in input.encode_utf16() {
        hash ^= u32::from(code);
        hash = hash.wrapping_mul(0x0100_0193);
    }

    hash
}

/// Returns the FNV-1a hash of `input` as an 8-character lowercase hex string.
pub fn hex(input: &str) -> String {
    format!("{:08x}", hash(input))
}
