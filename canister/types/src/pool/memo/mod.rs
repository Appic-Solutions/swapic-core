//! A pool deposit's terms as its order's `memo` carries them on chain, and the ref they hash
//! to: `ref = keccak256(memo)`.
//!
//! One packed layout, version 1, every integer big endian:
//!
//! | bytes | field |
//! |---|---|
//! | 1 | version, `0x01` |
//! | 1 | kind: `0x00` swap, `0x01` stake |
//! | 32 | salt |
//! | 16 | `min_usdc`: the least micro USDC an arrival must bring to be credited |
//!
//! then a swap: the recipient (an account), the token out (a principal), `min_out`,
//! `premium` and `icp_fees` (16 bytes each); or a stake: the owner (`0x01` and a 20-byte EVM
//! address, or `0x02` and an account) and `move_fee` (16 bytes). An account is a principal
//! then its 32 subaccount bytes (zeros: the default subaccount); a principal is its length
//! (1 to 29) then its bytes.
//!
//! Exactly one encoding per terms: a field the kind has no place for must be zero, and a
//! memo decodes only when every byte is read and re-encodes to itself.

#[cfg(test)]
mod tests;

use super::{DepositKind, DepositTerms, IcpAccount, PoolAccount};
use crate::evm::EvmAddress;
use crate::hash::PoolRef;
use crate::numeric::TokenAmount;
use alloy_primitives::keccak256;
use candid::Principal;
use thiserror::Error;

/// The layout this module writes and reads.
pub const MEMO_VERSION: u8 = 1;

/// The longest memo the canister relays or credits. The longest terms (a swap with two
/// 29-byte principals) take 190 bytes.
pub const MAX_MEMO_BYTES: usize = 256;

/// The longest principal the ICP has.
const MAX_PRINCIPAL_BYTES: usize = 29;

const KIND_SWAP: u8 = 0;
const KIND_STAKE: u8 = 1;
const OWNER_EVM: u8 = 1;
const OWNER_ICP: u8 = 2;

/// Why bytes are not a memo, or terms have no memo.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MemoError {
    #[error("a memo is at most {MAX_MEMO_BYTES} bytes, not {len}")]
    TooLong { len: usize },
    #[error("the memo ends inside {field}")]
    Truncated { field: &'static str },
    #[error("the memo has {len} bytes, {consumed} of them terms")]
    TrailingBytes { len: usize, consumed: usize },
    #[error("memo version {0} is not {MEMO_VERSION}")]
    Version(u8),
    #[error("deposit kind {0} is neither a swap nor a stake")]
    Kind(u8),
    #[error("owner tag {0} is neither an EVM address nor an ICP account")]
    OwnerTag(u8),
    #[error("{field} is a principal of {len} bytes, not 1 to {MAX_PRINCIPAL_BYTES}")]
    PrincipalLength { field: &'static str, len: usize },
    #[error("{field} is above u128::MAX")]
    AmountTooLarge { field: &'static str },
    #[error("the terms carry a fee their kind has no place for")]
    NotCanonical,
}

/// The ref a memo commits a deposit to.
pub fn memo_ref(memo: &[u8]) -> PoolRef {
    PoolRef::new(keccak256(memo).0)
}

/// `terms` as a memo.
pub fn encode(terms: &DepositTerms) -> Result<Vec<u8>, MemoError> {
    if !terms.is_canonical() {
        return Err(MemoError::NotCanonical);
    }
    let mut w = Writer::default();
    w.byte(MEMO_VERSION);
    match &terms.kind {
        DepositKind::Swap {
            recipient,
            token_out,
            min_out,
        } => {
            w.byte(KIND_SWAP);
            w.bytes(&terms.salt);
            w.amount(terms.min_usdc, "min_usdc")?;
            w.account(recipient, "recipient")?;
            w.principal(token_out, "token_out")?;
            w.word(*min_out);
            w.amount(terms.premium, "premium")?;
            w.amount(terms.icp_fees, "icp_fees")?;
        }
        DepositKind::Stake { owner } => {
            w.byte(KIND_STAKE);
            w.bytes(&terms.salt);
            w.amount(terms.min_usdc, "min_usdc")?;
            match owner {
                PoolAccount::Evm(address) => {
                    w.byte(OWNER_EVM);
                    w.bytes(address.as_bytes());
                }
                PoolAccount::Icp(account) => {
                    w.byte(OWNER_ICP);
                    w.account(account, "owner")?;
                }
            }
            w.amount(terms.move_fee, "move_fee")?;
        }
    }
    if w.0.len() > MAX_MEMO_BYTES {
        return Err(MemoError::TooLong { len: w.0.len() });
    }
    Ok(w.0)
}

/// The terms a memo holds.
pub fn decode(memo: &[u8]) -> Result<DepositTerms, MemoError> {
    if memo.len() > MAX_MEMO_BYTES {
        return Err(MemoError::TooLong { len: memo.len() });
    }
    let mut r = Reader { bytes: memo, at: 0 };
    let version = r.byte("version")?;
    if version != MEMO_VERSION {
        return Err(MemoError::Version(version));
    }
    let kind = r.byte("kind")?;
    let salt = r.array::<32>("salt")?;
    let min_usdc = r.amount("min_usdc")?;
    let terms = match kind {
        KIND_SWAP => {
            let recipient = r.account("recipient")?;
            let token_out = r.principal("token_out")?;
            let min_out = r.word("min_out")?;
            DepositTerms {
                kind: DepositKind::Swap {
                    recipient,
                    token_out,
                    min_out,
                },
                min_usdc,
                premium: r.amount("premium")?,
                icp_fees: r.amount("icp_fees")?,
                move_fee: TokenAmount::ZERO,
                salt,
            }
        }
        KIND_STAKE => {
            let owner = match r.byte("owner")? {
                OWNER_EVM => PoolAccount::Evm(EvmAddress::new(r.array::<20>("owner")?)),
                OWNER_ICP => PoolAccount::Icp(r.account("owner")?),
                tag => return Err(MemoError::OwnerTag(tag)),
            };
            DepositTerms {
                kind: DepositKind::Stake { owner },
                min_usdc,
                premium: TokenAmount::ZERO,
                icp_fees: TokenAmount::ZERO,
                move_fee: r.amount("move_fee")?,
                salt,
            }
        }
        kind => return Err(MemoError::Kind(kind)),
    };
    if r.at != memo.len() {
        return Err(MemoError::TrailingBytes {
            len: memo.len(),
            consumed: r.at,
        });
    }
    Ok(terms)
}

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn byte(&mut self, byte: u8) {
        self.0.push(byte);
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }

    fn word(&mut self, value: u128) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    fn amount(&mut self, amount: TokenAmount, field: &'static str) -> Result<(), MemoError> {
        let value = amount
            .try_into_u128()
            .ok_or(MemoError::AmountTooLarge { field })?;
        self.word(value);
        Ok(())
    }

    fn principal(&mut self, principal: &Principal, field: &'static str) -> Result<(), MemoError> {
        let bytes = principal.as_slice();
        if bytes.is_empty() || bytes.len() > MAX_PRINCIPAL_BYTES {
            return Err(MemoError::PrincipalLength {
                field,
                len: bytes.len(),
            });
        }
        self.byte(u8::try_from(bytes.len()).expect("BUG: a principal is 29 bytes at most"));
        self.bytes(bytes);
        Ok(())
    }

    fn account(&mut self, account: &IcpAccount, field: &'static str) -> Result<(), MemoError> {
        self.principal(&account.owner, field)?;
        self.bytes(&account.subaccount_bytes());
        Ok(())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, len: usize, field: &'static str) -> Result<&[u8], MemoError> {
        let end = self
            .at
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(MemoError::Truncated { field })?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn byte(&mut self, field: &'static str) -> Result<u8, MemoError> {
        Ok(self.take(1, field)?[0])
    }

    fn array<const N: usize>(&mut self, field: &'static str) -> Result<[u8; N], MemoError> {
        Ok(self
            .take(N, field)?
            .try_into()
            .expect("BUG: take answers exactly N bytes"))
    }

    fn word(&mut self, field: &'static str) -> Result<u128, MemoError> {
        Ok(u128::from_be_bytes(self.array::<16>(field)?))
    }

    fn amount(&mut self, field: &'static str) -> Result<TokenAmount, MemoError> {
        Ok(TokenAmount::new(self.word(field)?))
    }

    fn principal(&mut self, field: &'static str) -> Result<Principal, MemoError> {
        let len = usize::from(self.byte(field)?);
        if len == 0 || len > MAX_PRINCIPAL_BYTES {
            return Err(MemoError::PrincipalLength { field, len });
        }
        Ok(Principal::from_slice(self.take(len, field)?))
    }

    /// An account; 32 zero bytes are the default subaccount, so they read as none.
    fn account(&mut self, field: &'static str) -> Result<IcpAccount, MemoError> {
        let owner = self.principal(field)?;
        let subaccount = self.array::<32>(field)?;
        Ok(IcpAccount {
            owner,
            subaccount: (subaccount != [0; 32]).then_some(subaccount),
        })
    }
}
