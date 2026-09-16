use settlement_api::types::events::Hash32;
use settlement_api::types::quote::{quote_hash, Quote, MAX_QUOTE_LIFETIME_S};
use std::cell::RefCell;
use std::collections::BTreeMap;

/// How many quotes may sit in the pending store at once. The store is heap, and the
/// quoter is the only writer, so this is a backstop against a compromised or looping
/// quoter growing the canister until it traps, not a business limit.
pub const MAX_PENDING: usize = 10_000;

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

/// Drops the quotes nobody can pay any more: past their expiry plus the window a permit
/// signed against them stays valid for. Keyed on `expires_at_s` and deliberately not on
/// the registration time beside it, which a re-registration resets. Returns how many went.
pub fn sweep_expired(now_s: u64, permit_deadline_s: u64) -> usize {
    PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        let before = pending.len();
        pending.retain(|_, (q, _)| now_s <= q.expires_at_s.saturating_add(permit_deadline_s));
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
