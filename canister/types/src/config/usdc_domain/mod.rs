//! USDC's EIP-712 domain on a chain: the name and version its contract answers (`name()`,
//! `version()`), which a 3009 authorization is signed under. Circle's USDC answers
//! "USD Coin" and "2" on the chains the canister launched on, and "USDC" on several newer
//! ones (Unichain, Monad, HyperEVM, World Chain, Plasma), so a chain's
//! entry names its own and an entry that names none reads as Circle's usual pair.

#[cfg(test)]
mod tests;

use minicbor::{Decode, Decoder, Encode, Encoder};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// The name a chain's USDC answers where its entry names none.
pub const DEFAULT_USDC_NAME: &str = "USD Coin";
/// The version a chain's USDC answers where its entry names none.
pub const DEFAULT_USDC_VERSION: &str = "2";
/// The most bytes a domain text holds.
pub const MAX_DOMAIN_TEXT: usize = 32;

/// An EIP-712 domain name or version: 1 to 32 printable ASCII characters, held inline so a
/// chain's entry stays `Copy`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DomainText {
    len: u8,
    bytes: [u8; MAX_DOMAIN_TEXT],
}

/// Why a text is no domain name or version.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("is not 1 to {MAX_DOMAIN_TEXT} printable ASCII characters")]
pub struct NotDomainText;

impl DomainText {
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)])
            .expect("BUG: a domain text is printable ASCII")
    }
}

impl FromStr for DomainText {
    type Err = NotDomainText;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let printable = text.bytes().all(|byte| (0x20..0x7f).contains(&byte));
        if text.is_empty() || text.len() > MAX_DOMAIN_TEXT || !printable {
            return Err(NotDomainText);
        }
        let mut bytes = [0; MAX_DOMAIN_TEXT];
        bytes[..text.len()].copy_from_slice(text.as_bytes());
        Ok(Self {
            len: u8::try_from(text.len()).expect("BUG: at most 32 bytes"),
            bytes,
        })
    }
}

impl fmt::Display for DomainText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for DomainText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DomainText({:?})", self.as_str())
    }
}

/// Stored as its text.
impl<C> Encode<C> for DomainText {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.str(self.as_str())?;
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for DomainText {
    fn decode(d: &mut Decoder<'b>, _: &mut C) -> Result<Self, minicbor::decode::Error> {
        d.str()?
            .parse()
            .map_err(|_| minicbor::decode::Error::message("a stored domain text is not one"))
    }
}
