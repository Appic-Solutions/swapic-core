//! The pool's fold: the ledger's lines, what a step must meet ([`PoolLedger::admits`]), and
//! what it changes ([`PoolLedger::apply`]), on the ledger and on the three maps the storage
//! layer keeps beside it (deposits by ref, shares by account, credited transfers).
//!
//! `admits` reads the fold only: no clock, no config, no sanctions, no chain. The replay
//! audit runs it again over every old line, so a rule that read anything else would refuse
//! a line the log already holds. What depends on the clock or on a read is the endpoint's,
//! before it appends. And a step recorded after money moved (`Paid`, `StakedIcp`,
//! `WithdrawnIcp`) is refused only when it names nothing the fold holds: never for what the
//! money did.

#[cfg(test)]
mod tests;

mod payouts;
mod rebalance;

use super::{
    Arrival, DepositKind, DepositResolution, DepositStatus, IcpCheck, PayoutPath, PayoutProgress,
    PayoutQueue, PendingOp, Pockets, PoolAccount, PoolSettings, PoolSettingsError, PoolStep,
    Registered, Registration, ShareLine, Shares, StakeResolution, UnseenArrival,
};
use crate::chain::ChainId;
use crate::hash::{OwnerKey, PoolOpId, PoolRef};
use crate::numeric::{BlockNumber, Timestamp, TokenAmount};
use crate::pool::payout::{EvmPayout, NonceUse, PayoutLines, PayoutSettingsError};
use crate::pool::CreditKey;
use candid::Principal;
use minicbor::{Decode, Encode};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// How many failed drives a payout gets before it stops for a human.
pub const MAX_PAYOUT_ATTEMPTS: u32 = 10;

/// What the fold holds besides the ledger, as `admits` and `apply` read it.
pub trait PoolLookup {
    fn deposit(&self, r#ref: &PoolRef) -> Option<Registered>;
    fn shares_of(&self, owner: &OwnerKey) -> Shares;
    fn is_credited(&self, key: &CreditKey) -> bool;
    /// A payout from the Base pocket, by id.
    fn payout(&self, id: u64) -> Option<EvmPayout>;
    /// The Base queue's head: its place and the payout there.
    fn queue_head(&self) -> Option<(u64, u64)>;
    /// Whether a staker used a withdrawal nonce.
    fn nonce_used(&self, key: &NonceUse) -> bool;
    /// An orphan, by its credit key.
    fn orphan(&self, key: &CreditKey) -> Option<crate::pool::Orphan>;
    /// A rebalance move, by id (P11c): none in a fold that keeps no moves.
    fn pool_move(&self, _id: u64) -> Option<crate::pool::Move> {
        None
    }
    /// Whether the intent the order `order_id` published was rescued already (P11e): none in
    /// a fold that keeps no rescues.
    fn eco_rescued(&self, _order_id: &[u8; 32]) -> bool {
        false
    }
}

/// A paid payout's Eco intent and the payout: what an Eco refund is matched by.
pub type IntentIndex = ([u8; 32], u64);

/// The pool's own lines: settings, pockets, shares outstanding, and the indexes the timers
/// and the guards read instead of a scan of every deposit ever made.
#[derive(Clone, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct PoolLedger {
    #[n(0)]
    pub settings: Option<PoolSettings>,
    #[n(1)]
    pub pockets: Pockets,
    #[n(2)]
    pub total_shares: Shares,
    /// What arrived or was burned and is not paid or minted yet: in the pockets, and not
    /// the stakers'. The share price leaves it out: a swap's payout and the fees its path
    /// can spend, a withdrawal's amount, a stake's arrival until its mint.
    #[n(3)]
    pub owed: TokenAmount,
    /// The TVL room held by an ICP stake whose pull is out, until it lands or is refused. A
    /// registration holds none (Jeff, 2026-10-21).
    #[n(4)]
    pub stake_room: TokenAmount,
    /// The refs registered and not arrived, expired or unattributed. A registration holds
    /// nothing of the pockets (Jeff, 2026-10-21): the money is held once it is real.
    #[n(5)]
    pub open: BTreeSet<PoolRef>,
    /// The refs arrived and not paid or minted, a payout stopped for a human included.
    #[n(6)]
    pub paying: BTreeSet<PoolRef>,
    /// Open registrations per party (payer and owner).
    #[n(7)]
    pub open_by_party: BTreeMap<OwnerKey, u32>,
    /// ICP stakes and withdrawals in progress.
    #[n(8)]
    pub pending: BTreeMap<PoolOpId, PendingOp>,
    /// Arrivals from blocks no balance check of their chain has read at yet.
    #[n(9)]
    pub unseen: Vec<UnseenArrival>,
    /// Transfers to the canister's Base address no deposit could take: not the stakers',
    /// held for the orphan refund (P11b).
    #[n(10)]
    pub unattributed: TokenAmount,
    /// When the ICP pocket's balance check found a shortfall: latched until a controller
    /// reopens the pool.
    #[n(11)]
    pub icp_shortfall_at: Option<Timestamp>,
    #[n(12)]
    pub icp_check: Option<IcpCheck>,
    #[n(13)]
    pub icp_checked_at: Option<Timestamp>,
    #[n(14)]
    pub icp_failed_at: Option<Timestamp>,
    /// How far the canister's own read of Base got: done through this block, all of it, or
    /// with `scan_log` through that log of it.
    #[n(15)]
    pub scan_cursor: Option<BlockNumber>,
    /// The arrived swaps whose payouts wait for the ICP pocket's free ckUSDC, first in first
    /// out: credited and owed, and held only when they start.
    #[n(16)]
    pub queue: PayoutQueue,
    /// When the queue's head became its head: how long it has waited, which P11c's urgent
    /// rebalance and the alert read.
    #[n(17)]
    pub head_since: Option<Timestamp>,
    /// The log of `scan_cursor`'s block the read is done through, the rest of the block to
    /// read yet. None: the block is done whole.
    #[n(18)]
    pub scan_log: Option<u64>,
    /// The Base payouts' lines (P11b): absent until their first step.
    #[n(19)]
    pub payouts: Option<PayoutLines>,
    /// A Base payout one step changed through its legs, on its way to the step's effects:
    /// never stored (always none between steps).
    #[cbor(skip)]
    pub evm_changed: Option<EvmPayout>,
    /// The rebalancing's lines (P11c): absent until its first step. At 40, clear of the
    /// indices other plans add.
    #[n(40)]
    pub rebalance: Option<crate::pool::RebalanceLines>,
    /// The ckUSDC ledger blocks controllers resolved lost ICP stake pulls with
    /// (`resolve_stake`, the review, 2026-10-24): each resolves one stake. Absent until the
    /// first.
    #[n(41)]
    pub resolved_pulls: Option<BTreeSet<u64>>,
}

/// What one step changes: the ledger, and the entries of the maps beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolEffects {
    pub ledger: PoolLedger,
    pub deposit: Option<(PoolRef, Registered)>,
    pub shares: Option<(OwnerKey, ShareLine)>,
    pub credited: Option<CreditKey>,
    /// A Base payout as the step leaves it.
    pub payout: Option<EvmPayout>,
    /// A place in the Base queue taken: the place and the payout.
    pub queued: Option<(u64, u64)>,
    /// A place in the Base queue given up.
    pub dequeued: Option<u64>,
    /// A withdrawal nonce spent.
    pub nonce: Option<NonceUse>,
    /// A paid payout's Eco intent, indexed.
    pub intent: Option<IntentIndex>,
    /// An orphan as the step leaves it.
    pub orphan: Option<(CreditKey, crate::pool::Orphan)>,
    /// A rebalance move as the step leaves it.
    pub moved: Option<crate::pool::Move>,
}

/// Why a pool step was refused.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PoolStepError {
    #[error("the pool has no settings")]
    NoSettings,
    /// A payout's transfer asked on other terms while an earlier ask may have landed (the
    /// review, 2026-10-24): the drive counts it, so the payout reaches a human.
    #[error("a transfer asked on other terms may have landed")]
    AskedOnOtherTerms,
    #[error(transparent)]
    Settings(#[from] PoolSettingsError),
    #[error("the ckUSDC ledger cannot change while the pool holds anything")]
    CkusdcLedgerFixed,
    #[error("the ref is not the one the registration's terms commit to")]
    BadRef,
    #[error("the ref {0} is registered already")]
    Known(PoolRef),
    #[error("no deposit is registered under {0}")]
    Unknown(PoolRef),
    #[error("{step} is not a step from {status}")]
    NotFrom { step: &'static str, status: String },
    #[error("the transfer {0:?} was credited already")]
    AlreadyCredited(CreditKey),
    #[error("{0} must be above zero")]
    Zero(&'static str),
    #[error("{0}")]
    Terms(&'static str),
    #[error("a pool line overflowed")]
    Overflow,
    #[error("the arrival of {amount} is below the {min_usdc} registered")]
    BelowMinimum {
        amount: TokenAmount,
        min_usdc: TokenAmount,
    },
    #[error("the ICP pocket holds {free} free, and {need} is needed")]
    PocketShort {
        free: TokenAmount,
        need: TokenAmount,
    },
    #[error("the account holds {have} shares, and {need} are needed")]
    ShortShares { have: Shares, need: Shares },
    #[error("{burned} shares are fewer than the {least} that {usdc} is worth")]
    BurnTooSmall {
        burned: Shares,
        least: Shares,
        usdc: TokenAmount,
    },
    #[error("the mint is {given} shares, and the pool's price makes it {expected:?}")]
    WrongMint {
        given: Shares,
        expected: Option<Shares>,
    },
    #[error("the operation {0} is in progress already")]
    Pending(PoolOpId),
    #[error("no operation {0} is in progress")]
    NotPending(PoolOpId),
    #[error("the step does not match the operation it names")]
    Mismatch,
    #[error("the cursor at {at} cannot move back to {to}")]
    CursorBack { at: BlockNumber, to: BlockNumber },
    #[error("the arrival does not fit the pool: {0}")]
    DoesNotFit(&'static str),
    #[error(transparent)]
    PayoutSettings(#[from] PayoutSettingsError),
    #[error("the payouts have no settings")]
    NoPayoutSettings,
    #[error("no payout {0}")]
    UnknownPayout(u64),
    #[error("{step} is not a step of payout {id} from {status}")]
    PayoutNotFrom {
        step: &'static str,
        id: u64,
        status: String,
    },
    #[error("the payout's id must be {expected}")]
    PayoutId { expected: u64 },
    #[error("the Base pocket holds {free} free behind the queue, and {need} is needed")]
    BaseShort {
        free: TokenAmount,
        need: TokenAmount,
    },
    #[error("the withdrawal nonce {0} of the staker is used")]
    NonceUsed(u64),
    #[error(transparent)]
    RebalanceSettings(#[from] crate::pool::RebalanceSettingsError),
    #[error("the rebalancing has no settings")]
    NoRebalanceSettings,
    #[error("no move {0}")]
    UnknownMove(u64),
    #[error("{step} is not a step of move {id} from {status}")]
    MoveNotFrom {
        step: &'static str,
        id: u64,
        status: String,
    },
    #[error("the move's id must be {expected}")]
    MoveId { expected: u64 },
}

/// Why `register_pool_deposit` refuses terms, before anything is appended. Each is one of
/// the interface's `PoolError`s.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RegisterRefusal {
    #[error("the pool has no settings")]
    NoSettings,
    #[error("the pool is not open for swaps")]
    ClosedForSwaps,
    #[error("the ICP pocket is short")]
    PocketShort,
    #[error("the ref does not match its terms")]
    BadRef,
    #[error("the ref is registered already")]
    Known,
    #[error("{0}")]
    NotAllowed(String),
    /// No longer answered (2026-10-23): the canister enforces no per swap cap, the quoter's.
    #[error("above the per-swap cap of {cap}")]
    AboveSwapCap { cap: TokenAmount },
    #[error("the pool would hold {would_be}, above its cap of {cap}")]
    AboveTvlCap {
        cap: TokenAmount,
        would_be: TokenAmount,
    },
    #[error("the premium must be at least {least}")]
    PremiumTooLow { least: TokenAmount },
    #[error("{0} is not a token the pool pays out")]
    TokenNotListed(Principal),
}

fn sat_add(a: TokenAmount, b: TokenAmount) -> TokenAmount {
    a.checked_add(b).unwrap_or(TokenAmount::MAX)
}

fn sat_sub(a: TokenAmount, b: TokenAmount) -> TokenAmount {
    a.checked_sub(b).unwrap_or(TokenAmount::ZERO)
}

fn status_name(status: &DepositStatus) -> String {
    format!("{status:?}")
}

/// What a step that names a payout (a deposit's, a withdrawal's, or a Base payout's ICP
/// legs) names.
enum Target {
    Deposit(Box<Registered>),
    Withdraw(Box<PendingOp>),
    Evm(Box<EvmPayout>),
}

impl Target {
    fn payout(&self) -> Option<&PayoutProgress> {
        match self {
            Target::Deposit(deposit) => Some(&deposit.payout),
            Target::Withdraw(op) => match op.as_ref() {
                PendingOp::Withdraw { payout, .. } => Some(payout),
                PendingOp::Stake { .. } => None,
            },
            Target::Evm(payout) => payout.legs.as_ref(),
        }
    }

    /// In a payout a drive works on: a deposit arrived or paying, or a withdrawal, not
    /// stopped for a human.
    fn in_payout(&self) -> bool {
        match self {
            Target::Deposit(deposit) => {
                matches!(
                    deposit.status,
                    DepositStatus::Arrived | DepositStatus::Paying
                )
            }
            Target::Withdraw(op) => match op.as_ref() {
                PendingOp::Withdraw { payout, .. } => payout.failed.is_none(),
                PendingOp::Stake { .. } => false,
            },
            // an ICP swap's legs run once its pull landed, until its amount is known
            Target::Evm(payout) => payout.status == crate::pool::PayoutStatus::Swapping,
        }
    }
}

impl PoolLedger {
    pub fn settings(&self) -> Result<&PoolSettings, PoolStepError> {
        self.settings.as_ref().ok_or(PoolStepError::NoSettings)
    }

    /// The stakers' assets, the share price's `A`: every pocket line, less what the pool
    /// owes and has not paid. Read off the ledger only, so a donation moves nothing.
    pub fn total_assets(&self) -> TokenAmount {
        sat_sub(self.pockets.assets(), self.owed)
    }

    /// What the TVL cap is measured on: every pocket line, and the room stakes on their way
    /// hold.
    pub fn tvl_measure(&self) -> TokenAmount {
        sat_add(self.pockets.assets(), self.stake_room)
    }

    /// What the canister's address on the pool chain should hold for the pool: the Base
    /// pocket's held lines and the unattributed transfers.
    pub fn base_held(&self) -> TokenAmount {
        sat_add(
            sat_add(
                sat_add(self.pockets.base.free, self.pockets.base.reserved),
                self.unattributed,
            ),
            self.lines().fees_base,
        )
    }

    /// What the canister's ckUSDC account should hold for the pool: the ICP pocket's free and
    /// reserved lines, and the ICP swaps' network fees.
    pub fn icp_held(&self) -> TokenAmount {
        sat_add(
            sat_add(self.pockets.icp.free, self.pockets.icp.reserved),
            self.lines().fees_icp,
        )
    }

    /// The Base payouts' lines, empty before their first step.
    pub fn lines(&self) -> PayoutLines {
        self.payouts.clone().unwrap_or_default()
    }

    /// The arrivals on `chain` from blocks above `block`: in the lines, and not yet in a
    /// balance read at `block`.
    pub fn arrivals_above(&self, chain: ChainId, block: BlockNumber) -> TokenAmount {
        self.unseen
            .iter()
            .filter(|arrival| arrival.chain == chain && arrival.block > block)
            .fold(TokenAmount::ZERO, |sum, arrival| {
                sat_add(sum, arrival.amount)
            })
    }

    /// A balance check of `chain` read at `block`: every arrival at or below it is in that
    /// balance now.
    pub fn seen_through(mut self, chain: ChainId, block: BlockNumber) -> Self {
        self.unseen
            .retain(|arrival| arrival.chain != chain || arrival.block > block);
        self
    }

    /// A controller reopened the pool: the ICP latch is cleared with the Base one.
    pub fn reopened(mut self) -> Self {
        self.icp_shortfall_at = None;
        self
    }

    /// The open registrations `party` holds.
    pub fn open_of(&self, party: &OwnerKey) -> u32 {
        self.open_by_party.get(party).copied().unwrap_or(0)
    }

    fn target(&self, r#ref: &PoolRef, lookup: &impl PoolLookup) -> Option<Target> {
        if let Some(deposit) = lookup.deposit(r#ref) {
            return Some(Target::Deposit(Box::new(deposit)));
        }
        if let Some(id) = crate::pool::evm_payout_id(r#ref) {
            return lookup
                .payout(id)
                .map(|payout| Target::Evm(Box::new(payout)));
        }
        let id = PoolOpId::new(r#ref.into_bytes());
        self.pending
            .get(&id)
            .map(|op| Target::Withdraw(Box::new(op.clone())))
    }

    fn deposit(
        &self,
        r#ref: &PoolRef,
        lookup: &impl PoolLookup,
    ) -> Result<Registered, PoolStepError> {
        lookup.deposit(r#ref).ok_or(PoolStepError::Unknown(*r#ref))
    }

    /// Whether `amount` arriving for `deposit` fits the pool, measured on what arrived: a
    /// stake within the TVL cap; a swap has no cap on the canister (2026-10-23: the per-swap
    /// cap is the quoter's own). Answers what a swap's payout
    /// holds once it starts (P11d: the user pays every fee): everything that arrived but the
    /// premium, its payout and its own ICP fees, out of which its path's ledger fees are
    /// paid. A payout the ICP pocket cannot cover yet is queued, never refused.
    pub fn arrival_fits(
        &self,
        deposit: &Registered,
        amount: TokenAmount,
    ) -> Result<TokenAmount, &'static str> {
        let settings = self.settings.as_ref().ok_or("the pool has no settings")?;
        match deposit.terms.kind {
            DepositKind::Swap { .. } => {
                let budget = amount
                    .checked_sub(deposit.terms.premium)
                    .ok_or("below the premium")?;
                budget
                    .checked_sub(deposit.terms.icp_fees())
                    .filter(|payout| *payout > TokenAmount::ZERO)
                    .ok_or("below the premium and the ICP fees")?;
                Ok(budget)
            }
            DepositKind::Stake { .. } => {
                let would_be = sat_add(self.tvl_measure(), amount);
                if would_be > settings.tvl_cap {
                    return Err("above the TVL cap");
                }
                Ok(TokenAmount::ZERO)
            }
        }
    }

    /// Where the canister's read of Base stands, in order: a block, then its log (a block
    /// done whole above every log of it).
    pub fn scan_position(&self) -> Option<(BlockNumber, u128)> {
        self.scan_cursor
            .map(|block| (block, self.scan_log.map_or(u128::MAX, u128::from)))
    }

    /// The ICP pocket's free ckUSDC less what the queued payouts will take: what a
    /// registration may count on and a withdrawal may take, behind the queue.
    pub fn icp_available(&self) -> TokenAmount {
        sat_sub(self.pockets.icp.free, self.queue.total())
    }

    /// What the queue's head still needs of the ICP pocket: what it holds, less the free
    /// ckUSDC. Nothing when the queue is empty or its head is covered. What P11c's urgent
    /// rebalance moves to ICP.
    pub fn queue_need(&self) -> Option<TokenAmount> {
        let (_, held) = self.queue.head()?;
        held.checked_sub(self.pockets.icp.free)
            .filter(|need| *need > TokenAmount::ZERO)
    }

    /// What a stake of `amount` mints now.
    pub fn mint_now(&self, amount: TokenAmount) -> Option<Shares> {
        Shares::mint_for(amount, self.total_assets(), self.total_shares)
    }

    /// The rules that read the line's own time `at`, beside [`PoolLedger::admits`] (the
    /// review, 2026-10-24), whoever wrote the line: a retry's fresh dedup instant is exactly
    /// the line's time; a retry with its own pair (its transfer may have landed) leaves its
    /// next drive inside the ledger's window less the margin. A replay reads these too, of
    /// every line in the log: the lines of builds never deployed are not grandfathered
    /// (hfisl's log holds no Pool event).
    pub fn admits_at(
        &self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
        at: Timestamp,
    ) -> Result<(), PoolStepError> {
        let PoolStep::DepositResolved {
            r#ref,
            resolution: DepositResolution::Retry,
            retry_at,
        } = step
        else {
            return Ok(());
        };
        match retry_at {
            Some(retry_at) if *retry_at != at => Err(PoolStepError::Terms(
                "a retry's fresh dedup instant is its line's own time",
            )),
            Some(_) => Ok(()),
            None => {
                let Some(deposit) = lookup.deposit(r#ref) else {
                    return Ok(());
                };
                match deposit.payout.attempt {
                    Some(attempt)
                        if !deposit.payout.cannot_land_twice()
                            && !super::deduplicated(attempt.created_at, at) =>
                    {
                        Err(PoolStepError::Terms(
                            "a retry with its own dedup pair must leave its next drive inside \
                             the ledger's window",
                        ))
                    }
                    _ => Ok(()),
                }
            }
        }
    }

    /// Every rule a step must meet, read off the fold alone.
    pub fn admits(&self, step: &PoolStep, lookup: &impl PoolLookup) -> Result<(), PoolStepError> {
        let not_from = |step: &'static str, status: &DepositStatus| PoolStepError::NotFrom {
            step,
            status: status_name(status),
        };
        match step {
            PoolStep::SettingsSet { settings } => {
                settings.validate()?;
                settings.fees_complete()?;
                if let Some(current) = &self.settings {
                    let holds_any = self.pockets.icp.total() != TokenAmount::ZERO
                        || self.total_shares != Shares::ZERO
                        || !self.pending.is_empty()
                        || !self.open.is_empty()
                        || !self.paying.is_empty()
                        || !self.lines().open.is_empty();
                    if current.ckusdc_ledger != settings.ckusdc_ledger && holds_any {
                        return Err(PoolStepError::CkusdcLedgerFixed);
                    }
                }
                Ok(())
            }
            PoolStep::Registered { r#ref, deposit } => {
                self.settings()?;
                if deposit.pool_ref() != Some(*r#ref) {
                    return Err(PoolStepError::BadRef);
                }
                if lookup.deposit(r#ref).is_some() {
                    return Err(PoolStepError::Known(*r#ref));
                }
                admits_terms(deposit)
            }
            PoolStep::Expired { r#ref } => {
                let deposit = self.deposit(r#ref, lookup)?;
                if deposit.status != DepositStatus::Registered {
                    return Err(not_from("Expired", &deposit.status));
                }
                Ok(())
            }
            PoolStep::Arrived {
                r#ref,
                credit,
                amount,
                ..
            } => {
                let deposit = self.deposit(r#ref, lookup)?;
                if deposit.status != DepositStatus::Registered {
                    return Err(not_from("Arrived", &deposit.status));
                }
                // a transfer that awaited its terms was credited to nothing until claimed
                let claimed = lookup
                    .orphan(credit)
                    .is_some_and(|orphan| orphan.status == crate::pool::OrphanStatus::Claimed);
                if lookup.is_credited(credit) && !claimed {
                    return Err(PoolStepError::AlreadyCredited(*credit));
                }
                if *amount < deposit.terms.min_usdc {
                    return Err(PoolStepError::BelowMinimum {
                        amount: *amount,
                        min_usdc: deposit.terms.min_usdc,
                    });
                }
                self.arrival_fits(&deposit, *amount)
                    .map_err(PoolStepError::DoesNotFit)?;
                Ok(())
            }
            PoolStep::Unattributed { credit, amount, .. } => {
                if lookup.is_credited(credit) {
                    return Err(PoolStepError::AlreadyCredited(*credit));
                }
                if *amount == TokenAmount::ZERO {
                    return Err(PoolStepError::Zero("amount"));
                }
                Ok(())
            }
            // a payout starts first in first out, and only once the pocket's free ckUSDC
            // covers all it holds
            PoolStep::PayoutStarted { r#ref, .. } => {
                let deposit = self.deposit(r#ref, lookup)?;
                if deposit.status != DepositStatus::Arrived || !deposit.terms.kind.is_swap() {
                    return Err(not_from("PayoutStarted", &deposit.status));
                }
                if self.queue.head().map(|(head, _)| head) != Some(*r#ref) {
                    return Err(PoolStepError::Terms("a payout starts first in first out"));
                }
                if deposit.held() > self.pockets.icp.free {
                    return Err(PoolStepError::PocketShort {
                        free: self.pockets.icp.free,
                        need: deposit.held(),
                    });
                }
                Ok(())
            }
            PoolStep::Paid { r#ref, .. } => {
                let deposit = self.deposit(r#ref, lookup)?;
                if deposit.status != DepositStatus::Paying {
                    return Err(not_from("Paid", &deposit.status));
                }
                Ok(())
            }
            PoolStep::FellBack { r#ref, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                let payout = target.payout().ok_or(PoolStepError::Mismatch)?;
                let path = payout.attempt.map(|attempt| attempt.path);
                if !target.in_payout() || !path.is_some_and(|path| path.swaps()) {
                    return Err(PoolStepError::Terms(
                        "only an ICPSwap leg with no output falls back",
                    ));
                }
                // a path through ICP whose first leg paid out falls back once the way back
                // brought the ICP back as ckUSDC, and never once its second leg paid out
                if payout.swap.out.is_some() {
                    let through_icp = matches!(path, Some(PayoutPath::IcpswapVia { .. }));
                    let second = payout.leg(Some(super::LEG_SECOND));
                    let back = payout.leg(Some(super::LEG_BACK));
                    if !through_icp || second.out.is_some() || back.out.is_none() {
                        return Err(PoolStepError::Terms(
                            "only an ICPSwap leg with no output falls back",
                        ));
                    }
                    return Ok(());
                }
                // a leg falls back before its swap, or after one that did not happen and was
                // undone (the driver read the pool to know): never once the swap paid out
                if payout.swap.begun && payout.swap.undone.is_none() {
                    return Err(PoolStepError::Terms("a begun swap falls back once undone"));
                }
                Ok(())
            }
            PoolStep::Minted {
                r#ref,
                owner,
                shares,
            } => {
                let deposit = self.deposit(r#ref, lookup)?;
                let DepositKind::Stake { owner: staker } = deposit.terms.kind else {
                    return Err(not_from("Minted", &deposit.status));
                };
                if deposit.status != DepositStatus::Arrived {
                    return Err(not_from("Minted", &deposit.status));
                }
                if staker != *owner {
                    return Err(PoolStepError::Mismatch);
                }
                let expected =
                    self.mint_now(sat_sub(deposit.owed(), deposit.terms.rebalance_share));
                if expected != Some(*shares) {
                    return Err(PoolStepError::WrongMint {
                        given: *shares,
                        expected,
                    });
                }
                self.total_shares
                    .checked_add(*shares)
                    .ok_or(PoolStepError::Overflow)?;
                Ok(())
            }
            PoolStep::StakedIcp {
                id, owner, amount, ..
            } => match self.pending.get(id) {
                Some(PendingOp::Stake {
                    owner: started,
                    amount: pulled,
                    ..
                }) if started == owner && pulled == amount => Ok(()),
                Some(_) => Err(PoolStepError::Mismatch),
                None => Err(PoolStepError::NotPending(*id)),
            },
            PoolStep::WithdrawnIcp { id, owner, .. } => match self.pending.get(id) {
                Some(PendingOp::Withdraw { owner: started, .. }) if started == owner => Ok(()),
                Some(_) => Err(PoolStepError::Mismatch),
                None => Err(PoolStepError::NotPending(*id)),
            },
            // a payout's transfer asked: never on other terms while one may have landed
            PoolStep::TransferAsked {
                r#ref,
                token,
                amount,
                ..
            } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                if !target.in_payout() {
                    return Err(PoolStepError::Terms("only a payout in progress asks"));
                }
                // may have landed: not refused at its last ask, or an answer of it once lost
                match target.payout() {
                    Some(payout)
                        if !payout.cannot_land_twice()
                            && payout.sent.is_some_and(|sent| {
                                (sent.token, sent.amount) != (*token, *amount)
                            }) =>
                    {
                        Err(PoolStepError::AskedOnOtherTerms)
                    }
                    _ => Ok(()),
                }
            }
            PoolStep::TransferLost { r#ref, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                if !target.in_payout() {
                    return Err(PoolStepError::Terms(
                        "only a payout in progress loses an answer",
                    ));
                }
                match target.payout().and_then(|payout| payout.sent) {
                    Some(_) => Ok(()),
                    None => Err(PoolStepError::Terms(
                        "only a transfer asked loses an answer",
                    )),
                }
            }
            PoolStep::TransferRefused { r#ref } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                match target.payout().and_then(|payout| payout.sent) {
                    Some(sent) if !sent.refused => Ok(()),
                    _ => Err(PoolStepError::Terms("only a transfer asked is refused")),
                }
            }
            PoolStep::AttemptFailed { r#ref, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                // a queued payout whose drive fails before it starts is counted too, and held
                // for a human at the usual attempts: it never blocks the queue (CR-08)
                if self.queue.0.iter().any(|(queued, _)| queued == r#ref) {
                    return Ok(());
                }
                if !target.in_payout() {
                    return Err(PoolStepError::Terms("only a payout in progress fails"));
                }
                Ok(())
            }
            PoolStep::SwapDeclined { r#ref, leg, .. } | PoolStep::SwapBegun { r#ref, leg, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                let payout = target.payout().ok_or(PoolStepError::Mismatch)?;
                let swaps = admits_leg(payout, *leg);
                let this = payout.leg(*leg);
                if !target.in_payout() || !swaps || this.begun {
                    return Err(PoolStepError::Terms(
                        "the ICPSwap leg is not before its call",
                    ));
                }
                if matches!(step, PoolStep::SwapDeclined { .. }) && this.declined > 0 {
                    return Err(PoolStepError::Terms(
                        "a declined leg falls back the second time",
                    ));
                }
                Ok(())
            }
            PoolStep::Swapped { r#ref, leg, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                let payout = target.payout().ok_or(PoolStepError::Mismatch)?;
                let this = payout.leg(*leg);
                if !this.begun || this.out.is_some() {
                    return Err(PoolStepError::Terms("only a begun swap answers once"));
                }
                Ok(())
            }
            PoolStep::StakeStarted { id, amount, .. } => {
                self.settings()?;
                if self.pending.contains_key(id) {
                    return Err(PoolStepError::Pending(*id));
                }
                if *amount == TokenAmount::ZERO {
                    return Err(PoolStepError::Zero("amount"));
                }
                Ok(())
            }
            PoolStep::StakeFailed { id, .. } => match self.pending.get(id) {
                Some(PendingOp::Stake { .. }) => Ok(()),
                Some(_) => Err(PoolStepError::Mismatch),
                None => Err(PoolStepError::NotPending(*id)),
            },
            // an ICP stake whose pull's answer was lost, resolved once by a controller (its
            // door holds it to the ledger's deduplication window)
            PoolStep::StakeResolved { id, resolution } => match self.pending.get(id) {
                Some(PendingOp::Stake { .. }) => match resolution {
                    StakeResolution::Pulled { shares, .. } if *shares == Shares::ZERO => {
                        Err(PoolStepError::Zero("shares"))
                    }
                    // one ledger block resolves one stake
                    StakeResolution::Pulled { block, .. }
                        if self
                            .resolved_pulls
                            .as_ref()
                            .is_some_and(|blocks| blocks.contains(block)) =>
                    {
                        Err(PoolStepError::Terms("a ledger block resolves one stake"))
                    }
                    _ => Ok(()),
                },
                Some(_) => Err(PoolStepError::Mismatch),
                None => Err(PoolStepError::NotPending(*id)),
            },
            // a deposit held for a human, resolved once by a controller
            PoolStep::DepositResolved {
                r#ref,
                resolution,
                retry_at,
            } => {
                let deposit = self.deposit(r#ref, lookup)?;
                if !matches!(deposit.status, DepositStatus::PayoutFailed(_)) {
                    return Err(not_from("DepositResolved", &deposit.status));
                }
                // only a retry of a started payout carries a dedup instant
                if retry_at.is_some() && resolution != &DepositResolution::Retry {
                    return Err(PoolStepError::Terms("only a retry carries a dedup instant"));
                }
                let started = deposit.payout.attempt.is_some();
                match resolution {
                    // keyed on its transfer (the review, 2026-10-24): one that may have
                    // landed is retried only with its own dedup pair, on the ckUSDC ledger
                    // the payout started with, which deduplicates it (the door holds it to
                    // the window); one that cannot land twice, whatever it pays, afresh
                    DepositResolution::Retry => match (deposit.payout.attempt, deposit.payout.sent)
                    {
                        (None, _) if retry_at.is_some() => Err(PoolStepError::Terms(
                            "a payout that never started has no dedup instant",
                        )),
                        (None, _) => Ok(()),
                        // it may have landed (asked and not refused, or an answer of it lost):
                        // only its own pair, on the ckUSDC ledger it started with
                        (Some(attempt), Some(sent)) if !deposit.payout.cannot_land_twice() => {
                            let started_on = match attempt.token {
                                Some(token) => token.0,
                                None => self.settings()?.ckusdc_ledger,
                            };
                            if retry_at.is_some() {
                                Err(PoolStepError::Terms(
                                    "a transfer that may have landed is retried with its own \
                                     dedup pair",
                                ))
                            } else if sent.token.0 != started_on {
                                Err(PoolStepError::Terms(
                                    "a transfer that may have landed on another token's ledger \
                                     is never retried",
                                ))
                            } else {
                                Ok(())
                            }
                        }
                        (Some(_), _) => Ok(()),
                    },
                    // its transfer as asked, which the door read in the block named
                    DepositResolution::MarkPaid { .. } => {
                        if !started || deposit.payout.sent.is_none() {
                            return Err(PoolStepError::Terms(
                                "only a payout that asked its transfer is marked paid",
                            ));
                        }
                        Ok(())
                    }
                    DepositResolution::Refund => {
                        // only when the canister knows nothing went out: a transfer that may
                        // have landed, or a leg that moved money, could pay twice
                        if !deposit.payout.nothing_went_out() {
                            return Err(PoolStepError::Terms(
                                "its payout may have sent something: it is not refunded on Base",
                            ));
                        }
                        if started && deposit.held() > self.pockets.icp.reserved {
                            return Err(PoolStepError::Overflow);
                        }
                        let (credit, orphan) = deposit
                            .refund_orphan()
                            .ok_or(PoolStepError::Terms("a deposit refunds only what arrived"))?;
                        if deposit.terms.src_payer == crate::evm::EvmAddress::ZERO {
                            return Err(PoolStepError::Terms("the deposit has no payer to refund"));
                        }
                        if orphan.amount > self.pockets.base.free {
                            return Err(PoolStepError::Overflow);
                        }
                        if lookup.orphan(&credit).is_some_and(|known| {
                            known.status != crate::pool::OrphanStatus::Credited
                        }) {
                            return Err(PoolStepError::Terms("its transfer is an orphan already"));
                        }
                        Ok(())
                    }
                }
            }
            PoolStep::WithdrawStarted {
                id,
                owner,
                burned,
                usdc,
                ..
            } => {
                self.settings()?;
                if self.pending.contains_key(id) {
                    return Err(PoolStepError::Pending(*id));
                }
                if *usdc == TokenAmount::ZERO {
                    return Err(PoolStepError::Zero("usdc"));
                }
                let have = lookup.shares_of(&PoolAccount::Icp(*owner).key());
                if have < *burned {
                    return Err(PoolStepError::ShortShares {
                        have,
                        need: *burned,
                    });
                }
                let least = Shares::burn_for(*usdc, self.total_assets(), self.total_shares)
                    .ok_or(PoolStepError::Terms("the pool's assets cannot pay that"))?;
                if *burned < least {
                    return Err(PoolStepError::BurnTooSmall {
                        burned: *burned,
                        least,
                        usdc: *usdc,
                    });
                }
                // a withdrawal waits behind the queued payouts
                if *usdc > self.icp_available() {
                    return Err(PoolStepError::PocketShort {
                        free: self.icp_available(),
                        need: *usdc,
                    });
                }
                Ok(())
            }
            PoolStep::SwapApproved { r#ref, leg } | PoolStep::SwapUndone { r#ref, leg, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                let payout = target.payout().ok_or(PoolStepError::Mismatch)?;
                let swaps = admits_leg(payout, *leg);
                let this = payout.leg(*leg);
                if !target.in_payout() || !swaps || this.out.is_some() {
                    return Err(PoolStepError::Terms("the ICPSwap leg has its output"));
                }
                if matches!(step, PoolStep::SwapUndone { .. })
                    && (!this.begun || this.undone.is_some())
                {
                    return Err(PoolStepError::Terms("only a begun swap is undone, once"));
                }
                Ok(())
            }
            PoolStep::IcpChecked { .. } | PoolStep::IcpCheckFailed { .. } => Ok(()),
            // never back: a block done whole is past any log of it
            PoolStep::ScanCursor { block, log } => match self.scan_position() {
                Some(at) if (*block, log.map_or(u128::MAX, u128::from)) < at => {
                    Err(PoolStepError::CursorBack {
                        at: at.0,
                        to: *block,
                    })
                }
                _ => Ok(()),
            },
            PoolStep::PayoutSettingsSet { .. }
            | PoolStep::PayoutOpened { .. }
            | PoolStep::PayoutAmountKnown { .. }
            | PoolStep::PayoutQueued { .. }
            | PoolStep::PayoutWaiting { .. }
            | PoolStep::PayoutSigned { .. }
            | PoolStep::PayoutPaid { .. }
            | PoolStep::PayoutReverted { .. }
            | PoolStep::PayoutUndoing { .. }
            | PoolStep::PayoutUndone { .. }
            | PoolStep::PayoutPulled { .. }
            | PoolStep::PayoutStopped { .. }
            | PoolStep::PayoutRelisted { .. }
            | PoolStep::PayoutRefunded { .. }
            | PoolStep::PayoutDelivered { .. }
            | PoolStep::PayoutUnfilled { .. }
            | PoolStep::PayoutResolved { .. }
            | PoolStep::PayoutFitWaited { .. }
            | PoolStep::PayoutFillEarly { .. }
            | PoolStep::OrphanBooked { .. }
            | PoolStep::OrphanRefundSigned { .. }
            | PoolStep::OrphanRefundClosed { .. }
            | PoolStep::TermsClaimed { .. } => self.admits_payout(step, lookup),
            // the nonce it spends is the state's to check: nothing of the pool moves
            PoolStep::ApproveSigned { .. } => Ok(()),
            PoolStep::MoveSettingsSet { .. }
            | PoolStep::MoveOpened { .. }
            | PoolStep::MoveProposed { .. }
            | PoolStep::MoveRouted { .. }
            | PoolStep::MoveSigned { .. }
            | PoolStep::MoveBurned { .. }
            | PoolStep::MoveReverted { .. }
            | PoolStep::MoveWithdrawing { .. }
            | PoolStep::MoveWithdrawn { .. }
            | PoolStep::MoveRefused { .. }
            | PoolStep::MoveLanded { .. }
            | PoolStep::MoveSweeping { .. }
            | PoolStep::MoveArrived { .. }
            | PoolStep::MoveStuck { .. }
            | PoolStep::MoveRetried { .. }
            | PoolStep::MoveLost { .. }
            | PoolStep::MoveRefunded { .. }
            | PoolStep::MoveUnfilled { .. }
            | PoolStep::MoveReimbursed { .. } => self.admits_rebalance(step, lookup),
            // one rescue per order; the nonce it spends is the state's to check
            PoolStep::EcoRescueSigned { order_id, .. } => {
                if lookup.eco_rescued(order_id) {
                    return Err(PoolStepError::Terms(
                        "the order's intent was rescued already",
                    ));
                }
                Ok(())
            }
            PoolStep::IcpRoutesSet { routes } => {
                let settings = self.settings()?;
                routes
                    .validate(settings.ckusdc_ledger)
                    .map_err(PoolSettingsError::from)?;
                routes
                    .fees_complete(settings.ckusdc_ledger)
                    .map_err(PoolSettingsError::from)?;
                Ok(())
            }
        }
    }

    /// Records a step [`PoolLedger::admits`] admitted, sealed at `at`. Never fails: a line
    /// that would go below zero stops at zero.
    pub fn apply(self, step: &PoolStep, lookup: &impl PoolLookup, at: Timestamp) -> PoolEffects {
        self.apply_at(step, lookup, at, 0)
    }

    /// [`PoolLedger::apply`] of the line at `index` of the log: a step that records a signed
    /// transaction is known by the index of its line.
    pub fn apply_at(
        self,
        step: &PoolStep,
        lookup: &impl PoolLookup,
        at: Timestamp,
        index: u64,
    ) -> PoolEffects {
        if let Some(effects) = self.clone().apply_payout(step, lookup, at, index) {
            return effects;
        }
        if let Some(effects) = self.clone().apply_rebalance(step, lookup, at, index) {
            return effects;
        }
        let mut ledger = self;
        let mut deposit_out = None;
        let mut shares_out = None;
        let mut credited = None;
        let mut orphan_out = None;
        match step {
            // the routes are `IcpRoutesSet`'s: new knobs keep the routes set
            PoolStep::SettingsSet { settings } => {
                let routes = ledger
                    .settings
                    .as_ref()
                    .and_then(|current| current.routes.clone());
                let mut settings = settings.clone();
                if settings.routes.is_none() {
                    settings.routes = routes;
                }
                ledger.settings = Some(settings);
            }
            PoolStep::ApproveSigned { .. } => {}
            PoolStep::IcpRoutesSet { routes } => {
                if let Some(settings) = ledger.settings.as_mut() {
                    settings.routes = Some(routes.clone());
                }
            }
            PoolStep::Registered { r#ref, deposit } => {
                ledger.open.insert(*r#ref);
                for party in deposit.parties() {
                    *ledger.open_by_party.entry(party).or_insert(0) += 1;
                }
                deposit_out = Some((*r#ref, Registered::new(deposit.clone())));
            }
            PoolStep::Expired { r#ref } => {
                if let Some(mut deposit) = lookup.deposit(r#ref) {
                    ledger.close_open(r#ref, &deposit.terms);
                    deposit.status = DepositStatus::Expired;
                    deposit_out = Some((*r#ref, deposit));
                }
            }
            PoolStep::Arrived {
                r#ref,
                credit,
                amount,
                block,
            } => {
                if let Some(mut deposit) = lookup.deposit(r#ref) {
                    ledger.close_open(r#ref, &deposit.terms);
                    if deposit.terms.kind.is_swap() {
                        // the money is real now: the payout and its fees are owed, and queued
                        // first in first out until the pocket's free ckUSDC covers them
                        if let Ok(held) = ledger.arrival_fits(&deposit, *amount) {
                            deposit.held = Some(held);
                            if ledger.queue.0.is_empty() {
                                ledger.head_since = Some(at);
                            }
                            ledger.queue.0.push((*r#ref, held));
                        }
                    }
                    ledger.paying.insert(*r#ref);
                    deposit.status = DepositStatus::Arrived;
                    deposit.arrived = Some(Arrival {
                        credit: *credit,
                        amount: *amount,
                        block: *block,
                    });
                    ledger.owed = sat_add(ledger.owed, owed_line(&deposit));
                    deposit_out = Some((*r#ref, deposit));
                }
                ledger.pockets.base.free = sat_add(ledger.pockets.base.free, *amount);
                credited = Some(*credit);
                match lookup
                    .orphan(credit)
                    .filter(|orphan| orphan.status == crate::pool::OrphanStatus::Claimed)
                {
                    // a transfer that awaited its terms: on the Base balance's books since
                    // its own line, credited once
                    Some(mut orphan) => {
                        orphan.status = crate::pool::OrphanStatus::Credited;
                        orphan_out = Some((*credit, orphan));
                    }
                    None => ledger.unseen.push(UnseenArrival {
                        chain: credit.chain,
                        block: *block,
                        amount: *amount,
                    }),
                }
            }
            PoolStep::Unattributed {
                credit,
                amount,
                block,
                r#ref,
                src_payer,
                why,
                awaiting_until,
            } => {
                // held for a controller, one at a time; a deposit's transfer found without
                // its terms awaits them first
                let status = match (awaiting_until, r#ref) {
                    (Some(until), Some(r#ref)) => crate::pool::OrphanStatus::AwaitingTerms {
                        r#ref: *r#ref,
                        until: *until,
                    },
                    _ => crate::pool::OrphanStatus::Held,
                };
                orphan_out = Some((
                    *credit,
                    crate::pool::Orphan {
                        amount: *amount,
                        block: *block,
                        payer: *src_payer,
                        why: why.clone(),
                        status,
                    },
                ));
                ledger.unattributed = sat_add(ledger.unattributed, *amount);
                ledger.unseen.push(UnseenArrival {
                    chain: credit.chain,
                    block: *block,
                    amount: *amount,
                });
                credited = Some(*credit);
                if let Some(r#ref) = r#ref {
                    if let Some(mut deposit) = lookup.deposit(r#ref) {
                        match deposit.status {
                            DepositStatus::Registered => {
                                ledger.close_open(r#ref, &deposit.terms);
                                deposit.status = DepositStatus::Unattributed;
                                deposit_out = Some((*r#ref, deposit));
                            }
                            DepositStatus::Expired => {
                                deposit.status = DepositStatus::Unattributed;
                                deposit_out = Some((*r#ref, deposit));
                            }
                            _ => {}
                        }
                    }
                }
            }
            // the hold: the head of the queue takes all it holds of the free ckUSDC
            PoolStep::PayoutStarted { r#ref, attempt } => {
                if let Some(mut deposit) = lookup.deposit(r#ref) {
                    ledger.queue.0.retain(|(queued, _)| queued != r#ref);
                    // the next head waits from now
                    ledger.head_since = (!ledger.queue.0.is_empty()).then_some(at);
                    let held = deposit.held();
                    let icp = &mut ledger.pockets.icp;
                    icp.free = sat_sub(icp.free, held);
                    icp.reserved = sat_add(icp.reserved, held);
                    deposit.status = DepositStatus::Paying;
                    deposit.payout.attempt = Some(*attempt);
                    deposit_out = Some((*r#ref, deposit));
                }
            }
            PoolStep::Paid {
                r#ref,
                token,
                amount,
                block,
                spent,
            } => {
                if let Some(mut deposit) = lookup.deposit(r#ref) {
                    ledger.owed = sat_sub(ledger.owed, owed_line(&deposit));
                    ledger.spend_reserved(deposit.held(), *spent);
                    ledger.paying.remove(r#ref);
                    deposit.status = DepositStatus::Paid;
                    deposit.held = Some(TokenAmount::ZERO);
                    deposit.paid = Some(super::Paid {
                        token: *token,
                        amount: *amount,
                        block: *block,
                        spent: *spent,
                    });
                    deposit_out = Some((*r#ref, deposit));
                }
            }
            PoolStep::FellBack {
                r#ref,
                why,
                amount,
                token,
            } => {
                let why = why.clone();
                let (amount, token) = (*amount, *token);
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    if let Some(attempt) = payout.attempt.as_mut() {
                        attempt.path = PayoutPath::Ckusdc;
                        attempt.amount = amount;
                        attempt.token = token;
                    }
                    payout.fell_back = Some(why);
                });
            }
            PoolStep::Minted {
                r#ref,
                owner,
                shares,
            } => {
                if let Some(mut deposit) = lookup.deposit(r#ref) {
                    ledger.owed = sat_sub(ledger.owed, owed_line(&deposit));
                    ledger.paying.remove(r#ref);
                    ledger.total_shares = ledger
                        .total_shares
                        .checked_add(*shares)
                        .unwrap_or(Shares::new(u128::MAX));
                    shares_out = Some(credit_shares(lookup, owner, *shares));
                    deposit.status = DepositStatus::Paid;
                    deposit.minted = Some(*shares);
                    deposit_out = Some((*r#ref, deposit));
                }
            }
            PoolStep::StakedIcp {
                id,
                owner,
                amount,
                shares,
                ..
            } => {
                ledger.pending.remove(id);
                ledger.book_stake(*amount, *shares);
                shares_out = Some(credit_shares(lookup, owner, *shares));
            }
            PoolStep::StakeResolved { id, resolution } => {
                match (ledger.pending.remove(id), resolution) {
                    // the pull landed: booked as `StakedIcp` books it
                    (
                        Some(PendingOp::Stake { owner, amount, .. }),
                        StakeResolution::Pulled { block, shares },
                    ) => {
                        ledger.book_stake(amount, *shares);
                        ledger
                            .resolved_pulls
                            .get_or_insert_with(BTreeSet::new)
                            .insert(*block);
                        shares_out = Some(credit_shares(lookup, &owner, *shares));
                    }
                    // it never landed: dropped as `StakeFailed` drops it
                    (Some(PendingOp::Stake { amount, .. }), StakeResolution::NotPulled) => {
                        ledger.stake_room = sat_sub(ledger.stake_room, amount);
                    }
                    _ => {}
                }
            }
            PoolStep::DepositResolved {
                r#ref,
                resolution,
                retry_at,
            } => {
                if let Some(mut deposit) = lookup.deposit(r#ref) {
                    match resolution {
                        DepositResolution::Retry => {
                            deposit.payout.failures = 0;
                            deposit.payout.failed = None;
                            match deposit.payout.attempt.as_mut() {
                                // paying again: with its own dedup pair, or asked afresh at
                                // the door's instant when nothing can land twice
                                Some(attempt) => {
                                    if let Some(retry_at) = retry_at {
                                        attempt.created_at = *retry_at;
                                    }
                                    deposit.status = DepositStatus::Paying;
                                }
                                // never started (CR-08): queued again, at the back
                                None => {
                                    deposit.status = DepositStatus::Arrived;
                                    if deposit.terms.kind.is_swap() {
                                        if ledger.queue.0.is_empty() {
                                            ledger.head_since = Some(at);
                                        }
                                        ledger.queue.0.push((*r#ref, deposit.held()));
                                    }
                                }
                            }
                        }
                        // its transfer as asked, paid in `block`: as `Paid` books it
                        DepositResolution::MarkPaid { block } => {
                            if let Some(sent) = deposit.payout.sent {
                                ledger.owed = sat_sub(ledger.owed, owed_line(&deposit));
                                ledger.spend_reserved(deposit.held(), sent.spent);
                                ledger.paying.remove(r#ref);
                                deposit.status = DepositStatus::Paid;
                                deposit.held = Some(TokenAmount::ZERO);
                                deposit.paid = Some(super::Paid {
                                    token: sent.token.0,
                                    amount: sent.amount,
                                    block: *block,
                                    spent: sent.spent,
                                });
                            }
                        }
                        DepositResolution::Refund => {
                            ledger.owed = sat_sub(ledger.owed, owed_line(&deposit));
                            ledger.paying.remove(r#ref);
                            // started, nothing went out: its hold is free again on ICP
                            if deposit.payout.attempt.is_some() {
                                let held = deposit.held();
                                let icp = &mut ledger.pockets.icp;
                                icp.reserved = sat_sub(icp.reserved, held);
                                icp.free = sat_add(icp.free, held);
                            }
                            // out of the Base pocket's free line, onto the unattributed one:
                            // the Base balance's books stay where they were
                            if let Some((credit, orphan)) = deposit.refund_orphan() {
                                let base = &mut ledger.pockets.base;
                                base.free = sat_sub(base.free, orphan.amount);
                                ledger.unattributed = sat_add(ledger.unattributed, orphan.amount);
                                orphan_out = Some((credit, orphan));
                            }
                            deposit.held = Some(TokenAmount::ZERO);
                            deposit.status = DepositStatus::Refunded;
                        }
                    }
                    deposit_out = Some((*r#ref, deposit));
                }
            }
            PoolStep::WithdrawnIcp { id, spent, .. } => {
                if let Some(PendingOp::Withdraw { usdc, .. }) = ledger.pending.remove(id) {
                    ledger.owed = sat_sub(ledger.owed, usdc);
                    ledger.spend_reserved(usdc, *spent);
                }
            }
            PoolStep::TransferAsked {
                r#ref,
                token,
                amount,
                spent,
            } => {
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    // an answer lost once is kept for good
                    let lost = payout.sent.map_or(Default::default(), |sent| sent.lost);
                    payout.sent = Some(super::Sent {
                        token: *token,
                        amount: *amount,
                        spent: *spent,
                        refused: false,
                        lost,
                    });
                });
            }
            PoolStep::TransferRefused { r#ref } => {
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    if let Some(sent) = payout.sent.as_mut() {
                        sent.refused = true;
                    }
                });
            }
            PoolStep::TransferLost { r#ref, held } => {
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    if let Some(sent) = payout.sent.as_mut() {
                        sent.lost = crate::pool::payout::Flag(true);
                    }
                    // held for a human at once, as its tenth failure would
                    if let Some(why) = held {
                        payout.failed = Some(why.clone());
                    }
                });
                if let (Some(why), Some((_, deposit))) = (held, deposit_out.as_mut()) {
                    deposit.status = DepositStatus::PayoutFailed(why.clone());
                }
            }
            PoolStep::AttemptFailed { r#ref, why } => {
                let why = why.clone();
                let mut stopped = false;
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    payout.failures = payout.failures.saturating_add(1);
                    if payout.failures >= MAX_PAYOUT_ATTEMPTS {
                        payout.failed = Some(why.clone());
                        stopped = true;
                    }
                });
                // a stopped payout stays in `paying`: it is still between its start and its
                // record of payment, which the ICP pocket's check leaves out
                if stopped {
                    if let Some((_, deposit)) = deposit_out.as_mut() {
                        deposit.status = DepositStatus::PayoutFailed(why);
                    }
                    // one that never started leaves the queue, so the payouts behind it go
                    // on; what it holds stays owed and was never reserved (CR-08)
                    let head = ledger.queue.head().map(|(head, _)| head);
                    ledger.queue.0.retain(|(queued, _)| queued != r#ref);
                    if head == Some(*r#ref) {
                        ledger.head_since = (!ledger.queue.0.is_empty()).then_some(at);
                    }
                }
            }
            PoolStep::SwapDeclined { r#ref, leg, .. } => {
                let leg = *leg;
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    let this = payout.leg_mut(leg);
                    this.declined = this.declined.saturating_add(1);
                });
            }
            PoolStep::SwapBegun {
                r#ref,
                before,
                leg,
                amount_in,
            } => {
                let (before, leg, amount_in) = (*before, *leg, *amount_in);
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    let this = payout.leg_mut(leg);
                    this.begun = true;
                    this.before = before;
                    this.amount_in = amount_in;
                });
            }
            PoolStep::Swapped {
                r#ref,
                out,
                send,
                leg,
            } => {
                let (out, send, leg) = (*out, *send, *leg);
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    let this = payout.leg_mut(leg);
                    this.out = Some(out);
                    this.send = send;
                });
            }
            PoolStep::StakeStarted {
                id,
                owner,
                from,
                amount,
                created_at,
            } => {
                ledger.stake_room = sat_add(ledger.stake_room, *amount);
                ledger.pending.insert(
                    *id,
                    PendingOp::Stake {
                        owner: *owner,
                        from: *from,
                        amount: *amount,
                        created_at: *created_at,
                    },
                );
            }
            PoolStep::StakeFailed { id, .. } => {
                if let Some(PendingOp::Stake { amount, .. }) = ledger.pending.remove(id) {
                    ledger.stake_room = sat_sub(ledger.stake_room, amount);
                }
            }
            PoolStep::WithdrawStarted {
                id,
                owner,
                burned,
                usdc,
                to,
                token_out,
                min_out,
                attempt,
            } => {
                let account = PoolAccount::Icp(*owner);
                let line = ShareLine {
                    account,
                    shares: lookup
                        .shares_of(&account.key())
                        .checked_sub(*burned)
                        .unwrap_or(Shares::ZERO),
                };
                shares_out = Some((account.key(), line));
                ledger.total_shares = ledger
                    .total_shares
                    .checked_sub(*burned)
                    .unwrap_or(Shares::ZERO);
                let icp = &mut ledger.pockets.icp;
                icp.free = sat_sub(icp.free, *usdc);
                icp.reserved = sat_add(icp.reserved, *usdc);
                ledger.owed = sat_add(ledger.owed, *usdc);
                ledger.pending.insert(
                    *id,
                    PendingOp::Withdraw {
                        owner: *owner,
                        burned: *burned,
                        usdc: *usdc,
                        to: *to,
                        token_out: *token_out,
                        min_out: *min_out,
                        payout: PayoutProgress {
                            attempt: Some(*attempt),
                            ..PayoutProgress::default()
                        },
                    },
                );
            }
            PoolStep::SwapApproved { r#ref, leg } => {
                let leg = *leg;
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    let this = payout.leg_mut(leg);
                    this.approvals = Some(this.approvals.unwrap_or(0).saturating_add(1));
                });
            }
            PoolStep::SwapUndone {
                r#ref,
                cost,
                withdrawn,
                leg,
            } => {
                let (cost, withdrawn, leg) = (*cost, *withdrawn, *leg);
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    let this = payout.leg_mut(leg);
                    this.undone = Some(cost);
                    this.withdrawn = Some(withdrawn);
                });
            }
            PoolStep::IcpChecked { check } => {
                ledger.icp_check = Some(*check);
                ledger.icp_checked_at = Some(at);
                if check.shortfall {
                    ledger.icp_shortfall_at = Some(at);
                }
            }
            PoolStep::IcpCheckFailed { .. } => ledger.icp_failed_at = Some(at),
            PoolStep::ScanCursor { block, log } => {
                ledger.scan_cursor = Some(*block);
                ledger.scan_log = *log;
            }
            // `apply_payout` records these, and answered before this match
            PoolStep::PayoutSettingsSet { .. }
            | PoolStep::PayoutOpened { .. }
            | PoolStep::PayoutAmountKnown { .. }
            | PoolStep::PayoutQueued { .. }
            | PoolStep::PayoutWaiting { .. }
            | PoolStep::PayoutSigned { .. }
            | PoolStep::PayoutPaid { .. }
            | PoolStep::PayoutReverted { .. }
            | PoolStep::PayoutUndoing { .. }
            | PoolStep::PayoutUndone { .. }
            | PoolStep::PayoutPulled { .. }
            | PoolStep::PayoutStopped { .. }
            | PoolStep::PayoutRelisted { .. }
            | PoolStep::PayoutRefunded { .. }
            | PoolStep::PayoutDelivered { .. }
            | PoolStep::PayoutUnfilled { .. }
            | PoolStep::PayoutResolved { .. }
            | PoolStep::PayoutFitWaited { .. }
            | PoolStep::PayoutFillEarly { .. }
            | PoolStep::OrphanBooked { .. }
            | PoolStep::OrphanRefundSigned { .. }
            | PoolStep::OrphanRefundClosed { .. }
            | PoolStep::TermsClaimed { .. } => {}
            // `apply_rebalance` records these, and answered before this match
            PoolStep::MoveSettingsSet { .. }
            | PoolStep::MoveOpened { .. }
            | PoolStep::MoveProposed { .. }
            | PoolStep::MoveRouted { .. }
            | PoolStep::MoveSigned { .. }
            | PoolStep::MoveBurned { .. }
            | PoolStep::MoveReverted { .. }
            | PoolStep::MoveWithdrawing { .. }
            | PoolStep::MoveWithdrawn { .. }
            | PoolStep::MoveRefused { .. }
            | PoolStep::MoveLanded { .. }
            | PoolStep::MoveSweeping { .. }
            | PoolStep::MoveArrived { .. }
            | PoolStep::MoveStuck { .. }
            | PoolStep::MoveRetried { .. }
            | PoolStep::MoveLost { .. }
            | PoolStep::MoveRefunded { .. }
            | PoolStep::MoveUnfilled { .. }
            | PoolStep::MoveReimbursed { .. } => {}
            // the state indexes it: nothing of the pool moves
            PoolStep::EcoRescueSigned { .. } => {}
        }
        let payout = ledger.evm_changed.take();
        PoolEffects {
            ledger,
            deposit: deposit_out,
            shares: shares_out,
            credited,
            payout,
            queued: None,
            dequeued: None,
            nonce: None,
            intent: None,
            orphan: orphan_out,
            moved: None,
        }
    }

    /// A registration stops being open: out of the open set and its parties' counts.
    fn close_open(&mut self, r#ref: &PoolRef, terms: &Registration) {
        if !self.open.remove(r#ref) {
            return;
        }
        for party in terms.parties() {
            if let Some(count) = self.open_by_party.get_mut(&party) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.open_by_party.remove(&party);
                }
            }
        }
    }

    /// An ICP stake's pull landed: its room taken back, its ckUSDC free on ICP, its shares
    /// outstanding.
    fn book_stake(&mut self, amount: TokenAmount, shares: Shares) {
        self.stake_room = sat_sub(self.stake_room, amount);
        self.pockets.icp.free = sat_add(self.pockets.icp.free, amount);
        self.total_shares = self
            .total_shares
            .checked_add(shares)
            .unwrap_or(Shares::new(u128::MAX));
    }

    /// A payout left the ICP pocket: `spent` in all, `reserve` of it held for it until now.
    fn spend_reserved(&mut self, reserve: TokenAmount, spent: TokenAmount) {
        let icp = &mut self.pockets.icp;
        icp.reserved = sat_sub(icp.reserved, reserve);
        if spent >= reserve {
            icp.free = sat_sub(icp.free, sat_sub(spent, reserve));
        } else {
            icp.free = sat_add(icp.free, sat_sub(reserve, spent));
        }
    }

    /// Changes the payout progress of the deposit or withdrawal `r#ref` names: a withdrawal
    /// in the ledger here, a deposit answered for the map.
    fn with_payout(
        &mut self,
        r#ref: &PoolRef,
        lookup: &impl PoolLookup,
        change: impl FnOnce(&mut PayoutProgress),
    ) -> Option<(PoolRef, Registered)> {
        if let Some(mut deposit) = lookup.deposit(r#ref) {
            change(&mut deposit.payout);
            return Some((*r#ref, deposit));
        }
        // a Base payout's legs: answered through `evm_changed`, which `apply` collects
        if let Some(mut payout) = crate::pool::evm_payout_id(r#ref).and_then(|id| lookup.payout(id))
        {
            change(payout.legs.get_or_insert_with(PayoutProgress::default));
            self.evm_changed = Some(payout);
            return None;
        }
        let id = PoolOpId::new(r#ref.into_bytes());
        if let Some(PendingOp::Withdraw { payout, .. }) = self.pending.get_mut(&id) {
            change(payout);
        }
        None
    }
}

/// What an arrived deposit puts on the `owed` line: a swap its whole hold (the payout and
/// every fee its path can cost), so the share price neither counts the payout nor the fees
/// it will spend; a stake its whole arrival, so its own mint is priced on the assets before
/// it.
fn owed_line(deposit: &Registered) -> TokenAmount {
    if deposit.terms.kind.is_swap() {
        deposit.held()
    } else {
        deposit.owed()
    }
}

/// The least ICP fees a swap paying `token_out` commits (P11d, 2026-10-21): its path's own
/// ckUSDC fees, ckUSDC alone 1, a direct token 2 (the approval and the deposit), a token
/// through ICP 3 (the ckUSDC leg's 2 and 1 covering the ICP leg's ICP fees). None for a token
/// not listed, or no ckUSDC fee read.
pub fn icp_fees_floor(settings: &PoolSettings, token_out: Principal) -> Option<TokenAmount> {
    let fee = settings.fee_of(settings.ckusdc_ledger)?;
    let count: u32 = match settings.path_of(token_out)? {
        crate::pool::RoutePath::Ckusdc => 1,
        crate::pool::RoutePath::Direct { .. } => 2,
        crate::pool::RoutePath::ViaIcp { .. } => 3,
    };
    fee.checked_mul(count)
}

/// Whether a swap step may name `leg` of `payout`: the first leg of any ICPSwap path; the
/// second of a path through ICP once its first paid out; the way back once its second
/// cannot pay (declined twice, or undone), never once it paid out.
fn admits_leg(payout: &PayoutProgress, leg: Option<u8>) -> bool {
    let path = payout.attempt.map(|attempt| attempt.path);
    match leg {
        None | Some(0) => path.is_some_and(|path| path.swaps()),
        Some(super::LEG_SECOND) => {
            matches!(path, Some(PayoutPath::IcpswapVia { .. })) && payout.swap.out.is_some()
        }
        Some(super::LEG_BACK) => {
            let second = payout.leg(Some(super::LEG_SECOND));
            matches!(path, Some(PayoutPath::IcpswapVia { .. }))
                && payout.swap.out.is_some()
                && second.out.is_none()
                && (second.undone.is_some() || !second.begun)
        }
        Some(_) => false,
    }
}

/// `owner`'s line with `minted` more shares.
fn credit_shares(
    lookup: &impl PoolLookup,
    owner: &PoolAccount,
    minted: Shares,
) -> (OwnerKey, ShareLine) {
    let key = owner.key();
    let shares = lookup
        .shares_of(&key)
        .checked_add(minted)
        .unwrap_or(Shares::new(u128::MAX));
    (
        key,
        ShareLine {
            account: *owner,
            shares,
        },
    )
}

/// The amounts of a registration hold together: something must arrive, a swap carries a
/// premium below its minimum and no rebalancing share, a stake a rebalancing share below its
/// minimum and no premium (the ref commits only the one fee its kind has).
fn admits_terms(terms: &Registration) -> Result<(), PoolStepError> {
    if terms.min_usdc == TokenAmount::ZERO {
        return Err(PoolStepError::Zero("min_usdc"));
    }
    if terms.min_usdc.try_into_u128().is_none() {
        return Err(PoolStepError::Overflow);
    }
    match terms.kind {
        DepositKind::Swap { .. } => {
            if terms.rebalance_share != TokenAmount::ZERO {
                return Err(PoolStepError::Terms("a swap carries no rebalancing share"));
            }
            let fees = terms
                .premium
                .checked_add(terms.icp_fees())
                .ok_or(PoolStepError::Overflow)?;
            if fees >= terms.min_usdc {
                return Err(PoolStepError::Terms(
                    "the premium and the ICP fees must be below min_usdc",
                ));
            }
        }
        DepositKind::Stake { .. } => {
            if terms.premium != TokenAmount::ZERO || terms.icp_fees() != TokenAmount::ZERO {
                return Err(PoolStepError::Terms(
                    "a stake carries no premium and no ICP fees",
                ));
            }
            if terms.rebalance_share >= terms.min_usdc {
                return Err(PoolStepError::Terms(
                    "the rebalancing share must be below min_usdc",
                ));
            }
        }
    }
    Ok(())
}

/// How much of the settings' premium share of what arrived a swap's premium must be at its
/// arrival, in percent (P11e): the quoter prices the premium on the most a route delivers, and
/// a route delivers a little less than its most.
pub const PREMIUM_AT_ARRIVAL_PERCENT: u32 = 90;

/// The premium a swap of `min_usdc` carries at the least (P11d, 2026-10-21: no floor):
/// exactly the settings' share of it, rounded up, pure pool income. Every ledger fee is the
/// user's own line beside it.
pub fn premium_share(settings: &PoolSettings, min_usdc: TokenAmount) -> Option<TokenAmount> {
    min_usdc
        .checked_mul(settings.premium_bps)?
        .checked_div_ceil(10_000_u32)
}

/// Every rule a deposit's terms meet at its arrival (P11d, 2026-10-21: no registration),
/// `terms.min_usdc` being what arrived, read off the ledger: the fees' own rules, then for a
/// swap the pool open for swaps, the token listed (no cap: the per-swap cap is the
/// quoter's), the ICP fees at least the path's own ckUSDC fees and, on a ckUSDC payout, `arrived - premium - icp_fees` at
/// least the recipient's minimum; for a stake the rebalancing share's floor and the TVL cap.
/// A payout the ICP pocket cannot cover yet is queued, never refused.
pub fn check_arrival(ledger: &PoolLedger, terms: &Registration) -> Result<(), RegisterRefusal> {
    let settings = ledger
        .settings
        .as_ref()
        .ok_or(RegisterRefusal::NoSettings)?;
    admits_terms(terms).map_err(|error| RegisterRefusal::NotAllowed(error.to_string()))?;
    match terms.kind {
        DepositKind::Swap {
            token_out, min_out, ..
        } => {
            if !settings.open_for_swaps {
                return Err(RegisterRefusal::ClosedForSwaps);
            }
            if !settings.is_listed(token_out) {
                return Err(RegisterRefusal::TokenNotListed(token_out));
            }
            // the premium at arrival (P11e): at least 90% of the settings' share of what
            // arrived, the quoter pricing it on the most a route delivers
            let share = premium_share(settings, terms.min_usdc).ok_or(
                RegisterRefusal::NotAllowed("the premium's share overflows".to_string()),
            )?;
            let least = share
                .checked_mul(PREMIUM_AT_ARRIVAL_PERCENT)
                .and_then(|scaled| scaled.checked_div_ceil(100_u32))
                .unwrap_or(TokenAmount::MAX);
            if terms.premium < least {
                return Err(RegisterRefusal::PremiumTooLow { least });
            }
            let floor = icp_fees_floor(settings, token_out)
                .ok_or_else(|| RegisterRefusal::NotAllowed("no ckUSDC fee was read".to_string()))?;
            if terms.icp_fees() < floor {
                return Err(RegisterRefusal::NotAllowed(format!(
                    "the ICP fees must be at least {floor}"
                )));
            }
            let payout = terms
                .min_usdc
                .checked_sub(terms.premium)
                .and_then(|left| left.checked_sub(terms.icp_fees()))
                .unwrap_or(TokenAmount::ZERO);
            if token_out == settings.ckusdc_ledger && payout < TokenAmount::from(min_out) {
                return Err(RegisterRefusal::NotAllowed(
                    "below the recipient's minimum".to_string(),
                ));
            }
        }
        DepositKind::Stake { .. } => {
            // the canister's floor on the staker's rebalancing share (P11b), once set
            if let Some(payouts) = ledger.lines().settings {
                let least = payouts
                    .least_rebalance_share(terms.min_usdc)
                    .unwrap_or(TokenAmount::MAX);
                if terms.rebalance_share < least {
                    return Err(RegisterRefusal::NotAllowed(format!(
                        "the rebalancing share must be at least {least}"
                    )));
                }
            }
            let would_be = sat_add(ledger.tvl_measure(), terms.min_usdc);
            if would_be > settings.tvl_cap {
                return Err(RegisterRefusal::AboveTvlCap {
                    cap: settings.tvl_cap,
                    would_be,
                });
            }
        }
    }
    Ok(())
}
