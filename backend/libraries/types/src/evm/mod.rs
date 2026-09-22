//! EVM values and the one transaction envelope this canister signs.
//!
//! The envelope is EIP-1559 (transaction type 2), written here rather than taken from a
//! consensus library, because its bytes are what a chain accepts and what an address is
//! derived against: `golden/evm_tx_v1.txt` pins them, and the unit tests pin them against
//! `cast mktx` and `cast keccak`.

#[cfg(test)]
mod tests;

use crate::chain::ChainId;
use crate::hash::TxHash;
use crate::numeric::{GasAmount, Nonce, Wei, WeiPerGas};
use alloy_primitives::keccak256;
use alloy_rlp::{BufMut, Encodable, Header};
use minicbor::decode::{self, Decoder};
use minicbor::encode::{self, Encoder, Write};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// The transaction type byte of an EIP-1559 transaction, which prefixes both the signing
/// payload and the raw bytes.
const EIP_1559_TYPE: u8 = 0x02;

/// A 20-byte account address on an EVM chain. Held as bytes, never as text, so nothing that
/// carries one can differ by case.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvmAddress([u8; 20]);

/// Why text is not an address.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum EvmAddressError {
    #[error("an address starts with 0x")]
    NoPrefix,
    #[error("an address is 40 hex digits, not {len}")]
    WrongLength { len: usize },
    #[error("an address is hex")]
    NotHex,
    #[error("the address carries a checksum that does not hold for its bytes")]
    BadChecksum,
}

impl EvmAddress {
    /// The address no account has, which a chain reads as a contract creation and this
    /// canister reads as no address at all.
    pub const ZERO: Self = Self([0; 20]);

    pub const fn new(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// The left-padded 32-byte word a rail carries an address in: CCTP's mint recipient and
    /// its destination caller are both this shape.
    pub fn to_word(self) -> [u8; 32] {
        let mut word = [0; 32];
        word[12..].copy_from_slice(&self.0);
        word
    }

    /// The address of a secp256k1 public key: the twenty low bytes of the keccak of the
    /// uncompressed key without its `0x04` prefix byte.
    pub fn from_public_key(uncompressed: &[u8; 65]) -> Self {
        let hash = keccak256(&uncompressed[1..]);
        let mut address = [0u8; 20];
        address.copy_from_slice(&hash[12..]);
        Self(address)
    }

    /// The EIP-55 mixed-case form: a digit's case is the corresponding nibble of the
    /// keccak of the lowercase digits.
    fn checksummed(&self) -> String {
        let lower = hex::encode(self.0);
        let hash = keccak256(lower.as_bytes());
        let mut out = String::with_capacity(42);
        out.push_str("0x");
        for (i, digit) in lower.chars().enumerate() {
            // the nibble of the hash that sits over this digit
            let nibble = hash[i / 2] >> (if i % 2 == 0 { 4 } else { 0 }) & 0x0f;
            if nibble >= 8 {
                out.extend(digit.to_uppercase());
            } else {
                out.push(digit);
            }
        }
        out
    }
}

/// Mixed case is a checksum and is verified; one case throughout is no checksum at all,
/// which is what every tool that writes an address in lower case produces.
impl FromStr for EvmAddress {
    type Err = EvmAddressError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let digits = text.strip_prefix("0x").ok_or(EvmAddressError::NoPrefix)?;
        if digits.len() != 40 {
            return Err(EvmAddressError::WrongLength { len: digits.len() });
        }
        let bytes: [u8; 20] = hex::decode(digits)
            .map_err(|_| EvmAddressError::NotHex)?
            .try_into()
            .expect("BUG: 40 hex digits decode to 20 bytes");
        let address = Self(bytes);
        let mixed =
            digits.chars().any(char::is_uppercase) && digits.chars().any(char::is_lowercase);
        if mixed && address.checksummed() != text {
            return Err(EvmAddressError::BadChecksum);
        }
        Ok(address)
    }
}

impl fmt::Display for EvmAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.checksummed())
    }
}

impl fmt::Debug for EvmAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.checksummed())
    }
}

impl AsRef<[u8]> for EvmAddress {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Stored as its twenty raw bytes, which is what it is.
impl<C> minicbor::Encode<C> for EvmAddress {
    fn encode<W: Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), encode::Error<W::Error>> {
        e.bytes(&self.0)?.ok()
    }
}

impl<'b, C> minicbor::Decode<'b, C> for EvmAddress {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, decode::Error> {
        let bytes = d.bytes()?;
        <[u8; 20]>::try_from(bytes).map(Self).map_err(|_| {
            decode::Error::message(format!(
                "EvmAddress: {} bytes, and an address is 20",
                bytes.len()
            ))
        })
    }
}

/// A secp256k1 signature over a transaction's signing hash, with the recovery bit the
/// envelope carries. `y_parity` is recovered rather than trusted: the threshold signer
/// answers r and s only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcdsaSignature {
    r: [u8; 32],
    s: [u8; 32],
    y_parity: bool,
}

impl EcdsaSignature {
    pub const fn new(r: [u8; 32], s: [u8; 32], y_parity: bool) -> Self {
        Self { r, s, y_parity }
    }

    pub const fn r(&self) -> &[u8; 32] {
        &self.r
    }

    pub const fn s(&self) -> &[u8; 32] {
        &self.s
    }

    pub const fn y_parity(&self) -> bool {
        self.y_parity
    }
}

/// An unsigned EIP-1559 transaction. There is no access list: this canister sends none, and
/// the empty list is part of the envelope it signs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Eip1559Tx {
    pub chain_id: ChainId,
    pub nonce: Nonce,
    pub max_fee: WeiPerGas,
    pub max_priority_fee: WeiPerGas,
    pub gas_limit: GasAmount,
    pub to: EvmAddress,
    pub value: Wei,
    pub data: Vec<u8>,
}

/// A 256-bit amount as RLP writes an integer: big-endian, without leading zeros, and zero
/// as no bytes at all. The same rule as [`trim_word`], which is the one implementation of
/// it, because a signature word and an amount are both a 32-byte integer here.
fn rlp_amount<Unit>(amount: crate::checked_amount::CheckedAmountOf<Unit>, out: &mut dyn BufMut) {
    trim_word(&amount.to_be_bytes()).encode(out);
}

impl Eip1559Tx {
    /// The transaction's fields in envelope order, as one RLP list, without a signature.
    fn rlp_fields(&self, out: &mut dyn BufMut) -> usize {
        let mut payload = Vec::new();
        self.chain_id.get().encode(&mut payload);
        self.nonce.get().encode(&mut payload);
        rlp_amount(self.max_priority_fee, &mut payload);
        rlp_amount(self.max_fee, &mut payload);
        rlp_amount(self.gas_limit, &mut payload);
        self.to.as_bytes().as_slice().encode(&mut payload);
        rlp_amount(self.value, &mut payload);
        self.data.as_slice().encode(&mut payload);
        // the access list, always empty
        Header {
            list: true,
            payload_length: 0,
        }
        .encode(&mut payload);
        out.put_slice(&payload);
        payload.len()
    }

    /// The bytes that are hashed to be signed: the type byte, then the RLP list of the
    /// nine unsigned fields.
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut fields = Vec::new();
        let payload_length = self.rlp_fields(&mut fields);
        let mut out = vec![EIP_1559_TYPE];
        Header {
            list: true,
            payload_length,
        }
        .encode(&mut out);
        out.extend_from_slice(&fields);
        out
    }

    /// What the threshold signer signs.
    pub fn signing_hash(&self) -> TxHash {
        TxHash::new(keccak256(self.signing_payload()).into())
    }

    /// The broadcastable transaction: the same nine fields, then the parity, r and s.
    /// Borrows rather than consumes, so a caller that has recorded the calldata can move it
    /// on to the outbox afterwards instead of keeping a copy for it.
    pub fn signed(&self, signature: EcdsaSignature) -> SignedTx {
        let mut fields = Vec::new();
        let mut payload_length = self.rlp_fields(&mut fields);
        let mut tail = Vec::new();
        signature.y_parity.encode(&mut tail);
        trim_word(&signature.r).encode(&mut tail);
        trim_word(&signature.s).encode(&mut tail);
        payload_length += tail.len();
        let mut raw = vec![EIP_1559_TYPE];
        Header {
            list: true,
            payload_length,
        }
        .encode(&mut raw);
        raw.extend_from_slice(&fields);
        raw.extend_from_slice(&tail);
        SignedTx {
            hash: TxHash::new(keccak256(&raw).into()),
            raw,
        }
    }
}

/// A signature word as RLP writes an integer: without its leading zeros.
fn trim_word(word: &[u8; 32]) -> &[u8] {
    let start = word.iter().position(|byte| *byte != 0).unwrap_or(32);
    &word[start..]
}

/// A signed transaction: the bytes to broadcast, and the hash a receipt is looked up by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedTx {
    raw: Vec<u8>,
    hash: TxHash,
}

impl SignedTx {
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// The bytes to broadcast, by value: the caller becomes their owner, so keeping them
    /// costs no copy.
    pub fn into_raw(self) -> Vec<u8> {
        self.raw
    }

    pub fn hash(&self) -> TxHash {
        self.hash
    }
}
