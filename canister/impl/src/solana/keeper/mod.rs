//! The canister's Solana keeper jobs (design 5.2.8): the watcher asks and brings a blockhash; the
//! canister decides and signs. Every job's money goes where the program or the canister's own build
//! sends it, never where the watcher says.

#[cfg(test)]
mod tests;

/// How long a burn's event account must have stood before CCTP gives its rent back.
pub const RECLAIM_AFTER_S: u64 = 5 * 24 * 3600;
/// The most `reclaim` instructions one v1 transaction holds.
pub const MAX_RECLAIMS: usize = 4;
/// A top-up at most once an hour.
pub const TOP_UP_EVERY_S: u64 = 3600;
/// A stuck Forwarder message is relayed at most twice.
pub const MAX_FORWARD_TRIES: u64 = 2;

/// Whether a ref may be reclaimed now: recorded as burned through Swapic, past CCTP's window, and with
/// its one retry left.
pub fn reclaimable(burn: Option<(u64, u32)>, now_s: u64) -> Result<(), &'static str> {
    let (at_s, tries) = burn.ok_or("the ref is not one the canister recorded")?;
    if now_s < at_s + RECLAIM_AFTER_S {
        return Err("the ref's five days have not passed");
    }
    if tries >= 2 {
        return Err("the ref's reclaim was tried twice");
    }
    Ok(())
}

/// The lamports a top-up sends, if one is due: up to the target, and only when below the floor, at
/// most once an hour.
pub fn top_up(rent_lamports: u64, low: u64, target: u64, last_s: Option<u64>, now_s: u64) -> Option<u64> {
    if rent_lamports >= low {
        return None;
    }
    if last_s.is_some_and(|last| now_s < last + TOP_UP_EVERY_S) {
        return None;
    }
    target.checked_sub(rent_lamports).filter(|amount| *amount > 0)
}
