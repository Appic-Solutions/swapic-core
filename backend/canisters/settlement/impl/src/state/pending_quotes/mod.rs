use crate::storage::memory::{pending_expiry_memory, pending_quotes_memory, Memory};
use ic_stable_structures::{StableBTreeMap, StableBTreeSet};
use settlement_api::types::errors::RegisterQuoteError;
use std::cell::RefCell;
use std::time::Duration;
use thiserror::Error;
use types::evm::EvmAddressError;
use types::quote::{QuoteAddressError, QuoteAddressField, QuoteError, MAX_QUOTE_LIFETIME};
use types::{BlockNumber, ExpiryKey, PendingQuote, Quote, QuoteHash, UnixSeconds};

/// How many quotes may sit in the pending store at once. The quoter is the only writer, so
/// this is a backstop against a compromised or looping quoter filling stable memory, not a
/// business limit.
pub const MAX_PENDING: u64 = 10_000;

thread_local! {
    // Pre-money: a registered quote is a promise the quoter made, not something that
    // happened to money, so it is not in the event log. It is in stable memory like
    // everything else, so an upgrade keeps it.
    static PENDING: RefCell<StableBTreeMap<QuoteHash, PendingQuote, Memory>> =
        RefCell::new(StableBTreeMap::init(pending_quotes_memory()));

    // The pending store keyed by expiry, so an eviction pass walks the quotes that can be
    // stale and stops at the first one that cannot: without it a pass decodes every stored
    // quote to find the few it may drop, however distant their expiries are.
    //
    // Derived from PENDING and nothing else, and PENDING is not in the event log, so this
    // index is no part of the fold: it stays out of `State::matches` and out of the replay
    // audit's comparison, which measure the log against what the log produced.
    //
    // Nothing rebuilds it, so an upgrade from a wasm without it opens on an empty index
    // against whatever PENDING holds, and those quotes are never evicted: they keep their
    // slots against MAX_PENDING until a controller calls `clear_pending_quotes`. The wasm
    // this index arrives in wants a fresh install anyway, which is what `post_upgrade`
    // says, so there is nothing to migrate from.
    static PENDING_EXPIRY: RefCell<StableBTreeSet<ExpiryKey, Memory>> =
        RefCell::new(StableBTreeSet::init(pending_expiry_memory()));
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
    #[error("the quote names no refund address, so no refund could ever be paid on it")]
    NoRefundAddress,
    #[error("the quote's refund address is not an EVM address: {reason}")]
    RefundAddressNotAnAddress { reason: EvmAddressError },
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
            RegisterError::NoRefundAddress => Self::NoRefundAddress,
            RegisterError::RefundAddressNotAnAddress { reason } => {
                Self::RefundAddressNotAnAddress {
                    reason: reason.into(),
                }
            }
        }
    }
}

/// Writes the headers of the store and its index in an update context, so no query is ever
/// the first to grow their memory.
pub fn init() {
    PENDING.with(|_| ());
    PENDING_EXPIRY.with(|_| ());
}

/// Records a quote against its hash, with the height the canister last heard of on the
/// quote's source chain: the deposit that pays this quote cannot be in an earlier block,
/// so a claim's log read starts there. `now` and `registered_at` are the caller's, so every
/// rule here is testable without a canister.
pub fn register(
    quote: Quote,
    now: UnixSeconds,
    registered_at: Option<BlockNumber>,
) -> Result<QuoteHash, RegisterError> {
    quote.validate()?;
    // a quote a refund could never be paid on is a swap that can only freeze with the
    // user's funds in the vault: the fold does not hold the payer, so the refund address
    // is the only way back, and the quoter learns here rather than after the money arrives
    match quote.evm_address(QuoteAddressField::RefundAddress) {
        Ok(_) => {}
        Err(QuoteAddressError::Absent { .. }) => return Err(RegisterError::NoRefundAddress),
        Err(QuoteAddressError::NotAnAddress { reason, .. }) => {
            return Err(RegisterError::RefundAddressNotAnAddress { reason })
        }
    }
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
    let hash = quote.hash().expect(
        "BUG: Quote::validate refuses every amount above u128::MAX, and text is capped at 256 bytes",
    );
    PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        // a re-registration is always allowed, cap or no cap: the hash covers every field,
        // so an overwrite replaces a quote with itself
        if pending.len() >= MAX_PENDING && !pending.contains_key(&hash) {
            return Err(RegisterError::StoreFull);
        }
        pending.insert(
            hash,
            PendingQuote {
                quote,
                registered_at,
            },
        );
        Ok(hash)
    })?;
    // the hash covers `expires_at`, so a re-registration carries the same key: the index
    // entry is written again rather than moved, and no stale key can be left behind
    PENDING_EXPIRY.with(|index| {
        index.borrow_mut().insert(ExpiryKey {
            expires_at,
            quote_hash: hash,
        })
    });
    Ok(hash)
}

/// The entry the store holds for a quote: the quote the quoter registered and the height
/// it was registered at.
pub fn get_pending(quote_hash: &QuoteHash) -> Option<PendingQuote> {
    PENDING.with(|p| p.borrow().get(quote_hash))
}

/// The quote alone, for a caller with no use for the height.
pub fn quote_of(quote_hash: &QuoteHash) -> Option<Quote> {
    get_pending(quote_hash).map(|entry| entry.quote)
}

/// What one eviction pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Evicted {
    /// quotes dropped by this pass
    pub dropped: usize,
    /// index entries the pass read, which is the work its cap bounds: at most one past the
    /// cap, and one entry for a store whose soonest expiry is still in its window, however
    /// many quotes sit behind it
    pub visited: usize,
    /// whether the pass stopped at its cap with another stale quote still in the store
    pub more: bool,
}

/// Drops the quotes nobody can pay any more: past their expiry plus the window a permit
/// signed against them stays valid for. Keyed on the expiry, never on when the quote was
/// registered.
///
/// Walks the expiry index rather than the store, in expiry order, so the pass stops at the
/// first quote still inside its window: everything behind it expires later. At most `cap`
/// entries are visited and at most `cap` quotes go, so a pass costs what it evicts and not
/// what the store holds, and no quote is decoded to decide its fate.
pub fn sweep_expired(now: UnixSeconds, permit_deadline: Duration, cap: usize) -> Evicted {
    let mut stale: Vec<ExpiryKey> = Vec::new();
    let mut visited = 0;
    let mut more = false;
    // the walk holds the index borrowed, so the keys come out before anything is removed
    PENDING_EXPIRY.with(|index| {
        for key in index.borrow().iter() {
            visited += 1;
            // a deadline past u64::MAX seconds never closes, and nothing behind this key
            // closes earlier
            let closed = key
                .expires_at
                .checked_add(permit_deadline)
                .is_some_and(|deadline| now > deadline);
            if !closed {
                break;
            }
            // one key past the cap is the evidence that work remains, and it is the entry
            // the next pass drops first
            if stale.len() == cap {
                more = true;
                break;
            }
            stale.push(key);
        }
    });
    PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        for key in &stale {
            pending.remove(&key.quote_hash);
        }
    });
    PENDING_EXPIRY.with(|index| {
        let mut index = index.borrow_mut();
        for key in &stale {
            index.remove(key);
        }
    });
    Evicted {
        dropped: stale.len(),
        visited,
        more,
    }
}

/// Empties the store and its index, and answers how many quotes it held. The store survives
/// upgrades, so this is how a store a looping or compromised quoter filled is recovered in
/// place.
pub fn clear() -> u64 {
    let held = PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        let held = pending.len();
        pending.clear_new();
        held
    });
    PENDING_EXPIRY.with(|index| index.borrow_mut().clear());
    held
}

/// Test-only: the expiry index as it stands, in key order.
#[cfg(test)]
pub(crate) fn expiry_index() -> Vec<ExpiryKey> {
    PENDING_EXPIRY.with(|index| index.borrow().iter().collect())
}

/// Test-only: every pending quote, in swap id order.
#[cfg(test)]
pub(crate) fn all_pending() -> Vec<(QuoteHash, PendingQuote)> {
    PENDING.with(|p| p.borrow().iter().collect())
}

#[cfg(test)]
mod tests;
