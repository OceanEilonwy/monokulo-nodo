//! Monero address checking: format, network prefix and checksum.
//!
//! A mistyped payout address loses funds, so a setting of this type verifies the checksum rather
//! than only the shape of the string.

use tiny_keccak::{Hasher, Keccak};

const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const FULL_BLOCK_CHARS: usize = 11;
const FULL_BLOCK_BYTES: usize = 8;
const CHECKSUM_BYTES: usize = 4;
/// Prefix, public spend key and public view key.
const STANDARD_BYTES: usize = 1 + 64 + CHECKSUM_BYTES;
/// As above, plus an 8 byte payment id.
const INTEGRATED_BYTES: usize = STANDARD_BYTES + 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoneroNetwork {
    Mainnet,
    Stagenet,
    Testnet,
}

/// Bytes per encoded length, for the short final block. Other lengths are invalid.
fn block_bytes(chars: usize) -> Option<usize> {
    match chars {
        2 => Some(1),
        3 => Some(2),
        5 => Some(3),
        6 => Some(4),
        7 => Some(5),
        9 => Some(6),
        10 => Some(7),
        11 => Some(8),
        _ => None,
    }
}

fn decode_block(block: &[u8], out: &mut Vec<u8>) -> Option<()> {
    let size = block_bytes(block.len())?;
    let mut value: u64 = 0;
    for symbol in block {
        let digit = ALPHABET.iter().position(|a| a == symbol)? as u64;
        value = value.checked_mul(58)?.checked_add(digit)?;
    }
    if size < FULL_BLOCK_BYTES && value >> (size * 8) != 0 {
        return None;
    }
    out.extend_from_slice(&value.to_be_bytes()[FULL_BLOCK_BYTES - size..]);
    Some(())
}

fn decode_base58(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(INTEGRATED_BYTES);
    for block in text.as_bytes().chunks(FULL_BLOCK_CHARS) {
        decode_block(block, &mut out)?;
    }
    Some(out)
}

/// Checks an address and reports which network it belongs to.
pub fn check_monero_address(address: &str) -> Result<MoneroNetwork, &'static str> {
    if address.len() != 95 && address.len() != 106 {
        return Err("a Monero address has 95 or 106 characters");
    }
    let bytes = decode_base58(address).ok_or("contains characters that are not valid in a Monero address")?;
    if bytes.len() != STANDARD_BYTES && bytes.len() != INTEGRATED_BYTES {
        return Err("has the wrong length for a Monero address");
    }
    let (payload, checksum) = bytes.split_at(bytes.len() - CHECKSUM_BYTES);
    let mut keccak = Keccak::v256();
    keccak.update(payload);
    let mut hash = [0u8; 32];
    keccak.finalize(&mut hash);
    if hash[..CHECKSUM_BYTES] != *checksum {
        return Err("has an incorrect checksum, so it was probably mistyped");
    }
    // Standard and subaddress prefixes have the standard length, integrated ones the longer length.
    let integrated = bytes.len() == INTEGRATED_BYTES;
    match (bytes[0], integrated) {
        (18 | 42, false) | (19, true) => Ok(MoneroNetwork::Mainnet),
        (24 | 36, false) | (25, true) => Ok(MoneroNetwork::Stagenet),
        (53 | 63, false) | (54, true) => Ok(MoneroNetwork::Testnet),
        _ => Err("does not have a known Monero address prefix"),
    }
}

/// Test support: builds addresses with a valid checksum, since real ones are few and long.
#[cfg(test)]
pub(crate) fn encode_address(prefix: u8, body_len: usize) -> String {
    let mut bytes = vec![prefix];
    bytes.extend((0..body_len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(7)));
    let mut keccak = Keccak::v256();
    keccak.update(&bytes);
    let mut hash = [0u8; 32];
    keccak.finalize(&mut hash);
    bytes.extend_from_slice(&hash[..CHECKSUM_BYTES]);

    let mut text = String::new();
    for block in bytes.chunks(FULL_BLOCK_BYTES) {
        let chars = (2..=FULL_BLOCK_CHARS).find(|c| block_bytes(*c) == Some(block.len())).unwrap();
        let mut value = block.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
        let mut encoded = vec![ALPHABET[0]; chars];
        for slot in encoded.iter_mut().rev() {
            *slot = ALPHABET[(value % 58) as usize];
            value /= 58;
        }
        text.push_str(std::str::from_utf8(&encoded).unwrap());
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Monero project's published donation address, an independent check on the codec.
    const REAL: &str = "888tNkZrPN6JsEgekjMnABU4TBzc2Dt29EPAvkRxbANsAnjyPbb3iQ1YBRk1UXcdRsiKc9dhwMVgN5S9cQUiyoogDavup3H";

    #[test]
    fn accepts_a_real_address() {
        assert_eq!(check_monero_address(REAL), Ok(MoneroNetwork::Mainnet));
    }

    #[test]
    fn identifies_networks() {
        for (prefix, body, network) in [
            (18, 64, MoneroNetwork::Mainnet),
            (42, 64, MoneroNetwork::Mainnet),
            (19, 72, MoneroNetwork::Mainnet),
            (24, 64, MoneroNetwork::Stagenet),
            (36, 64, MoneroNetwork::Stagenet),
            (25, 72, MoneroNetwork::Stagenet),
            (53, 64, MoneroNetwork::Testnet),
            (63, 64, MoneroNetwork::Testnet),
            (54, 72, MoneroNetwork::Testnet),
        ] {
            assert_eq!(check_monero_address(&encode_address(prefix, body)), Ok(network), "prefix {prefix}");
        }
    }

    #[test]
    fn rejects_unknown_prefixes_and_mismatched_lengths() {
        assert!(check_monero_address(&encode_address(0, 64)).is_err());
        assert!(check_monero_address(&encode_address(18, 72)).is_err(), "standard prefix, integrated length");
        assert!(check_monero_address(&encode_address(19, 64)).is_err(), "integrated prefix, standard length");
    }

    #[test]
    fn rejects_a_single_changed_character() {
        for at in [0, 1, 40, 94] {
            let mut bytes = REAL.as_bytes().to_vec();
            bytes[at] = if bytes[at] == b'2' { b'3' } else { b'2' };
            let changed = String::from_utf8(bytes).unwrap();
            assert!(check_monero_address(&changed).is_err(), "changed at {at}");
        }
    }

    #[test]
    fn rejects_malformed_text() {
        let long_zzz = "z".repeat(95);
        let with_bad_char = format!("0{}", &REAL[1..]);
        let with_unicode = format!("é{}", &REAL[2..]);
        for bad in ["", "abc", &REAL[..94], long_zzz.as_str(), with_bad_char.as_str(), with_unicode.as_str()] {
            assert!(check_monero_address(bad).is_err(), "{bad:?}");
        }
    }
}
