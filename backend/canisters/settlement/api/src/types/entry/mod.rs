//! The entry doors: what a claim and a gasless pull take and how each refuses.

use crate::types::errors::{AppendError, GuardError};
use crate::types::events::{EvmAddressError, Hash32};
use crate::types::quote::{QuoteAddressError, QuoteError, RailTokenError};
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
    /// The range from `from` to the head at `anchor` would take `windows` calls of ten
    /// thousand blocks, above `cap`; refused before any call.
    RangeTooWide {
        from: u64,
        anchor: u64,
        windows: u64,
        cap: u64,
    },
    Rpc(RpcError),
    UnreadableHead,
    UnreadableLogs,
    /// The vault's log holds no deposit for the quote in the range read.
    NotFound {
        quote_hash: Hash32,
    },
    /// The vault's log holds `seen` deposits for the quote, and none of them is the token
    /// and amount wanted: funds under the hash for an operator, and no swap.
    NoneMatches {
        quote_hash: Hash32,
        seen: u64,
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
    /// The quote's tokens are not its rail's: refused before any outcall.
    RailToken(RailTokenError),
    /// A field of the quote that the claim reads as an EVM address is not one.
    QuoteAddress(QuoteAddressError),
    Deposit(DepositError),
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
    /// A field of the quote that the pull reads as an EVM address is not one.
    QuoteAddress(QuoteAddressError),
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

/// Why bytes are not a CCTP v2 burn message.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum MessageError {
    TooShort { len: u64, wanted: u64 },
    UnknownVersion { version: u32 },
    UnknownBodyVersion { version: u32 },
    ExpirationBlockTooLarge,
}

impl From<types::cctp::MessageError> for MessageError {
    fn from(error: types::cctp::MessageError) -> Self {
        use types::cctp::MessageError as Domain;
        match error {
            Domain::TooShort { len, wanted } => Self::TooShort {
                len: crate::types::wire_len(len),
                wanted: crate::types::wire_len(wanted),
            },
            Domain::UnknownVersion { version } => Self::UnknownVersion { version },
            Domain::UnknownBodyVersion { version } => Self::UnknownBodyVersion { version },
            Domain::ExpirationBlockTooLarge => Self::ExpirationBlockTooLarge,
        }
    }
}

/// A field of a burn message the swap's burn determined.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageField {
    SourceDomain,
    DestinationDomain,
    Sender,
    Recipient,
    DestinationCaller,
    BurnToken,
    MintRecipient,
    Amount,
    MessageSender,
    MaxFee,
}

/// Why a pushed message is not the swap's own burn: the field that reads otherwise, with
/// what the swap's burn wrote (`expected`) and what the message carries (`found`). Words
/// are the 32-byte words the message carries.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum MessageMismatch {
    Version {
        found: u32,
    },
    Domain {
        field: MessageField,
        expected: u32,
        found: u32,
    },
    Word {
        field: MessageField,
        expected: Hash32,
        found: Hash32,
    },
    Threshold {
        expected: u32,
        found: u32,
    },
    Amount {
        field: MessageField,
        expected: Nat,
        found: Nat,
    },
    FeeAboveMaxFee {
        fee: Nat,
        max_fee: Nat,
    },
    HookData {
        len: u64,
    },
}

/// Why a rail could not decide on a swap: a knob the deploy left unset, an address of the
/// quote or of the vault that is not one, or an attestation that is not the swap's.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum RailError {
    NoDomain { chain_id: u64 },
    NoUsdc { chain_id: u64 },
    NoTokenMessenger,
    NoMessageTransmitter,
    NoEcoPortal,
    Vault(VaultError),
    FeeOverflow { amount: Nat },
    RailToken(RailTokenError),
    QuoteAddress(QuoteAddressError),
    UnreadableMessage(MessageError),
    Message(MessageMismatch),
    NoMintHash,
}

/// Why `push_attestation` stored nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum PushAttestationError {
    Guard(GuardError),
    UnknownSwap(Hash32),
    /// The swap is not on a CCTP rail, so no attestation is its.
    NotACctpSwap(Hash32),
    MessageTooLong {
        len: u64,
        cap: u64,
    },
    AttestationTooLong {
        len: u64,
        cap: u64,
    },
    /// The swap's burn has not confirmed, so there is no burn for the message to be of.
    NoBurnConfirmed(Hash32),
    /// The burn the push names is not the transaction the swap's burn confirmed as.
    NotTheSwapsBurn {
        pushed: Hash32,
        confirmed: Hash32,
    },
    /// This canister's address is not derived yet, so the message's caller cannot be
    /// checked.
    AddressNotDerived,
    /// The message does not bind to the swap's burn, or the rail cannot check it.
    Rail(RailError),
}

#[cfg(test)]
mod tests;

/// What Eco's quote response gave a swap on the Eco rail, as the watcher hands it in:
/// `destination_chain` is Eco's `destinationChainID` and never the chain the user is paid
/// on, `route` its `encodedRoute`, `deadline_s` the reward's deadline and `prover` the
/// prover it names. The reward itself is the swap's own amount of the source USDC, with
/// the vault as its creator, which the canister fills in.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct EcoIntent {
    pub destination_chain: u64,
    pub route: Vec<u8>,
    pub deadline_s: u64,
    pub prover: String,
}

/// Why `push_eco_intent` stored nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PushEcoIntentError {
    Guard(GuardError),
    UnknownSwap(Hash32),
    /// The swap is not on the Eco rail.
    NotAnEcoSwap(Hash32),
    ProverNotAnAddress {
        reason: EvmAddressError,
    },
    RouteTooLong {
        len: u64,
        cap: u64,
    },
}
