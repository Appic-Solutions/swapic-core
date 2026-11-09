//! The watcher's nudge (Jeff, 2026-11-10): the canister keeps its one regular read of an
//! Across payout's fill at its deadline, and reads once more, at once, when the watcher has
//! seen the fill on the destination (`check_across_fill`). A fill read at an agreed block is
//! final whenever it is read, so a filled deposit is paid then; anything else changes nothing
//! but the time of the read (`PayoutFillEarly`), and the deadline read stays the only one that
//! finds a deposit unfilled. One read per fill wait: a nudge within one wait of the last early
//! read is not read.

#[cfg(test)]
mod tests;

use crate::agreed::{AcrossFill, FillRead};
use crate::pool::queue::ChainReads;
use settlement_api::types::pool::{AcrossFillCheck, PoolError};
use types::pool::payout::{EvmPayout, PayoutStatus};
use types::pool::PoolStep;
use types::{ChainId, Timestamp};

/// What a nudge does with a payout, before any read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// Its deposit's fill read on `chain`, an open read concluding nothing before `after_s`.
    Read { chain: ChainId, after_s: u64 },
    /// Read early within one wait: not read before `again_at_s`.
    TooSoon { again_at_s: u64 },
    /// No longer delivering: nothing to read.
    NotDelivering,
}

/// What a nudge does with `payout` at `now_s`, `wait_s` the destination's fill wait, pure: a
/// payout with no Across deposit is refused; one that no longer delivers, or was read early
/// within one wait, is not read.
pub fn gate(payout: &EvmPayout, now_s: u64, wait_s: u64) -> Result<Gate, PoolError> {
    let Some(across) = payout.across.as_ref() else {
        return Err(PoolError::NotAllowed(format!(
            "the payout {} made no Across deposit",
            payout.id
        )));
    };
    if payout.status != PayoutStatus::Delivering {
        return Ok(Gate::NotDelivering);
    }
    if let Some(early) = payout.fill_early_at {
        let again_at_s = early.as_secs().get().saturating_add(wait_s);
        if now_s < again_at_s {
            return Ok(Gate::TooSoon { again_at_s });
        }
    }
    Ok(Gate::Read {
        chain: across.destination,
        after_s: across.fill_deadline(),
    })
}

/// The door's body after its guards: payout `id`'s deposit's fill, read once through `reads`
/// at `clock`. Filled: paid, as the queue pays it. Anything else read: the read's time noted.
/// An outcall that failed: `Unavailable`, nothing noted, ask again.
pub async fn check(
    reads: &impl ChainReads,
    id: u64,
    clock: impl Fn() -> Timestamp,
) -> Result<AcrossFillCheck, PoolError> {
    let payout = crate::storage::pool::payout(id).ok_or(PoolError::Unknown)?;
    let destination = payout
        .across
        .as_ref()
        .map_or(payout.terms.dst_chain, |across| across.destination);
    let wait_s = crate::agreed::fill_wait_on(destination).as_secs();
    let (chain, after_s) = match gate(&payout, clock().as_secs().get(), wait_s)? {
        Gate::Read { chain, after_s } => (chain, after_s),
        Gate::TooSoon { again_at_s } => return Ok(AcrossFillCheck::TooSoon { again_at_s }),
        Gate::NotDelivering => return Ok(AcrossFillCheck::NotDelivering),
    };
    let Some(across) = payout.across.as_ref() else {
        return Ok(AcrossFillCheck::NotDelivering);
    };
    let step = match reads.across_fill_after(chain, across, after_s).await {
        Ok((AcrossFill::Filled, header)) => PoolStep::PayoutDelivered {
            id,
            block: header.number,
        },
        // open, too early, or a hash the deadline read will judge: only the read's time
        Ok(_) | Err(FillRead::TooEarly) => PoolStep::PayoutFillEarly { id },
        Err(FillRead::Unread(why)) => return Err(PoolError::Unavailable(why)),
    };
    let paid = matches!(step, PoolStep::PayoutDelivered { .. });
    let now = clock();
    if crate::storage::pool::record_at(step, now).is_err() {
        // the payout moved during the read (the queue read it too): what it is now
        return match crate::storage::pool::payout(id).map(|payout| payout.status) {
            Some(PayoutStatus::Paid) => Ok(AcrossFillCheck::Paid),
            Some(PayoutStatus::Delivering) => Err(PoolError::Unavailable(
                "the read's line was refused: ask again".to_string(),
            )),
            _ => Ok(AcrossFillCheck::NotDelivering),
        };
    }
    Ok(match paid {
        true => AcrossFillCheck::Paid,
        false => AcrossFillCheck::NotYet {
            again_at_s: now.as_secs().get().saturating_add(wait_s),
        },
    })
}

/// `check_across_fill`'s body: the watcher or a controller, refused while halted.
pub async fn door(id: u64) -> Result<AcrossFillCheck, PoolError> {
    crate::pool::require_running()?;
    crate::guards::require_watcher_or_controller().map_err(crate::pool::guard_error)?;
    check(&crate::pool::queue::LiveReads, id, crate::pool::now).await
}
