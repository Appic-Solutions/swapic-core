use crate::events::{put_str, Hash32};
use candid::CandidType;
use serde::Deserialize;
use sha2::Digest;
use std::cell::RefCell;
use std::collections::BTreeMap;

/// The only layout this canister speaks. The version byte is the first byte of the
/// preimage, so a future layout is a new parser and a new golden file, never a branch
/// inside this one.
pub const QUOTE_VERSION: u8 = 1;

// update together with the struct; the exhaustive destructure in `quote_bytes` is the
// compile-time check, this is the test-coverage check
pub const QUOTE_FIELD_COUNT: usize = 15;

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
/// hash; `tests/golden/quote_hash_v1.txt` is the vector to check a reimplementation
/// against.
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

/// How many quotes may sit in the pending store at once. The store is heap, and the
/// quoter is the only writer, so this is a backstop against a compromised or looping
/// quoter growing the canister until it traps, not a business limit.
pub const MAX_PENDING: usize = 10_000;

/// The furthest ahead a quote may expire. Without it a quote with an expiry decades out
/// would hold its slot against the cap forever.
pub const MAX_QUOTE_LIFETIME_S: u64 = 86_400;

thread_local! {
    // Pre-money state, and heap-only BY DESIGN: a registered quote is a promise the quoter
    // made, not something that happened to money, so it is neither in the event log nor in
    // stable memory. An upgrade drops the map and the quoter re-registers what is live.
    static PENDING: RefCell<BTreeMap<Hash32, (Quote, u64)>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// Records a quote against its hash. `now_s` is the caller's clock, in seconds, so every
/// rule here is testable without a canister.
pub fn register(quote: Quote, now_s: u64) -> Result<Hash32, String> {
    quote.validate()?;
    // the quote is good through the whole of its expiry second
    if now_s > quote.expires_at_s {
        return Err(format!(
            "quote expired at {} and it is now {now_s}",
            quote.expires_at_s
        ));
    }
    if quote.expires_at_s > now_s.saturating_add(MAX_QUOTE_LIFETIME_S) {
        return Err(format!(
            "quote expires at {}, more than {MAX_QUOTE_LIFETIME_S}s ahead of {now_s}",
            quote.expires_at_s
        ));
    }
    let hash = quote_hash(&quote);
    PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        // a re-registration is always allowed, cap or no cap: the quoter replays its live
        // quotes after an upgrade, and refusing those would strand swaps that already
        // exist. The hash covers every field, so an overwrite replaces a quote with itself.
        if pending.len() >= MAX_PENDING && !pending.contains_key(&hash) {
            return Err(format!("pending store is full at {MAX_PENDING} quotes"));
        }
        pending.insert(hash, (quote, now_s));
        Ok(hash)
    })
}

pub fn get_pending(quote_hash: &Hash32) -> Option<Quote> {
    PENDING.with(|p| p.borrow().get(quote_hash).map(|(q, _)| q.clone()))
}

/// Tests share one PENDING when the harness runs them on a single thread, so the store
/// tests start from a known map instead of assuming an empty one.
#[cfg(test)]
fn clear_pending() {
    PENDING.with(|p| p.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cross-repo fixture: swapic-backend's mirror builds this same quote field for
    /// field and must hash it to the same golden line.
    fn fixed_quote() -> Quote {
        Quote {
            version: 1,
            src_chain: 8453,
            src_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
            amount_in: 25_000_000,
            dst_chain: 42161,
            dst_token: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831".into(),
            expected_out: 24_990_000,
            min_out: 24_900_000,
            dst_address: "0x7551A66653f9a20979ed81835a0b7008EC83401b".into(),
            refund_address: None,
            auto_refund: true,
            gas_mode: GasMode::Legacy,
            rail: "cctp_v2_fast".into(),
            expires_at_s: 1_800_000_000,
            nonce: 7,
        }
    }

    /// `fixed_quote` with exactly one field moved, once per field. Doubles as the
    /// round-trip sample set.
    fn one_field_changed() -> Vec<(&'static str, Quote)> {
        let q = fixed_quote();
        // exhaustive destructure: a new field breaks this line, so the list below cannot
        // silently miss one and leave `QUOTE_FIELD_COUNT` undercounting
        let Quote {
            version: _,
            src_chain: _,
            src_token: _,
            amount_in: _,
            dst_chain: _,
            dst_token: _,
            expected_out: _,
            min_out: _,
            dst_address: _,
            refund_address: _,
            auto_refund: _,
            gas_mode: _,
            rail: _,
            expires_at_s: _,
            nonce: _,
        } = &q;
        vec![
            (
                "version",
                Quote {
                    version: 2,
                    ..q.clone()
                },
            ),
            (
                "src_chain",
                Quote {
                    src_chain: 1,
                    ..q.clone()
                },
            ),
            (
                "src_token",
                Quote {
                    src_token: "0xdAC17F958D2ee523a2206206994597C13D831ec7".into(),
                    ..q.clone()
                },
            ),
            (
                "amount_in",
                Quote {
                    amount_in: 25_000_001,
                    ..q.clone()
                },
            ),
            (
                "dst_chain",
                Quote {
                    dst_chain: 1,
                    ..q.clone()
                },
            ),
            (
                "dst_token",
                Quote {
                    dst_token: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".into(),
                    ..q.clone()
                },
            ),
            (
                "expected_out",
                Quote {
                    expected_out: 24_990_001,
                    ..q.clone()
                },
            ),
            (
                "min_out",
                Quote {
                    min_out: 24_900_001,
                    ..q.clone()
                },
            ),
            (
                "dst_address",
                Quote {
                    dst_address: "0x1111111254EEB25477B68fb85Ed929f73A960582".into(),
                    ..q.clone()
                },
            ),
            (
                // a non-empty address, deliberately: None and Some("") share a preimage
                // by design, which `none_and_empty_refund_address_share_a_preimage` pins
                "refund_address",
                Quote {
                    refund_address: Some("0x1111111254EEB25477B68fb85Ed929f73A960582".into()),
                    ..q.clone()
                },
            ),
            (
                "auto_refund",
                Quote {
                    auto_refund: false,
                    ..q.clone()
                },
            ),
            (
                "gas_mode",
                Quote {
                    gas_mode: GasMode::Gasless,
                    ..q.clone()
                },
            ),
            (
                "rail",
                Quote {
                    rail: "cctp_v2_standard".into(),
                    ..q.clone()
                },
            ),
            (
                "expires_at_s",
                Quote {
                    expires_at_s: 1_800_000_001,
                    ..q.clone()
                },
            ),
            ("nonce", Quote { nonce: 8, ..q }),
        ]
    }

    /// The awkward shapes: empty strings, multibyte utf8, and the top of every integer.
    fn edge_quotes() -> Vec<Quote> {
        vec![
            Quote {
                src_token: String::new(),
                dst_token: String::new(),
                dst_address: String::new(),
                rail: String::new(),
                refund_address: None,
                ..fixed_quote()
            },
            Quote {
                // length is bytes, not chars
                src_token: "tökén \u{2603}".into(),
                dst_address: "\u{1F680}".into(),
                refund_address: Some("réfund".into()),
                rail: "rail \u{2603}".into(),
                ..fixed_quote()
            },
            Quote {
                version: u8::MAX,
                src_chain: u64::MAX,
                // high words set: proves the u128s are 16 bytes, not truncated u64s
                amount_in: u128::MAX,
                dst_chain: u64::MAX,
                expected_out: u128::MAX - 1,
                min_out: 1u128 << 70,
                auto_refund: false,
                gas_mode: GasMode::Gasless,
                expires_at_s: u64::MAX,
                nonce: u64::MAX,
                ..fixed_quote()
            },
        ]
    }

    const GOLDEN: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/quote_hash_v1.txt"
    );

    #[test]
    fn any_field_change_changes_the_hash() {
        let base = quote_hash(&fixed_quote());
        let changed = one_field_changed();
        assert_eq!(
            changed.len(),
            QUOTE_FIELD_COUNT,
            "one_field_changed() must move every field"
        );
        let mut seen = std::collections::BTreeSet::new();
        for (field, q) in changed {
            let h = quote_hash(&q);
            assert_ne!(base, h, "moving {field} left the hash alone");
            assert!(seen.insert(h), "two different quotes hash alike ({field})");
        }
    }

    /// The layout is fixed-width apart from the four strings, so the total is arithmetic:
    /// a stray or missing prefix shows up here before it reaches the golden file.
    #[test]
    fn the_preimage_is_exactly_as_long_as_the_layout_says() {
        // 1 version + 8 src_chain + (4+42) src_token + 16 amount_in + 8 dst_chain
        // + (4+42) dst_token + 16 expected_out + 16 min_out + (4+42) dst_address
        // + (4+0) refund_address + 1 auto_refund + 1 gas_mode + (4+12) rail
        // + 8 expires_at_s + 8 nonce
        assert_eq!(quote_bytes(&fixed_quote()).len(), 241);
    }

    #[test]
    fn gas_mode_encodes_one_byte() {
        let gasless = quote_bytes(&Quote {
            gas_mode: GasMode::Gasless,
            ..fixed_quote()
        });
        let legacy = quote_bytes(&fixed_quote());
        assert_eq!(gasless.len(), legacy.len(), "one byte either way");
        // the byte sits after the refund_address prefix, before the rail
        let at = gasless.len() - 8 - 8 - (4 + 12) - 1;
        assert_eq!(gasless[at], 0, "Gasless encodes 0");
        assert_eq!(legacy[at], 1, "Legacy encodes 1");
    }

    /// The one place the layout is not injective, pinned here so nobody discovers it by
    /// accident: an absent refund address and an empty one write the same four bytes.
    /// `validate` refuses the empty one, so the collision cannot reach a stored quote.
    #[test]
    fn none_and_empty_refund_address_share_a_preimage() {
        let none = fixed_quote();
        let empty = Quote {
            refund_address: Some(String::new()),
            ..fixed_quote()
        };
        assert_eq!(quote_bytes(&none), quote_bytes(&empty));
        none.validate().expect("an absent refund address is fine");
        let err = empty.validate().expect_err("an empty one is not");
        assert!(err.contains("refund_address"), "name the field: {err}");
        // and the reader resolves it the one way that survives a round trip
        assert_eq!(parse_quote(&quote_bytes(&none)).unwrap(), none);
    }

    #[test]
    fn validate_refuses_a_version_this_canister_cannot_read() {
        let err = Quote {
            version: 2,
            ..fixed_quote()
        }
        .validate()
        .expect_err("only v1 is known");
        assert!(err.contains("version"), "name the field: {err}");
    }

    #[test]
    fn parse_quote_round_trips_every_field_variation() {
        let mut all = vec![fixed_quote()];
        all.extend(one_field_changed().into_iter().map(|(_, q)| q));
        all.extend(edge_quotes());
        for q in all {
            let bytes = quote_bytes(&q);
            let back = parse_quote(&bytes).unwrap_or_else(|e| panic!("{e} for {q:?}"));
            assert_eq!(back, q, "round trip lost a field");
            assert_eq!(quote_bytes(&back), bytes, "and the re-encode drifted");
        }
    }

    #[test]
    fn parse_quote_rejects_a_truncated_preimage() {
        let bytes = quote_bytes(&fixed_quote());
        for cut in [0, 1, 40, 100, bytes.len() - 1] {
            let err = parse_quote(&bytes[..cut]).expect_err("truncated bytes are not a quote");
            assert!(err.contains("truncated"), "say what went wrong: {err}");
        }
    }

    #[test]
    fn parse_quote_rejects_trailing_bytes() {
        let mut bytes = quote_bytes(&fixed_quote());
        bytes.push(0);
        let err = parse_quote(&bytes).expect_err("a quote is the whole input");
        assert!(err.contains("trailing"), "say what went wrong: {err}");
    }

    #[test]
    fn parse_quote_rejects_bytes_outside_the_layout() {
        let base = quote_bytes(&fixed_quote());
        let auto_refund_at = base.len() - 8 - 8 - (4 + 12) - 1 - 1;

        let mut bad_bool = base.clone();
        bad_bool[auto_refund_at] = 2;
        let err = parse_quote(&bad_bool).expect_err("a bool is 0 or 1");
        assert!(err.contains("auto_refund"), "name the field: {err}");

        let mut bad_mode = base.clone();
        bad_mode[auto_refund_at + 1] = 2;
        let err = parse_quote(&bad_mode).expect_err("a gas mode is 0 or 1");
        assert!(err.contains("gas_mode"), "name the field: {err}");

        // the src_token length prefix, blown up past the end of the input
        let mut long_str = base.clone();
        long_str[9..13].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(parse_quote(&long_str).is_err(), "a length must fit");

        // 0xff is never valid utf8, so this lands inside src_token's bytes
        let mut bad_utf8 = base;
        bad_utf8[13] = 0xff;
        let err = parse_quote(&bad_utf8).expect_err("strings are utf8");
        assert!(err.contains("src_token"), "name the field: {err}");
    }

    #[test]
    fn quote_hash_matches_golden_vector() {
        let got = hex::encode(quote_hash(&fixed_quote()));

        // regeneration is opt-in and never green, so a blessing is always a deliberate diff
        if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
            let dir = std::path::Path::new(GOLDEN).parent().unwrap();
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(GOLDEN, got + "\n").unwrap();
            panic!("golden regenerated, inspect the diff and rerun: {GOLDEN}");
        }

        let want = std::fs::read_to_string(GOLDEN).unwrap_or_else(|e| {
            panic!("golden missing or unreadable ({e}); regenerate with UPDATE_GOLDEN=1: {GOLDEN}")
        });
        assert_eq!(
            got,
            want.trim(),
            "canonical layout changed: breaking, and swapic-backend's mirror breaks with it"
        );
    }

    /// A live quote and the clock it is live on. Every store test calls `clear_pending`
    /// first, because a single-threaded harness gives them all one map.
    fn pending_quote(nonce: u64) -> Quote {
        Quote {
            nonce,
            ..fixed_quote()
        }
    }

    fn just_before_expiry(q: &Quote) -> u64 {
        q.expires_at_s - 1
    }

    #[test]
    fn register_refuses_a_quote_that_has_already_expired() {
        clear_pending();
        let q = pending_quote(9_001);
        let err = register(q.clone(), q.expires_at_s + 1).expect_err("expired quotes are refused");
        assert!(err.contains("expired"), "say why: {err}");
        assert_eq!(get_pending(&quote_hash(&q)), None, "and nothing was stored");
    }

    #[test]
    fn register_refuses_a_quote_that_does_not_validate() {
        clear_pending();
        let q = Quote {
            refund_address: Some(String::new()),
            ..pending_quote(9_002)
        };
        assert!(register(q.clone(), just_before_expiry(&q)).is_err());
        assert_eq!(get_pending(&quote_hash(&q)), None);
    }

    #[test]
    fn register_stores_the_quote_under_its_hash_and_a_rerun_overwrites() {
        clear_pending();
        let q = pending_quote(9_003);
        let h = register(q.clone(), just_before_expiry(&q)).expect("a live quote registers");
        assert_eq!(h, quote_hash(&q));
        assert_eq!(get_pending(&h), Some(q.clone()));
        // the quoter re-registers after an upgrade, so the same quote twice is not an error
        assert_eq!(register(q.clone(), just_before_expiry(&q)), Ok(h));
        assert_eq!(get_pending(&h), Some(q));
        assert_eq!(get_pending(&[0; 32]), None);
    }

    /// The boundary the expiry check draws: `expires_at_s` is the last second the quote is
    /// still good.
    #[test]
    fn a_quote_is_live_up_to_and_including_its_expiry_second() {
        clear_pending();
        let q = pending_quote(9_004);
        assert!(register(q.clone(), q.expires_at_s).is_ok());
        assert!(register(q.clone(), q.expires_at_s + 1).is_err());
    }

    /// An immortal quote would hold a slot against the cap forever, so the far end of the
    /// window is bounded as well as the near one.
    #[test]
    fn register_refuses_a_quote_that_expires_too_far_ahead() {
        clear_pending();
        let q = pending_quote(9_005);
        let far = q.expires_at_s - MAX_QUOTE_LIFETIME_S - 1;
        let err = register(q.clone(), far).expect_err("that is too long to hold a slot");
        assert!(
            err.contains(&MAX_QUOTE_LIFETIME_S.to_string()),
            "say why: {err}"
        );
        assert_eq!(get_pending(&quote_hash(&q)), None);
        // and the edge of the window is inside it
        assert!(register(q.clone(), far + 1).is_ok());
    }

    /// The backstop against a looping quoter: a full store refuses a new quote but must
    /// still take a re-registration, which is how the quoter replays after an upgrade.
    #[test]
    fn a_full_store_refuses_a_new_quote_and_still_takes_a_repeat() {
        clear_pending();
        // small quotes, distinct only by nonce, so the fill is cheap
        let tiny = |nonce: u64| Quote {
            src_token: String::new(),
            dst_token: String::new(),
            dst_address: String::new(),
            rail: String::new(),
            nonce,
            ..fixed_quote()
        };
        let now = just_before_expiry(&tiny(0));
        for nonce in 0..MAX_PENDING as u64 {
            register(tiny(nonce), now).expect("fills to the cap");
        }

        let overflow = tiny(MAX_PENDING as u64);
        let err = register(overflow.clone(), now).expect_err("the store is full");
        assert!(err.contains("full"), "say why: {err}");
        assert_eq!(get_pending(&quote_hash(&overflow)), None);

        // the one thing a full store must still accept
        let repeat = tiny(0);
        assert_eq!(register(repeat.clone(), now), Ok(quote_hash(&repeat)));
    }
}
