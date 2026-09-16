use std::cell::RefCell;
use std::collections::BTreeMap;
use thiserror::Error;
use types::quote::{QuoteError, MAX_QUOTE_LIFETIME};
use types::{Quote, QuoteHash, UnixSeconds};

/// How many quotes may sit in the pending store at once. The store is heap, and the
/// quoter is the only writer, so this is a backstop against a compromised or looping
/// quoter growing the canister until it traps, not a business limit.
pub const MAX_PENDING: usize = 10_000;

thread_local! {
    // Pre-money state, and heap-only BY DESIGN: a registered quote is a promise the quoter
    // made, not something that happened to money, so it is neither in the event log nor in
    // stable memory. An upgrade drops the map and the quoter re-registers what is live.
    static PENDING: RefCell<BTreeMap<QuoteHash, Quote>> = const { RefCell::new(BTreeMap::new()) };
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
        // a re-registration is always allowed, cap or no cap: the quoter replays its live
        // quotes after an upgrade, and refusing those would strand swaps that already
        // exist. The hash covers every field, so an overwrite replaces a quote with itself.
        if pending.len() >= MAX_PENDING && !pending.contains_key(&hash) {
            return Err(RegisterError::StoreFull);
        }
        pending.insert(hash, quote);
        Ok(hash)
    })
}

pub fn get_pending(quote_hash: &QuoteHash) -> Option<Quote> {
    PENDING.with(|p| p.borrow().get(quote_hash).cloned())
}

/// Drops the quotes nobody can pay any more: past their expiry plus the window a permit
/// signed against them stays valid for. Keyed on the expiry, never on when the quote was
/// registered, which a re-registration resets. Returns how many went.
pub fn sweep_expired(now: UnixSeconds, permit_deadline: std::time::Duration) -> usize {
    PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        let before = pending.len();
        // a deadline past u64::MAX seconds never closes
        pending.retain(|_, quote| {
            quote
                .expires_at
                .checked_add(permit_deadline)
                .is_none_or(|deadline| now <= deadline)
        });
        before - pending.len()
    })
}

/// Tests share one PENDING when the harness runs them on a single thread, so the store
/// tests start from a known map instead of assuming an empty one.
#[cfg(test)]
pub(crate) fn clear_pending() {
    PENDING.with(|p| p.borrow_mut().clear());
}

#[cfg(test)]
mod tests;
