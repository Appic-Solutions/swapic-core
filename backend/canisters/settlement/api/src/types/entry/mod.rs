//! The entry doors: what a claim and a gasless pull take and how each refuses.

use crate::types::errors::{AppendError, GuardError};
use crate::types::events::{EvmAddressError, Hash32};
use crate::types::quote::QuoteError;
use crate::types::rpc::RpcError;
use crate::types::tx::TxError;
use candid::{CandidType, Nat};
use serde::Deserialize;
use types::abi::Permit;
use types::{EvmAddress, TokenAmount, UnixSeconds};

/// An EIP-2612 permit a user signed for the vault, with what it permits: the token and the
/// amount of the quote, from the user's own account, until `deadline_s`. `r` and `s` are
/// the signature words and `v` its recovery byte, 27 or 28.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PermitSig {
    pub token: String,
    pub owner: String,
    pub amount: Nat,
    pub deadline_s: u64,
    pub v: u8,
    pub r: Hash32,
    pub s: Hash32,
}

/// A permit as the vault's `pullWithPermit` takes it, in the domain's own types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullPermit {
    pub token: EvmAddress,
    pub owner: EvmAddress,
    pub amount: TokenAmount,
    pub deadline: UnixSeconds,
    pub signature: Permit,
}

/// Why a permit is not one the vault can use, naming the field.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PermitError {
    NotAnAddress {
        field: String,
        reason: EvmAddressError,
    },
    AmountTooLarge,
}

impl TryFrom<PermitSig> for PullPermit {
    type Error = PermitError;

    fn try_from(permit: PermitSig) -> Result<Self, Self::Error> {
        let address = |field: &str, text: &str| {
            text.parse().map_err(
                |reason: types::evm::EvmAddressError| PermitError::NotAnAddress {
                    field: field.to_string(),
                    reason: reason.into(),
                },
            )
        };
        Ok(Self {
            token: address("token", &permit.token)?,
            owner: address("owner", &permit.owner)?,
            amount: TokenAmount::from_canonical_nat(permit.amount)
                .ok_or(PermitError::AmountTooLarge)?,
            deadline: UnixSeconds::new(permit.deadline_s),
            signature: Permit {
                v: permit.v,
                r: permit.r,
                s: permit.s,
            },
        })
    }
}

/// Why a chain has no vault to read or send to: a deploy mistake, named by chain.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum VaultError {
    NoVault {
        chain_id: u64,
    },
    NotAnAddress {
        chain_id: u64,
        reason: EvmAddressError,
    },
}

/// Why the deposit read verified nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum DepositError {
    Vault(VaultError),
    /// The watcher has pushed nothing young enough to anchor the read on.
    StaleChainData {
        chain_id: u64,
    },
    Rpc(RpcError),
    UnreadableHead,
    UnreadableLogs,
    /// The vault's log holds no deposit for the quote in the lookback.
    NotFound {
        quote_hash: Hash32,
    },
    /// The deposit is in a block the head has not reached by the configured depth.
    NotConfirmed {
        block: u64,
        latest: u64,
        depth: u64,
    },
}

/// Why `claim_swap` created no swap. Nothing was written.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum ClaimError {
    Guard(GuardError),
    InvalidQuote(QuoteError),
    SwapExists(Hash32),
    /// Past the quote's expiry plus the permit window, nobody can pay the quote and no
    /// deposit is claimed for it.
    QuoteExpired {
        expires_at_s: u64,
        claim_until_s: u64,
        now_s: u64,
    },
    /// `party` is `"dst_address"`, `"refund_address"` or `"from"` (the payer).
    Sanctioned {
        party: String,
    },
    /// A claim or a pull for this quote is already out, since `since_ns`.
    InFlight {
        since_ns: u64,
    },
    /// The quote's source token is not an EVM address, so no vault log can name it.
    SourceTokenNotAnAddress {
        token: String,
        reason: EvmAddressError,
    },
    Deposit(DepositError),
    /// The vault holds a deposit for the quote, but of another token.
    TokenMismatch {
        quoted: String,
        deposited: String,
    },
    /// The vault holds a deposit for the quote, but of another amount.
    AmountMismatch {
        quoted: Nat,
        deposited: Nat,
    },
    Append(AppendError),
}

/// Why `start_gasless_pull` sent nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum PullError {
    Guard(GuardError),
    /// The quote is not in the pending store: never registered, or evicted.
    UnknownQuote(Hash32),
    /// The quote's user pays their own gas.
    NotGasless,
    QuoteExpired {
        expires_at_s: u64,
        claim_until_s: u64,
        now_s: u64,
    },
    Permit(PermitError),
    /// The permit's `field` (`"token"` or `"amount"`) is not the quote's.
    PermitMismatch {
        field: String,
    },
    /// `party` is `"owner"`, `"dst_address"` or `"refund_address"`.
    Sanctioned {
        party: String,
    },
    InFlight {
        since_ns: u64,
    },
    /// A pull for this quote is signed and not yet landed; it is that transaction.
    AlreadyPulling {
        tx_hash: Hash32,
    },
    Vault(VaultError),
    Tx(TxError),
}

/// Why `push_attestation` stored nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PushAttestationError {
    Guard(GuardError),
    UnknownSwap(Hash32),
    MessageTooLong { len: u64, cap: u64 },
    AttestationTooLong { len: u64, cap: u64 },
}

#[cfg(test)]
mod tests;
