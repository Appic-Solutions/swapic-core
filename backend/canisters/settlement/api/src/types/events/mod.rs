use crate::types::swap::TransitionError;
use candid::{CandidType, Nat};
use serde::Deserialize;
use types::events::EventError as DomainEventError;
use types::{
    Attempt, BlockNumber, ChainId, GasAmount, Nonce, QuoteHash, TokenAmount, TxHash, Wei, WeiPerGas,
};

pub type Hash32 = [u8; 32];

/// The wire amounts named `amount`. Every other amount field names itself through
/// [`DomainEventError::amount_too_large`], because "amount is above u128::MAX" is not an
/// answer about a gas limit.
const AMOUNT_TOO_LARGE: DomainEventError = DomainEventError::AmountTooLarge { field: "amount" };

/// The wire error a field named `field` answers when its value is above `u128::MAX`.
fn too_large(field: &'static str) -> DomainEventError {
    DomainEventError::amount_too_large(field)
}

/// What happened. The event hash is computed over a canonical encoding of the variant,
/// never over this candid layout.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum EventType {
    /// `json` is the public view of the config written: JSON of `Config` with every rpc url
    /// as `"***"` and `max_swap_usd` as a decimal string.
    ConfigChanged {
        json: String,
    },
    FundsReceived {
        quote_hash: Hash32,
        quote_bytes: Vec<u8>,
        chain_id: u64,
        token: String,
        amount: Nat,
        tx_ref: String,
    },
    TxSigned {
        quote_hash: Hash32,
        attempt: u32,
        chain_id: u64,
        tx_hash: Hash32,
        raw_tx: Vec<u8>,
    },
    TxConfirmed {
        quote_hash: Hash32,
        attempt: u32,
        chain_id: u64,
        tx_hash: Hash32,
        block: u64,
    },
    TxFailed {
        quote_hash: Hash32,
        attempt: u32,
        reason: String,
    },
    PaidInStable {
        quote_hash: Hash32,
        chain_id: u64,
        amount: Nat,
    },
    DecisionRequired {
        quote_hash: Hash32,
        reason: String,
    },
    DecisionMade {
        quote_hash: Hash32,
        choice: Choice,
    },
    RefundStarted {
        quote_hash: Hash32,
        reason: String,
    },
    Refunded {
        quote_hash: Hash32,
        chain_id: u64,
        token: String,
        amount: Nat,
        to: String,
    },
    SwapDone {
        quote_hash: Hash32,
    },
    Frozen {
        quote_hash: Hash32,
        reason: String,
    },
    FeeAccrued {
        quote_hash: Hash32,
        amount: Nat,
    },
    PocketFunded {
        chain_id: u64,
        amount: Nat,
    },
    PocketReserved {
        quote_hash: Hash32,
        chain_id: u64,
        amount: Nat,
    },
    PocketRebalanced {
        from_chain: u64,
        to_chain: u64,
        amount: Nat,
        route: String,
    },
    PocketReleased {
        quote_hash: Hash32,
        chain_id: u64,
        amount: Nat,
    },
    PocketSpent {
        quote_hash: Hash32,
        chain_id: u64,
        amount: Nat,
    },
    /// Principals as text, so the audit line reads without a decoder.
    RolesChanged {
        quoter: String,
        watcher: String,
    },
    /// A swap's waiting index entries were made to agree with the swap: what no event could
    /// have produced was dropped, and the wait the swap is in is there. Only a divergence
    /// needs one, so this line is the repair's explanation.
    WaitingRepaired {
        quote_hash: Hash32,
    },
    /// A nonce was allocated to an outbound transaction, before the signature was asked
    /// for. `to` is an EIP-55 address.
    TxCreated {
        purpose: TxPurpose,
        chain_id: u64,
        nonce: u64,
        to: String,
        value_wei: Nat,
        data: Vec<u8>,
        gas_limit: Nat,
        max_fee_wei_per_gas: Nat,
        max_priority_fee_wei_per_gas: Nat,
    },
    /// An open transaction was re-sent at the same nonce with a higher fee, with the bytes
    /// that were broadcast.
    TxReplaced {
        purpose: TxPurpose,
        chain_id: u64,
        nonce: u64,
        max_fee_wei_per_gas: Nat,
        max_priority_fee_wei_per_gas: Nat,
        tx_hash: Hash32,
        raw_tx: Vec<u8>,
    },
    /// A nonce that was allocated and never signed for was spent by a zero-value
    /// self-transfer, so the chain is never left with a gap it cannot mine past.
    TxCancelled {
        chain_id: u64,
        nonce: u64,
        tx_hash: Hash32,
        raw_tx: Vec<u8>,
    },
    /// A gasless pull was signed: the transaction that takes a user's funds into the vault
    /// with their permit, recorded before it is broadcast. It names the quote and the nonce
    /// and no swap, because the deposit it makes is what `claim_swap` then verifies.
    PullSigned {
        quote_hash: Hash32,
        chain_id: u64,
        nonce: u64,
        tx_hash: Hash32,
        raw_tx: Vec<u8>,
    },
}

/// Why an outbound transaction exists. All but the last name the swap they belong to.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxPurpose {
    Burn(Hash32),
    Mint(Hash32),
    Payout(Hash32),
    Refund(Hash32),
    GaslessPull(Hash32),
    Cancel(u64),
}

impl From<types::events::TxPurpose> for TxPurpose {
    fn from(purpose: types::events::TxPurpose) -> Self {
        use types::events::TxPurpose as Domain;
        match purpose {
            Domain::Burn(hash) => Self::Burn(hash.into_bytes()),
            Domain::Mint(hash) => Self::Mint(hash.into_bytes()),
            Domain::Payout(hash) => Self::Payout(hash.into_bytes()),
            Domain::Refund(hash) => Self::Refund(hash.into_bytes()),
            Domain::GaslessPull(hash) => Self::GaslessPull(hash.into_bytes()),
            Domain::Cancel(chain_id) => Self::Cancel(chain_id.get()),
        }
    }
}

impl From<TxPurpose> for types::events::TxPurpose {
    fn from(purpose: TxPurpose) -> Self {
        match purpose {
            TxPurpose::Burn(hash) => Self::Burn(QuoteHash::new(hash)),
            TxPurpose::Mint(hash) => Self::Mint(QuoteHash::new(hash)),
            TxPurpose::Payout(hash) => Self::Payout(QuoteHash::new(hash)),
            TxPurpose::Refund(hash) => Self::Refund(QuoteHash::new(hash)),
            TxPurpose::GaslessPull(hash) => Self::GaslessPull(QuoteHash::new(hash)),
            TxPurpose::Cancel(chain_id) => Self::Cancel(ChainId::new(chain_id)),
        }
    }
}

#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum Choice {
    Requote,
    Refund,
}

/// One entry of the log. `time_ns` is IC time in nanoseconds.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub struct Event {
    pub index: u64,
    pub time_ns: u64,
    pub parent_hash: Hash32,
    pub hash: Hash32,
    pub payload: EventType,
}

impl From<types::Choice> for Choice {
    fn from(choice: types::Choice) -> Self {
        match choice {
            types::Choice::Requote => Self::Requote,
            types::Choice::Refund => Self::Refund,
        }
    }
}

impl From<Choice> for types::Choice {
    fn from(choice: Choice) -> Self {
        match choice {
            Choice::Requote => Self::Requote,
            Choice::Refund => Self::Refund,
        }
    }
}

impl From<types::Event> for Event {
    fn from(event: types::Event) -> Self {
        Self {
            index: event.index.get(),
            time_ns: event.timestamp.as_nanos(),
            parent_hash: event.parent_hash.into_bytes(),
            hash: event.hash.into_bytes(),
            payload: event.payload.into(),
        }
    }
}

impl From<types::EventType> for EventType {
    fn from(payload: types::EventType) -> Self {
        use types::EventType as Domain;
        match payload {
            Domain::ConfigChanged { json } => Self::ConfigChanged { json },
            Domain::FundsReceived {
                quote_hash,
                quote_bytes,
                chain_id,
                token,
                amount,
                tx_ref,
            } => Self::FundsReceived {
                quote_hash: quote_hash.into_bytes(),
                quote_bytes,
                chain_id: chain_id.get(),
                token: token.to_string(),
                amount: amount.into(),
                tx_ref,
            },
            Domain::TxSigned {
                quote_hash,
                attempt,
                chain_id,
                tx_hash,
                raw_tx,
            } => Self::TxSigned {
                quote_hash: quote_hash.into_bytes(),
                attempt: attempt.get(),
                chain_id: chain_id.get(),
                tx_hash: tx_hash.into_bytes(),
                raw_tx,
            },
            Domain::TxConfirmed {
                quote_hash,
                attempt,
                chain_id,
                tx_hash,
                block,
            } => Self::TxConfirmed {
                quote_hash: quote_hash.into_bytes(),
                attempt: attempt.get(),
                chain_id: chain_id.get(),
                tx_hash: tx_hash.into_bytes(),
                block: block.get(),
            },
            Domain::TxFailed {
                quote_hash,
                attempt,
                reason,
            } => Self::TxFailed {
                quote_hash: quote_hash.into_bytes(),
                attempt: attempt.get(),
                reason,
            },
            Domain::PaidInStable {
                quote_hash,
                chain_id,
                amount,
            } => Self::PaidInStable {
                quote_hash: quote_hash.into_bytes(),
                chain_id: chain_id.get(),
                amount: amount.into(),
            },
            Domain::DecisionRequired { quote_hash, reason } => Self::DecisionRequired {
                quote_hash: quote_hash.into_bytes(),
                reason,
            },
            Domain::DecisionMade { quote_hash, choice } => Self::DecisionMade {
                quote_hash: quote_hash.into_bytes(),
                choice: choice.into(),
            },
            Domain::RefundStarted { quote_hash, reason } => Self::RefundStarted {
                quote_hash: quote_hash.into_bytes(),
                reason,
            },
            Domain::Refunded {
                quote_hash,
                chain_id,
                token,
                amount,
                to,
            } => Self::Refunded {
                quote_hash: quote_hash.into_bytes(),
                chain_id: chain_id.get(),
                token: token.to_string(),
                amount: amount.into(),
                to: to.to_string(),
            },
            Domain::SwapDone { quote_hash } => Self::SwapDone {
                quote_hash: quote_hash.into_bytes(),
            },
            Domain::Frozen { quote_hash, reason } => Self::Frozen {
                quote_hash: quote_hash.into_bytes(),
                reason,
            },
            Domain::FeeAccrued { quote_hash, amount } => Self::FeeAccrued {
                quote_hash: quote_hash.into_bytes(),
                amount: amount.into(),
            },
            Domain::PocketFunded { chain_id, amount } => Self::PocketFunded {
                chain_id: chain_id.get(),
                amount: amount.into(),
            },
            Domain::PocketReserved {
                quote_hash,
                chain_id,
                amount,
            } => Self::PocketReserved {
                quote_hash: quote_hash.into_bytes(),
                chain_id: chain_id.get(),
                amount: amount.into(),
            },
            Domain::PocketRebalanced {
                from_chain,
                to_chain,
                amount,
                route,
            } => Self::PocketRebalanced {
                from_chain: from_chain.get(),
                to_chain: to_chain.get(),
                amount: amount.into(),
                route,
            },
            Domain::PocketReleased {
                quote_hash,
                chain_id,
                amount,
            } => Self::PocketReleased {
                quote_hash: quote_hash.into_bytes(),
                chain_id: chain_id.get(),
                amount: amount.into(),
            },
            Domain::PocketSpent {
                quote_hash,
                chain_id,
                amount,
            } => Self::PocketSpent {
                quote_hash: quote_hash.into_bytes(),
                chain_id: chain_id.get(),
                amount: amount.into(),
            },
            Domain::RolesChanged { quoter, watcher } => Self::RolesChanged { quoter, watcher },
            Domain::WaitingRepaired { quote_hash } => Self::WaitingRepaired {
                quote_hash: quote_hash.into_bytes(),
            },
            Domain::TxCreated {
                purpose,
                chain_id,
                nonce,
                to,
                value,
                data,
                gas_limit,
                max_fee,
                max_priority_fee,
            } => Self::TxCreated {
                purpose: purpose.into(),
                chain_id: chain_id.get(),
                nonce: nonce.get(),
                to: to.to_string(),
                value_wei: value.into(),
                data,
                gas_limit: gas_limit.into(),
                max_fee_wei_per_gas: max_fee.into(),
                max_priority_fee_wei_per_gas: max_priority_fee.into(),
            },
            Domain::TxReplaced {
                purpose,
                chain_id,
                nonce,
                max_fee,
                max_priority_fee,
                tx_hash,
                raw_tx,
            } => Self::TxReplaced {
                purpose: purpose.into(),
                chain_id: chain_id.get(),
                nonce: nonce.get(),
                max_fee_wei_per_gas: max_fee.into(),
                max_priority_fee_wei_per_gas: max_priority_fee.into(),
                tx_hash: tx_hash.into_bytes(),
                raw_tx,
            },
            Domain::TxCancelled {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            } => Self::TxCancelled {
                chain_id: chain_id.get(),
                nonce: nonce.get(),
                tx_hash: tx_hash.into_bytes(),
                raw_tx,
            },
            Domain::PullSigned {
                quote_hash,
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            } => Self::PullSigned {
                quote_hash: quote_hash.into_bytes(),
                chain_id: chain_id.get(),
                nonce: nonce.get(),
                tx_hash: tx_hash.into_bytes(),
                raw_tx,
            },
        }
    }
}

impl TryFrom<EventType> for types::EventType {
    type Error = DomainEventError;

    fn try_from(payload: EventType) -> Result<Self, Self::Error> {
        Ok(match payload {
            EventType::ConfigChanged { json } => Self::ConfigChanged { json },
            EventType::FundsReceived {
                quote_hash,
                quote_bytes,
                chain_id,
                token,
                amount: value,
                tx_ref,
            } => Self::FundsReceived {
                quote_hash: QuoteHash::new(quote_hash),
                quote_bytes,
                chain_id: ChainId::new(chain_id),
                token: token
                    .parse()
                    .map_err(DomainEventError::text_too_long("token"))?,
                amount: TokenAmount::from_canonical_nat(value).ok_or(AMOUNT_TOO_LARGE)?,
                tx_ref,
            },
            EventType::TxSigned {
                quote_hash,
                attempt,
                chain_id,
                tx_hash,
                raw_tx,
            } => Self::TxSigned {
                quote_hash: QuoteHash::new(quote_hash),
                attempt: Attempt::new(attempt),
                chain_id: ChainId::new(chain_id),
                tx_hash: TxHash::new(tx_hash),
                raw_tx,
            },
            EventType::TxConfirmed {
                quote_hash,
                attempt,
                chain_id,
                tx_hash,
                block,
            } => Self::TxConfirmed {
                quote_hash: QuoteHash::new(quote_hash),
                attempt: Attempt::new(attempt),
                chain_id: ChainId::new(chain_id),
                tx_hash: TxHash::new(tx_hash),
                block: BlockNumber::new(block),
            },
            EventType::TxFailed {
                quote_hash,
                attempt,
                reason,
            } => Self::TxFailed {
                quote_hash: QuoteHash::new(quote_hash),
                attempt: Attempt::new(attempt),
                reason,
            },
            EventType::PaidInStable {
                quote_hash,
                chain_id,
                amount: value,
            } => Self::PaidInStable {
                quote_hash: QuoteHash::new(quote_hash),
                chain_id: ChainId::new(chain_id),
                amount: TokenAmount::from_canonical_nat(value).ok_or(AMOUNT_TOO_LARGE)?,
            },
            EventType::DecisionRequired { quote_hash, reason } => Self::DecisionRequired {
                quote_hash: QuoteHash::new(quote_hash),
                reason,
            },
            EventType::DecisionMade { quote_hash, choice } => Self::DecisionMade {
                quote_hash: QuoteHash::new(quote_hash),
                choice: choice.into(),
            },
            EventType::RefundStarted { quote_hash, reason } => Self::RefundStarted {
                quote_hash: QuoteHash::new(quote_hash),
                reason,
            },
            EventType::Refunded {
                quote_hash,
                chain_id,
                token,
                amount: value,
                to,
            } => Self::Refunded {
                quote_hash: QuoteHash::new(quote_hash),
                chain_id: ChainId::new(chain_id),
                token: token
                    .parse()
                    .map_err(DomainEventError::text_too_long("token"))?,
                amount: TokenAmount::from_canonical_nat(value).ok_or(AMOUNT_TOO_LARGE)?,
                to: to.parse().map_err(DomainEventError::text_too_long("to"))?,
            },
            EventType::SwapDone { quote_hash } => Self::SwapDone {
                quote_hash: QuoteHash::new(quote_hash),
            },
            EventType::Frozen { quote_hash, reason } => Self::Frozen {
                quote_hash: QuoteHash::new(quote_hash),
                reason,
            },
            EventType::FeeAccrued {
                quote_hash,
                amount: value,
            } => Self::FeeAccrued {
                quote_hash: QuoteHash::new(quote_hash),
                amount: TokenAmount::from_canonical_nat(value).ok_or(AMOUNT_TOO_LARGE)?,
            },
            EventType::PocketFunded {
                chain_id,
                amount: value,
            } => Self::PocketFunded {
                chain_id: ChainId::new(chain_id),
                amount: TokenAmount::from_canonical_nat(value).ok_or(AMOUNT_TOO_LARGE)?,
            },
            EventType::PocketReserved {
                quote_hash,
                chain_id,
                amount: value,
            } => Self::PocketReserved {
                quote_hash: QuoteHash::new(quote_hash),
                chain_id: ChainId::new(chain_id),
                amount: TokenAmount::from_canonical_nat(value).ok_or(AMOUNT_TOO_LARGE)?,
            },
            EventType::PocketRebalanced {
                from_chain,
                to_chain,
                amount: value,
                route,
            } => Self::PocketRebalanced {
                from_chain: ChainId::new(from_chain),
                to_chain: ChainId::new(to_chain),
                amount: TokenAmount::from_canonical_nat(value).ok_or(AMOUNT_TOO_LARGE)?,
                route,
            },
            EventType::PocketReleased {
                quote_hash,
                chain_id,
                amount: value,
            } => Self::PocketReleased {
                quote_hash: QuoteHash::new(quote_hash),
                chain_id: ChainId::new(chain_id),
                amount: TokenAmount::from_canonical_nat(value).ok_or(AMOUNT_TOO_LARGE)?,
            },
            EventType::PocketSpent {
                quote_hash,
                chain_id,
                amount: value,
            } => Self::PocketSpent {
                quote_hash: QuoteHash::new(quote_hash),
                chain_id: ChainId::new(chain_id),
                amount: TokenAmount::from_canonical_nat(value).ok_or(AMOUNT_TOO_LARGE)?,
            },
            EventType::RolesChanged { quoter, watcher } => Self::RolesChanged { quoter, watcher },
            EventType::WaitingRepaired { quote_hash } => Self::WaitingRepaired {
                quote_hash: QuoteHash::new(quote_hash),
            },
            EventType::TxCreated {
                purpose,
                chain_id,
                nonce,
                to,
                value_wei,
                data,
                gas_limit,
                max_fee_wei_per_gas,
                max_priority_fee_wei_per_gas,
            } => Self::TxCreated {
                purpose: purpose.into(),
                chain_id: ChainId::new(chain_id),
                nonce: Nonce::new(nonce),
                to: to.parse().map_err(DomainEventError::not_an_address("to"))?,
                value: Wei::try_from(value_wei).map_err(|_| too_large("value_wei"))?,
                data,
                gas_limit: GasAmount::try_from(gas_limit).map_err(|_| too_large("gas_limit"))?,
                max_fee: WeiPerGas::try_from(max_fee_wei_per_gas)
                    .map_err(|_| too_large("max_fee_wei_per_gas"))?,
                max_priority_fee: WeiPerGas::try_from(max_priority_fee_wei_per_gas)
                    .map_err(|_| too_large("max_priority_fee_wei_per_gas"))?,
            },
            EventType::TxReplaced {
                purpose,
                chain_id,
                nonce,
                max_fee_wei_per_gas,
                max_priority_fee_wei_per_gas,
                tx_hash,
                raw_tx,
            } => Self::TxReplaced {
                purpose: purpose.into(),
                chain_id: ChainId::new(chain_id),
                nonce: Nonce::new(nonce),
                max_fee: WeiPerGas::try_from(max_fee_wei_per_gas)
                    .map_err(|_| too_large("max_fee_wei_per_gas"))?,
                max_priority_fee: WeiPerGas::try_from(max_priority_fee_wei_per_gas)
                    .map_err(|_| too_large("max_priority_fee_wei_per_gas"))?,
                tx_hash: TxHash::new(tx_hash),
                raw_tx,
            },
            EventType::TxCancelled {
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            } => Self::TxCancelled {
                chain_id: ChainId::new(chain_id),
                nonce: Nonce::new(nonce),
                tx_hash: TxHash::new(tx_hash),
                raw_tx,
            },
            EventType::PullSigned {
                quote_hash,
                chain_id,
                nonce,
                tx_hash,
                raw_tx,
            } => Self::PullSigned {
                quote_hash: QuoteHash::new(quote_hash),
                chain_id: ChainId::new(chain_id),
                nonce: Nonce::new(nonce),
                tx_hash: TxHash::new(tx_hash),
                raw_tx,
            },
        })
    }
}

/// Why the log does not fold. The replay stopped at `index` and applied nothing from there.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum ReplayError {
    OutOfSequence {
        expected: u64,
        found: u64,
    },
    Unlinked {
        index: u64,
        parent: Hash32,
        head: Hash32,
    },
    LogFull {
        index: u64,
    },
    Refused {
        index: u64,
        error: TransitionError,
    },
}

/// What one step of the deep check found.
///
/// `finished` is the field to read first: only a step that reached the head compared the
/// fold with the live one, and `matches` is its verdict. A step short of the head reports
/// how far the fold got and how much is left, and condemns nothing. An entry the fold
/// refuses, or a finished fold that differs, is a divergence and halts the canister; every
/// verdict starts the next audit over from genesis.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub struct AuditProgress {
    /// entries folded from genesis, this step's included
    pub folded_so_far: u64,
    /// entries between the fold and the head, as the log stood when this step read it
    pub remaining: u64,
    pub finished: bool,
    /// whether the fold is the live fold, which only a finished step says
    pub matches: bool,
    /// why the fold stopped, if it did
    pub refused: Option<ReplayError>,
    /// whether this step halted the canister
    pub halted: bool,
}

/// A value the canonical preimage has no bytes for.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum CanonicalError {
    AmountTooLarge(Nat),
    TooLong { len: u64 },
}

impl From<types::canonical::CanonicalError> for CanonicalError {
    fn from(error: types::canonical::CanonicalError) -> Self {
        use types::canonical::CanonicalError as Domain;
        match error {
            Domain::AmountTooLarge(amount) => Self::AmountTooLarge(amount.into()),
            Domain::TooLong { len } => Self::TooLong {
                len: crate::types::wire_len(len),
            },
        }
    }
}

/// Why a wire event is not a domain event, naming the field at fault.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum EventError {
    AmountTooLarge {
        field: String,
    },
    TextTooLong {
        field: String,
        len: u64,
    },
    /// The text in `field` is not an EVM address, and `reason` says which way.
    NotAnAddress {
        field: String,
        reason: EvmAddressError,
    },
}

/// Why a text is not an EVM address. Mirrors `types::evm::EvmAddressError` so a caller
/// replaying a log is told which of the four rules the text broke, rather than being told
/// something that is false for three of them.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum EvmAddressError {
    NoPrefix,
    WrongLength { len: u64 },
    NotHex,
    BadChecksum,
}

impl From<types::evm::EvmAddressError> for EvmAddressError {
    fn from(error: types::evm::EvmAddressError) -> Self {
        use types::evm::EvmAddressError as Domain;
        match error {
            Domain::NoPrefix => Self::NoPrefix,
            Domain::WrongLength { len } => Self::WrongLength {
                len: crate::types::wire_len(len),
            },
            Domain::NotHex => Self::NotHex,
            Domain::BadChecksum => Self::BadChecksum,
        }
    }
}

impl From<types::events::EventError> for EventError {
    fn from(error: types::events::EventError) -> Self {
        use types::events::EventError as Domain;
        match error {
            Domain::AmountTooLarge { field } => Self::AmountTooLarge {
                field: field.to_string(),
            },
            Domain::TextTooLong { field, len } => Self::TextTooLong {
                field: field.to_string(),
                len: crate::types::wire_len(len),
            },
            Domain::NotAnAddress { field, reason } => Self::NotAnAddress {
                field: field.to_string(),
                reason: reason.into(),
            },
        }
    }
}

#[cfg(test)]
mod tests;
