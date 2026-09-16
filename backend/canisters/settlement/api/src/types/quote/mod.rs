use crate::types::codec::put_str;
use crate::types::events::Hash32;
use candid::CandidType;
use serde::Deserialize;
use sha2::Digest;

/// The only layout this canister speaks. The version byte is the first byte of the
/// preimage, so a future layout is a new parser and a new golden file, never a branch
/// inside this one.
pub const QUOTE_VERSION: u8 = 1;

// update together with the struct; the exhaustive destructure in `quote_bytes` is the
// compile-time check, this is the test-coverage check
pub const QUOTE_FIELD_COUNT: usize = 15;

/// The longest any string field of a quote may be, in bytes. Generous for any address or
/// rail id, and it bounds what one pending entry can hold.
pub const MAX_QUOTE_STRING_BYTES: usize = 256;

/// Who pays the source-side gas. On the wire it is one byte, Gasless 0 and Legacy 1;
/// `quote_bytes` is the one place that mapping is written.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum GasMode {
    Gasless,
    Legacy,
}

/// What the off-chain quoter handed the user, and the only thing the settlement canister
/// ever hashes into a swap id. The hash is sha256 over a canonical preimage whose field
/// order is frozen and is NOT the order a candid tool prints this record in:
///
/// `version u8 | src_chain u64-be | src_token (u32-be len + utf8) | amount_in u128-be |
/// dst_chain u64 | dst_token | expected_out u128 | min_out u128 | dst_address |
/// refund_address (None encodes as empty) | auto_refund u8 |
/// gas_mode u8 (Gasless=0, Legacy=1) | rail | expires_at_s u64 | nonce u64`
///
/// Every integer is big-endian, every string is a u32-be byte length then utf8, and each
/// of the two one-byte fields is 0 or 1. Reproduce those bytes and you reproduce the
/// hash; `backend/canisters/settlement/api/golden/quote_hash_v1.txt` is the vector to
/// check a reimplementation against.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub struct Quote {
    pub version: u8,
    pub src_chain: u64,
    pub src_token: String,
    pub amount_in: u128,
    pub dst_chain: u64,
    pub dst_token: String,
    pub expected_out: u128,
    pub min_out: u128,
    pub dst_address: String,
    pub refund_address: Option<String>,
    pub auto_refund: bool,
    pub gas_mode: GasMode,
    pub rail: String,
    pub expires_at_s: u64,
    pub nonce: u64,
}

impl Quote {
    /// What a quote must satisfy before the canister will hold on to it. Called at the
    /// `register` chokepoint, so nothing that fails here reaches the pending store.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != QUOTE_VERSION {
            return Err(format!(
                "version is {}, and this canister reads layout v{QUOTE_VERSION}",
                self.version
            ));
        }
        // the one collision in the layout, closed at the door: an empty refund address
        // writes the same bytes as no refund address, so only one of the two is accepted
        if self.refund_address.as_deref() == Some("") {
            return Err(
                "refund_address is an empty string: leave it absent to mean no refund address"
                    .to_string(),
            );
        }
        for (field, value) in [
            ("src_token", self.src_token.as_str()),
            ("dst_token", self.dst_token.as_str()),
            ("dst_address", self.dst_address.as_str()),
            ("rail", self.rail.as_str()),
            (
                "refund_address",
                self.refund_address.as_deref().unwrap_or(""),
            ),
        ] {
            if value.len() > MAX_QUOTE_STRING_BYTES {
                return Err(format!(
                    "{field} is {} bytes, above the cap of {MAX_QUOTE_STRING_BYTES}",
                    value.len()
                ));
            }
        }
        Ok(())
    }
}

/// The canonical hash preimage, and a cross-repo contract: swapic-backend's quoter builds
/// these bytes on its own side and must match byte for byte. Ints big-endian, u128 in 16
/// bytes, strings u32-be length then utf8, bool and gas mode one byte each. The field
/// order below is frozen; changing it, or any width, is a breaking change everywhere.
/// `parse_quote` is its exact inverse.
pub fn quote_bytes(q: &Quote) -> Vec<u8> {
    // exhaustive destructure: a new field breaks this line, forcing a layout decision
    // instead of silently staying out of the hash
    let Quote {
        version,
        src_chain,
        src_token,
        amount_in,
        dst_chain,
        dst_token,
        expected_out,
        min_out,
        dst_address,
        refund_address,
        auto_refund,
        gas_mode,
        rail,
        expires_at_s,
        nonce,
    } = q;
    let mut b = Vec::new();
    b.push(*version);
    b.extend_from_slice(&src_chain.to_be_bytes());
    put_str(&mut b, src_token);
    b.extend_from_slice(&amount_in.to_be_bytes());
    b.extend_from_slice(&dst_chain.to_be_bytes());
    put_str(&mut b, dst_token);
    b.extend_from_slice(&expected_out.to_be_bytes());
    b.extend_from_slice(&min_out.to_be_bytes());
    put_str(&mut b, dst_address);
    // absent is the empty string; `validate` refuses an explicitly empty one so the two
    // never both reach a stored quote
    put_str(&mut b, refund_address.as_deref().unwrap_or(""));
    b.push(u8::from(*auto_refund));
    // written as a match, not `as u8`, so reordering the enum cannot move the wire value
    b.push(match gas_mode {
        GasMode::Gasless => 0,
        GasMode::Legacy => 1,
    });
    put_str(&mut b, rail);
    b.extend_from_slice(&expires_at_s.to_be_bytes());
    b.extend_from_slice(&nonce.to_be_bytes());
    b
}

/// The swap id: sha256 over the canonical preimage and nothing else, so anyone holding
/// the quote can recompute it.
pub fn quote_hash(q: &Quote) -> Hash32 {
    sha2::Sha256::digest(quote_bytes(q)).into()
}

/// Reads the canonical layout back. Strict on purpose: every byte must be one the writer
/// could have produced, so `parse ∘ encode` is the identity on quotes and `encode ∘ parse`
/// is the identity on everything it accepts.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize, field: &str) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(n)
            .ok_or_else(|| format!("{field}: length {n} overflows"))?;
        let out = self.bytes.get(self.at..end).ok_or_else(|| {
            format!(
                "{field}: truncated at byte {}, wanted {n} more of {}",
                self.at,
                self.bytes.len()
            )
        })?;
        self.at = end;
        Ok(out)
    }

    fn u8(&mut self, field: &str) -> Result<u8, String> {
        Ok(self.take(1, field)?[0])
    }

    fn u64(&mut self, field: &str) -> Result<u64, String> {
        let b: [u8; 8] = self.take(8, field)?.try_into().expect("8 bytes");
        Ok(u64::from_be_bytes(b))
    }

    fn u128(&mut self, field: &str) -> Result<u128, String> {
        let b: [u8; 16] = self.take(16, field)?.try_into().expect("16 bytes");
        Ok(u128::from_be_bytes(b))
    }

    fn bool(&mut self, field: &str) -> Result<bool, String> {
        match self.u8(field)? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(format!("{field}: {other} is not a bool")),
        }
    }

    fn string(&mut self, field: &str) -> Result<String, String> {
        let len: [u8; 4] = self.take(4, field)?.try_into().expect("4 bytes");
        let len = u32::from_be_bytes(len) as usize;
        let bytes = self.take(len, field)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| format!("{field}: not utf8"))
    }

    fn finish(self) -> Result<(), String> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(format!(
                "trailing bytes: {} of {} consumed",
                self.at,
                self.bytes.len()
            ))
        }
    }
}

pub fn parse_quote(bytes: &[u8]) -> Result<Quote, String> {
    let mut r = Reader { bytes, at: 0 };
    // same order as `quote_bytes`, read into named locals so the sequence is the layout
    // and not an evaluation-order accident
    let version = r.u8("version")?;
    let src_chain = r.u64("src_chain")?;
    let src_token = r.string("src_token")?;
    let amount_in = r.u128("amount_in")?;
    let dst_chain = r.u64("dst_chain")?;
    let dst_token = r.string("dst_token")?;
    let expected_out = r.u128("expected_out")?;
    let min_out = r.u128("min_out")?;
    let dst_address = r.string("dst_address")?;
    let refund_address = match r.string("refund_address")? {
        s if s.is_empty() => None,
        s => Some(s),
    };
    let auto_refund = r.bool("auto_refund")?;
    let gas_mode = match r.u8("gas_mode")? {
        0 => GasMode::Gasless,
        1 => GasMode::Legacy,
        other => return Err(format!("gas_mode: {other} is not a gas mode")),
    };
    let rail = r.string("rail")?;
    let expires_at_s = r.u64("expires_at_s")?;
    let nonce = r.u64("nonce")?;
    r.finish()?;
    // the version byte is carried, not interpreted: a future layout gets its own parser,
    // and `validate` is where the canister refuses one it cannot read
    Ok(Quote {
        version,
        src_chain,
        src_token,
        amount_in,
        dst_chain,
        dst_token,
        expected_out,
        min_out,
        dst_address,
        refund_address,
        auto_refund,
        gas_mode,
        rail,
        expires_at_s,
        nonce,
    })
}

/// The furthest ahead a quote may expire. Without it a quote with an expiry decades out
/// would hold its slot against the cap forever.
pub const MAX_QUOTE_LIFETIME_S: u64 = 86_400;

#[cfg(test)]
mod tests;
