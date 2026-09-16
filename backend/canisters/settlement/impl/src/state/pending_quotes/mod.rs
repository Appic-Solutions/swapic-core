use crate::storage::memory::{pending_quotes_memory, Memory};
use ic_stable_structures::StableBTreeMap;
use settlement_api::types::errors::RegisterQuoteError;
use std::cell::RefCell;
use std::time::Duration;
use thiserror::Error;
use types::quote::{QuoteError, MAX_QUOTE_LIFETIME};
use types::{Quote, QuoteHash, UnixSeconds};

/// How many quotes may sit in the pending store at once. The quoter is the only writer, so
/// this is a backstop against a compromised or looping quoter filling stable memory, not a
/// business limit.
pub const MAX_PENDING: u64 = 10_000;

thread_local! {
    // Pre-money: a registered quote is a promise the quoter made, not something that
    // happened to money, so it is not in the event log. It is in stable memory like
    // everything else, so an upgrade keeps it.
    static PENDING: RefCell<StableBTreeMap<QuoteHash, Quote, Memory>> =
        RefCell::new(StableBTreeMap::init(pending_quotes_memory()));
}

/// Why a quote was not registered.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RegisterError {
    #[error(transparent)]
    InvalidQuote(#[from] QuoteError),
    #[error("quote expired at {expires_at} and it is now {now}")]
    Expired {
        expires_at: UnixSeconds,
        now: UnixSeconds,
    },
    #[error("quote expires at {expires_at}, more than {}s ahead of {now}", MAX_QUOTE_LIFETIME.as_secs())]
    ExpiresTooFarAhead {
        expires_at: UnixSeconds,
        now: UnixSeconds,
    },
    #[error("pending store is full at {MAX_PENDING} quotes")]
    StoreFull,
}

impl From<RegisterError> for RegisterQuoteError {
    fn from(error: RegisterError) -> Self {
        match error {
            RegisterError::InvalidQuote(error) => Self::InvalidQuote(error.into()),
            RegisterError::Expired { expires_at, now } => Self::Expired {
                expires_at_s: expires_at.get(),
                now_s: now.get(),
            },
            RegisterError::ExpiresTooFarAhead { expires_at, now } => Self::ExpiresTooFarAhead {
                expires_at_s: expires_at.get(),
                now_s: now.get(),
                max_lifetime_s: MAX_QUOTE_LIFETIME.as_secs(),
            },
            RegisterError::StoreFull => Self::StoreFull {
                capacity: MAX_PENDING,
            },
        }
    }
}

/// Writes the store's header in an update context, so no query is ever the first to grow
/// its memory.
pub fn init() {
    PENDING.with(|_| ());
}

/// Records a quote against its hash. `now` is the caller's clock, so every rule here is
/// testable without a canister.
pub fn register(quote: Quote, now: UnixSeconds) -> Result<QuoteHash, RegisterError> {
    quote.validate()?;
    let expires_at = quote.expires_at;
    // the quote is good through the whole of its expiry second
    if now > expires_at {
        return Err(RegisterError::Expired { expires_at, now });
    }
    // a window reaching past u64::MAX seconds holds every expiry
    if now
        .checked_add(MAX_QUOTE_LIFETIME)
        .is_some_and(|latest| expires_at > latest)
    {
        return Err(RegisterError::ExpiresTooFarAhead { expires_at, now });
    }
    let hash = quote.hash();
    PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        // a re-registration is always allowed, cap or no cap: the hash covers every field,
        // so an overwrite replaces a quote with itself
        if pending.len() >= MAX_PENDING && !pending.contains_key(&hash) {
            return Err(RegisterError::StoreFull);
        }
        pending.insert(hash, quote);
        Ok(hash)
    })
}

pub fn get_pending(quote_hash: &QuoteHash) -> Option<Quote> {
    PENDING.with(|p| p.borrow().get(quote_hash))
}

/// Drops the quotes nobody can pay any more: past their expiry plus the window a permit
/// signed against them stays valid for. Keyed on the expiry, never on when the quote was
/// registered. Returns how many went.
pub fn sweep_expired(now: UnixSeconds, permit_deadline: Duration) -> usize {
    PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        // a deadline past u64::MAX seconds never closes
        let stale: Vec<QuoteHash> = pending
            .iter()
            .filter(|(_, quote)| {
                quote
                    .expires_at
                    .checked_add(permit_deadline)
                    .is_some_and(|deadline| now > deadline)
            })
            .map(|(hash, _)| hash)
            .collect();
        for hash in &stale {
            pending.remove(hash);
        }
        stale.len()
    })
}

/// Tests share one store when the harness runs them on a single thread, so the store
/// tests start from a known map instead of assuming an empty one.
#[cfg(test)]
pub(crate) fn clear_pending() {
    PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        let all: Vec<QuoteHash> = pending.iter().map(|(hash, _)| hash).collect();
        for hash in all {
            pending.remove(&hash);
        }
    });
}

#[cfg(test)]
mod tests;
