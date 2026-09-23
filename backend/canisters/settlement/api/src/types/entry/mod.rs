//! The entry doors: what a claim and a gasless pull take and how each refuses.

use crate::types::errors::{AppendError, GuardError};
use crate::types::events::{EvmAddressError, Hash32};
use crate::types::quote::{QuoteAddressError, QuoteError, RailTokenError};
use crate::types::rpc::RpcError;
use crate::types::tx::TxError;
use candid::{CandidType, Nat};
use serde::Deserialize;

/// The most bytes a Permit2 signature may be. A wallet signs sixty-five (or sixty-four
/// compact); a contract wallet answering EIP-1271 may sign more, and this is room for one
/// without letting a caller put a megabyte in a transaction this canister pays gas for.
pub const MAX_PERMIT2_SIGNATURE_BYTES: usize = 1_024;

/// An EIP-2612 permit a user signed for a token contract, with what it permits: the token
/// and the amount of the quote, from the user's own account, until `deadline_s`. `r` and
/// `s` are the signature words and `v` its recovery byte, 27 or 28.
///
/// NOT A SHAPE THIS CANISTER PULLS WITH, and a pull carrying one is refused: the vault's
/// 2612 door runs `safeTransferFrom` whether or not the permit verified, so the signature
/// decides nothing and any standing allowance is enough to move a user's funds, and a 2612
/// permit names `(owner, spender, value, deadline)` and never a quote, so one signed for a
/// quote can be spent under another. It stays on the wire so a caller that sends one is
/// told why, by name.
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

/// A Permit2 `PermitTransferFrom` a gasless user signed for the vault, witnessed by the
/// quote it pays for.
///
/// `quote_hash` is that witness and `spender` is the address the signature names as the
/// one who may spend: neither travels in the calldata (the vault derives the witness from
/// the quote it is called with, and Permit2 takes the spender from its caller), so both
/// are here to be checked against the quote and the vault before anything is signed.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Permit2Sig {
    /// the quote hash the signature's witness names
    pub quote_hash: Hash32,
    pub token: String,
    pub owner: String,
    /// the spender the signature names, which has to be the vault
    pub spender: String,
    pub amount: Nat,
    /// Permit2's own nonce, a number the signer picks and Permit2 spends once
    pub nonce: Nat,
    pub deadline_s: u64,
    /// the signature itself, at most [`MAX_PERMIT2_SIGNATURE_BYTES`] bytes
    pub signature: Vec<u8>,
}

/// What a gasless pull may be asked with. Only the Permit2 permit is a door this canister
/// uses; see [`PermitSig`] for why the other one is refused.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PullRequest {
    Eip2612(PermitSig),
    Permit2(Permit2Sig),
}

/// Why a permit the caller sent is not the one this quote's pull may use, with what it
/// says and what the quote says beside it.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PermitMismatch {
    /// The signature's witness names another quote, so the funds it frees are not this
    /// swap's.
    Witness {
        signed_for: Hash32,
    },
    Token {
        permitted: String,
        wanted: String,
    },
    Amount {
        permitted: Nat,
        wanted: Nat,
    },
    /// The signature names another spender than the vault the pull would call, so Permit2
    /// would refuse it on the chain.
    Spender {
        signed_for: String,
        vault: String,
    },
    /// The permit's own deadline has passed.
    Expired {
        deadline_s: u64,
        now_s: u64,
    },
}

/// Which door holds the marker: a claim reading the chain, or a pull being signed.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum InFlightKind {
    Claim,
    Pull,
}

impl From<types::InFlightKind> for InFlightKind {
    fn from(kind: types::InFlightKind) -> Self {
        match kind {
            types::InFlightKind::Claim => Self::Claim,
            types::InFlightKind::Pull => Self::Pull,
        }
    }
}

/// Why a permit is not one the vault can use, naming the field.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PermitError {
    NotAnAddress {
        field: String,
        reason: EvmAddressError,
    },
    AmountTooLarge,
    NonceTooLarge,
    /// The pull carried an EIP-2612 permit, which this canister never sends: the vault's
    /// 2612 door transfers whether or not the permit verified, and a 2612 signature names
    /// no quote.
    NotAPermit2Permit,
    SignatureTooLong {
        len: u64,
        cap: u64,
    },
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
    /// The config lists no confirmation depth for the chain, so nothing on it is deep
    /// enough to decide money on.
    NoDepth {
        chain_id: u64,
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
    /// The provider holds no block `block` under the hash the deposit's log named: reorged
    /// away, or not reached by the provider asked. Asked again, it may answer.
    NoBlock {
        block: u64,
    },
    /// The block the deposit is in did not come back as that block, with a time.
    UnreadableBlock,
}

/// Why `claim_swap` created no swap. Nothing was written.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum ClaimError {
    Guard(GuardError),
    InvalidQuote(QuoteError),
    SwapExists(Hash32),
    /// Past the quote's expiry, the permit window and the grace after it, no claim is
    /// admitted for the quote, whoever asks.
    QuoteExpired {
        expires_at_s: u64,
        claim_until_s: u64,
        now_s: u64,
    },
    /// The deposit found is in a block the chain made at `landed_at_s`, after the last
    /// second a deposit for the quote could land (its expiry plus the permit window). It
    /// is not the swap the user was quoted: the funds stay in the vault, for the
    /// late-arrival policy a later plan builds.
    LandedLate {
        block: u64,
        landed_at_s: u64,
        deposit_until_s: u64,
    },
    /// `party` is `"dst_address"`, `"refund_address"` or `"from"` (the payer).
    Sanctioned {
        party: String,
    },
    /// A claim or a pull for this quote is already out, since `since_ns`: `kind` says
    /// which of the two, so a caller told to wait knows what it is waiting on.
    InFlight {
        kind: InFlightKind,
        since_ns: u64,
    },
    /// The quote names a rail this deploy does not run, by its id.
    RailUnavailable {
        rail: String,
    },
    /// No quote with this id is in the pending store: the quoter never registered it, or
    /// it was evicted. A controller may still claim such a quote by hand.
    NotRegistered(Hash32),
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
    /// The permit is not one this quote's pull may use.
    PermitMismatch(PermitMismatch),
    /// A field of the quote that the pull reads as an EVM address is not one.
    QuoteAddress(QuoteAddressError),
    /// `party` is `"owner"`, `"dst_address"` or `"refund_address"`.
    Sanctioned {
        party: String,
    },
    InFlight {
        kind: InFlightKind,
        since_ns: u64,
    },
    /// A pull for this quote is signed and not yet landed; it is that transaction.
    AlreadyPulling {
        tx_hash: Hash32,
    },
    Vault(VaultError),
    Tx(TxError),
    /// The quote names a rail this deploy does not run, by its id: its claim would be
    /// refused, so its funds are not pulled into the vault.
    RailUnavailable {
        rail: String,
    },
    /// The quote's tokens are not its rail's: its claim would be refused, so its funds are
    /// not pulled into the vault.
    RailToken(RailTokenError),
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
    /// The message's hook data names another swap: the message is the attested message of
    /// that swap's burn, however identical the two burns are otherwise.
    Swap {
        expected: Hash32,
        found: Hash32,
    },
}

/// Why a rail could not decide on a swap: a knob the deploy left unset, an address of the
/// quote or of the vault that is not one, or an attestation that is not the swap's.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum RailError {
    NoDomain {
        chain_id: u64,
    },
    NoUsdc {
        chain_id: u64,
    },
    NoTokenMessenger,
    NoMessageTransmitter,
    NoEcoPortal,
    /// The rail, by its id, is one this deploy does not run.
    RailDisabled {
        rail: String,
    },
    Vault(VaultError),
    FeeOverflow {
        amount: Nat,
    },
    /// An amount above the largest floor a vault execution can carry, which is no amount
    /// this canister swaps.
    AmountTooLarge {
        amount: Nat,
    },
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

/// Why a pushed intent is not one the inbox can hold, whichever door it came through.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum EcoIntentError {
    ProverNotAnAddress { reason: EvmAddressError },
    RouteTooLong { len: u64, cap: u64 },
}

impl TryFrom<EcoIntent> for types::EcoIntent {
    type Error = EcoIntentError;

    fn try_from(intent: EcoIntent) -> Result<Self, Self::Error> {
        let prover = intent
            .prover
            .parse()
            .map_err(
                |reason: types::evm::EvmAddressError| EcoIntentError::ProverNotAnAddress {
                    reason: reason.into(),
                },
            )?;
        types::EcoIntent::new(
            types::ChainId::new(intent.destination_chain),
            intent.route,
            types::UnixSeconds::new(intent.deadline_s),
            prover,
        )
        .map_err(|types::rail::EcoIntentError::RouteTooLong { len, cap }| {
            EcoIntentError::RouteTooLong {
                len: crate::types::wire_len(len),
                cap: crate::types::wire_len(cap),
            }
        })
    }
}

impl From<EcoIntentError> for PushEcoIntentError {
    fn from(error: EcoIntentError) -> Self {
        match error {
            EcoIntentError::ProverNotAnAddress { reason } => Self::ProverNotAnAddress { reason },
            EcoIntentError::RouteTooLong { len, cap } => Self::RouteTooLong { len, cap },
        }
    }
}

/// Why `push_eco_intent` stored nothing.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum PushEcoIntentError {
    Guard(GuardError),
    UnknownSwap(Hash32),
    /// The swap is not on the Eco rail.
    NotAnEcoSwap(Hash32),
    /// The swap has already published an intent, and the one it published is the one its
    /// reclaim must name.
    AlreadyPublished(Hash32),
    ProverNotAnAddress {
        reason: EvmAddressError,
    },
    RouteTooLong {
        len: u64,
        cap: u64,
    },
}
