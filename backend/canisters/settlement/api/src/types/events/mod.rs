use candid::{CandidType, Nat};
use serde::Deserialize;
use types::address::TextTooLong;
use types::events::EventError as DomainEventError;
use types::{Attempt, BlockNumber, ChainId, QuoteHash, TokenAmount, TxHash};

pub type Hash32 = [u8; 32];

/// What happened. The event hash is computed over a canonical encoding of the variant,
/// never over this candid layout.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub enum EventType {
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
                token: text("token", &token)?,
                amount: amount(value)?,
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
                amount: amount(value)?,
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
                token: text("token", &token)?,
                amount: amount(value)?,
                to: text("to", &to)?,
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
                amount: amount(value)?,
            },
            EventType::PocketFunded {
                chain_id,
                amount: value,
            } => Self::PocketFunded {
                chain_id: ChainId::new(chain_id),
                amount: amount(value)?,
            },
            EventType::PocketReserved {
                quote_hash,
                chain_id,
                amount: value,
            } => Self::PocketReserved {
                quote_hash: QuoteHash::new(quote_hash),
                chain_id: ChainId::new(chain_id),
                amount: amount(value)?,
            },
            EventType::PocketRebalanced {
                from_chain,
                to_chain,
                amount: value,
                route,
            } => Self::PocketRebalanced {
                from_chain: ChainId::new(from_chain),
                to_chain: ChainId::new(to_chain),
                amount: amount(value)?,
                route,
            },
            EventType::PocketReleased {
                quote_hash,
                chain_id,
                amount: value,
            } => Self::PocketReleased {
                quote_hash: QuoteHash::new(quote_hash),
                chain_id: ChainId::new(chain_id),
                amount: amount(value)?,
            },
            EventType::PocketSpent {
                quote_hash,
                chain_id,
                amount: value,
            } => Self::PocketSpent {
                quote_hash: QuoteHash::new(quote_hash),
                chain_id: ChainId::new(chain_id),
                amount: amount(value)?,
            },
            EventType::RolesChanged { quoter, watcher } => Self::RolesChanged { quoter, watcher },
        })
    }
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
    AmountTooLarge { field: String },
    TextTooLong { field: String, len: u64 },
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
        }
    }
}

fn amount(value: Nat) -> Result<TokenAmount, DomainEventError> {
    TokenAmount::from_canonical_nat(value)
        .ok_or(DomainEventError::AmountTooLarge { field: "amount" })
}

fn text<T: std::str::FromStr<Err = TextTooLong>>(
    field: &'static str,
    value: &str,
) -> Result<T, DomainEventError> {
    value
        .parse()
        .map_err(|TextTooLong { len }| DomainEventError::TextTooLong { field, len })
}

#[cfg(test)]
mod tests;
