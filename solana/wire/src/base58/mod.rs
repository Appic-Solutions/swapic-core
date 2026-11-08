//! Base58 text for addresses and signatures. Text is case sensitive: never lowercase it.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

pub fn encode(bytes: &[u8]) -> String {
    let zeros = bytes.iter().take_while(|b| **b == 0).count();
    let mut digits: Vec<u8> = Vec::with_capacity(bytes.len() * 138 / 100 + 1);
    for &byte in &bytes[zeros..] {
        let mut carry = byte as u32;
        for digit in digits.iter_mut() {
            let value = (*digit as u32) << 8 | carry;
            *digit = (value % 58) as u8;
            carry = value / 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut out = String::with_capacity(zeros + digits.len());
    out.extend(core::iter::repeat('1').take(zeros));
    out.extend(digits.iter().rev().map(|d| ALPHABET[*d as usize] as char));
    out
}

pub fn decode(text: &str) -> Option<Vec<u8>> {
    let zeros = text.bytes().take_while(|b| *b == b'1').count();
    let mut bytes: Vec<u8> = Vec::with_capacity(text.len());
    for c in text.bytes().skip(zeros) {
        let mut carry = ALPHABET.iter().position(|a| *a == c)? as u32;
        for byte in bytes.iter_mut() {
            let value = *byte as u32 * 58 + carry;
            *byte = value as u8;
            carry = value >> 8;
        }
        while carry > 0 {
            bytes.push(carry as u8);
            carry >>= 8;
        }
    }
    let mut out = vec![0u8; zeros];
    out.extend(bytes.iter().rev());
    Some(out)
}

/// A 32-byte address from its text.
pub fn decode_pubkey(text: &str) -> Option<crate::Pubkey> {
    decode(text)?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::{TOKEN_PROGRAM, USDC_MINT};

    #[test]
    fn round_trips_addresses_and_leading_zeros() {
        assert_eq!(encode(&USDC_MINT), "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        assert_eq!(decode_pubkey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"), Some(TOKEN_PROGRAM));
        assert_eq!(encode(&[0u8; 32]), "11111111111111111111111111111111");
        assert_eq!(decode_pubkey("11111111111111111111111111111111"), Some([0u8; 32]));
        let mut signature = [7u8; 64];
        signature[0] = 0;
        assert_eq!(decode(&encode(&signature)).unwrap(), signature.to_vec());
    }

    #[test]
    fn refuses_foreign_characters_and_wrong_lengths() {
        assert_eq!(decode("0OIl"), None);
        assert_eq!(decode_pubkey("abc"), None);
    }
}
