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

use super::{
    Arrival, DepositKind, DepositStatus, IcpCheck, PayoutPath, PayoutProgress, PayoutQueue,
    PendingOp, Pockets, PoolAccount, PoolSettings, PoolSettingsError, PoolStep, Registered,
    Registration, ShareLine, Shares, UnseenArrival,
};
use crate::chain::ChainId;
use crate::hash::{OwnerKey, PoolOpId, PoolRef};
use crate::numeric::{BlockNumber, Timestamp, TokenAmount};
use crate::pool::CreditKey;
use candid::Principal;
use minicbor::{Decode, Encode};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// How many failed drives a payout gets before it stops for a human.
pub const MAX_PAYOUT_ATTEMPTS: u32 = 10;

/// The ledger fees an ICPSwap payout's path costs the pool, in ckUSDC fees: the approval,
/// the pool's pull, and the ckUSDC transfer should the leg fall back.
pub const ICPSWAP_PATH_FEES: u32 = 3;

/// What the fold holds besides the ledger, as `admits` and `apply` read it.
pub trait PoolLookup {
    fn deposit(&self, r#ref: &PoolRef) -> Option<Registered>;
    fn shares_of(&self, owner: &OwnerKey) -> Shares;
    fn is_credited(&self, key: &CreditKey) -> bool;
}

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
    /// How far the canister's own read of Base got.
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
}

/// What one step changes: the ledger, and the entries of the maps beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolEffects {
    pub ledger: PoolLedger,
    pub deposit: Option<(PoolRef, Registered)>,
    pub shares: Option<(OwnerKey, ShareLine)>,
    pub credited: Option<CreditKey>,
}

/// Why a pool step was refused.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PoolStepError {
    #[error("the pool has no settings")]
    NoSettings,
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

/// What a step that names a payout (a deposit's or a withdrawal's) names.
enum Target {
    Deposit(Box<Registered>),
    Withdraw(Box<PendingOp>),
}

impl Target {
    fn payout(&self) -> Option<&PayoutProgress> {
        match self {
            Target::Deposit(deposit) => Some(&deposit.payout),
            Target::Withdraw(op) => match op.as_ref() {
                PendingOp::Withdraw { payout, .. } => Some(payout),
                PendingOp::Stake { .. } => None,
            },
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
            sat_add(self.pockets.base.free, self.pockets.base.reserved),
            self.unattributed,
        )
    }

    /// What the canister's ckUSDC account should hold for the pool.
    pub fn icp_held(&self) -> TokenAmount {
        sat_add(self.pockets.icp.free, self.pockets.icp.reserved)
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

    /// Whether `amount` arriving for `deposit` fits the pool, measured on what arrived and
    /// not on what was registered: a swap within the per-swap cap, a stake within the TVL
    /// cap. Answers what a swap's payout holds once it starts: the payout and every fee its
    /// path can cost. A payout the ICP pocket cannot cover yet is queued, never refused.
    pub fn arrival_fits(
        &self,
        deposit: &Registered,
        amount: TokenAmount,
    ) -> Result<TokenAmount, &'static str> {
        let settings = self.settings.as_ref().ok_or("the pool has no settings")?;
        match deposit.terms.kind {
            DepositKind::Swap { token_out, .. } => {
                if amount > settings.per_swap_cap {
                    return Err("above the per-swap cap");
                }
                let payout = amount
                    .checked_sub(deposit.terms.premium)
                    .ok_or("below the premium")?;
                path_fees(settings, token_out)
                    .and_then(|fees| payout.checked_add(fees))
                    .ok_or("the payout overflows")
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
                        || !self.paying.is_empty();
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
                if lookup.is_credited(credit) {
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
                let on_icpswap = matches!(
                    payout.attempt.map(|attempt| attempt.path),
                    Some(PayoutPath::Icpswap { .. })
                );
                // a leg falls back before its swap, or after one that did not happen and was
                // undone (the driver read the pool to know): never once the swap paid out
                if !target.in_payout() || !on_icpswap || payout.swap.out.is_some() {
                    return Err(PoolStepError::Terms(
                        "only an ICPSwap leg with no output falls back",
                    ));
                }
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
            PoolStep::AttemptFailed { r#ref, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                // a queued payout has not started: it waits, and never fails for a human
                if self.queue.0.iter().any(|(queued, _)| queued == r#ref) {
                    return Err(PoolStepError::Terms("a queued payout has not started"));
                }
                if !target.in_payout() {
                    return Err(PoolStepError::Terms("only a payout in progress fails"));
                }
                Ok(())
            }
            PoolStep::SwapDeclined { r#ref, .. } | PoolStep::SwapBegun { r#ref, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                let payout = target.payout().ok_or(PoolStepError::Mismatch)?;
                let on_icpswap = matches!(
                    payout.attempt.map(|attempt| attempt.path),
                    Some(PayoutPath::Icpswap { .. })
                );
                if !target.in_payout() || !on_icpswap || payout.swap.begun {
                    return Err(PoolStepError::Terms(
                        "the ICPSwap leg is not before its call",
                    ));
                }
                if matches!(step, PoolStep::SwapDeclined { .. }) && payout.swap.declined > 0 {
                    return Err(PoolStepError::Terms(
                        "a declined leg falls back the second time",
                    ));
                }
                Ok(())
            }
            PoolStep::Swapped { r#ref, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                let payout = target.payout().ok_or(PoolStepError::Mismatch)?;
                if !payout.swap.begun || payout.swap.out.is_some() {
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
            PoolStep::SwapApproved { r#ref } | PoolStep::SwapUndone { r#ref, .. } => {
                let target = self
                    .target(r#ref, lookup)
                    .ok_or(PoolStepError::Unknown(*r#ref))?;
                let payout = target.payout().ok_or(PoolStepError::Mismatch)?;
                let on_icpswap = matches!(
                    payout.attempt.map(|attempt| attempt.path),
                    Some(PayoutPath::Icpswap { .. })
                );
                if !target.in_payout() || !on_icpswap || payout.swap.out.is_some() {
                    return Err(PoolStepError::Terms("the ICPSwap leg has its output"));
                }
                if matches!(step, PoolStep::SwapUndone { .. })
                    && (!payout.swap.begun || payout.swap.undone.is_some())
                {
                    return Err(PoolStepError::Terms("only a begun swap is undone, once"));
                }
                Ok(())
            }
            PoolStep::IcpChecked { .. } | PoolStep::IcpCheckFailed { .. } => Ok(()),
            PoolStep::ScanCursor { block } => match self.scan_cursor {
                Some(at) if *block < at => Err(PoolStepError::CursorBack { at, to: *block }),
                _ => Ok(()),
            },
        }
    }

    /// Records a step [`PoolLedger::admits`] admitted, sealed at `at`. Never fails: a line
    /// that would go below zero stops at zero.
    pub fn apply(self, step: &PoolStep, lookup: &impl PoolLookup, at: Timestamp) -> PoolEffects {
        let mut ledger = self;
        let mut deposit_out = None;
        let mut shares_out = None;
        let mut credited = None;
        match step {
            PoolStep::SettingsSet { settings } => ledger.settings = Some(settings.clone()),
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
                ledger.unseen.push(UnseenArrival {
                    chain: credit.chain,
                    block: *block,
                    amount: *amount,
                });
                credited = Some(*credit);
            }
            PoolStep::Unattributed {
                credit,
                amount,
                block,
                r#ref,
                ..
            } => {
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
                ledger.stake_room = sat_sub(ledger.stake_room, *amount);
                ledger.pockets.icp.free = sat_add(ledger.pockets.icp.free, *amount);
                ledger.total_shares = ledger
                    .total_shares
                    .checked_add(*shares)
                    .unwrap_or(Shares::new(u128::MAX));
                shares_out = Some(credit_shares(lookup, owner, *shares));
            }
            PoolStep::WithdrawnIcp { id, spent, .. } => {
                if let Some(PendingOp::Withdraw { usdc, .. }) = ledger.pending.remove(id) {
                    ledger.owed = sat_sub(ledger.owed, usdc);
                    ledger.spend_reserved(usdc, *spent);
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
                }
            }
            PoolStep::SwapDeclined { r#ref, .. } => {
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    payout.swap.declined = payout.swap.declined.saturating_add(1);
                });
            }
            PoolStep::SwapBegun { r#ref, before } => {
                let before = *before;
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    payout.swap.begun = true;
                    payout.swap.before = before;
                });
            }
            PoolStep::Swapped { r#ref, out, send } => {
                let (out, send) = (*out, *send);
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    payout.swap.out = Some(out);
                    payout.swap.send = send;
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
            PoolStep::SwapApproved { r#ref } => {
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    payout.swap.approvals =
                        Some(payout.swap.approvals.unwrap_or(0).saturating_add(1));
                });
            }
            PoolStep::SwapUndone {
                r#ref,
                cost,
                withdrawn,
            } => {
                let (cost, withdrawn) = (*cost, *withdrawn);
                deposit_out = ledger.with_payout(r#ref, lookup, |payout| {
                    payout.swap.undone = Some(cost);
                    payout.swap.withdrawn = Some(withdrawn);
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
            PoolStep::ScanCursor { block } => ledger.scan_cursor = Some(*block),
        }
        PoolEffects {
            ledger,
            deposit: deposit_out,
            shares: shares_out,
            credited,
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

/// The ledger fees a payout of `token_out` can cost the pool: ckUSDC's transfer, or the
/// ICPSwap path's (the approval, the pool's pull, and the ckUSDC fall back).
pub fn path_fees(settings: &PoolSettings, token_out: Principal) -> Option<TokenAmount> {
    let fee = settings.fee_of(settings.ckusdc_ledger)?;
    if token_out == settings.ckusdc_ledger {
        Some(fee)
    } else {
        fee.checked_mul(ICPSWAP_PATH_FEES)
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
            if terms.premium >= terms.min_usdc {
                return Err(PoolStepError::Terms("the premium must be below min_usdc"));
            }
        }
        DepositKind::Stake { .. } => {
            if terms.premium != TokenAmount::ZERO {
                return Err(PoolStepError::Terms("a stake carries no premium"));
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

/// The ref and the fee rules of a registration, refused in the interface's words: settings
/// present, a swap only while the pool is open for swaps, the ref the one its terms commit to,
/// not registered yet, and the fee its kind does not have at zero.
pub fn check_ref(
    ledger: &PoolLedger,
    r#ref: PoolRef,
    terms: &Registration,
    lookup: &impl PoolLookup,
) -> Result<(), RegisterRefusal> {
    let settings = ledger
        .settings
        .as_ref()
        .ok_or(RegisterRefusal::NoSettings)?;
    if terms.kind.is_swap() && !settings.open_for_swaps {
        return Err(RegisterRefusal::ClosedForSwaps);
    }
    if terms.pool_ref() != Some(r#ref) {
        return Err(RegisterRefusal::BadRef);
    }
    if lookup.deposit(&r#ref).is_some() {
        return Err(RegisterRefusal::Known);
    }
    admits_terms(terms).map_err(|error| RegisterRefusal::NotAllowed(error.to_string()))
}

/// The least premium a swap of `min_usdc` paying `token_out` carries (interface section 4,
/// 2026-10-21): the settings' share of it, rounded up, and never below what the pool pays in
/// ckUSDC for the payout's path: the ckUSDC fee for a ckUSDC payout, three of them for a
/// token's ICPSwap path (the approval, the pull into ICPSwap, the transfer out). The token's
/// own ledger fee is the recipient's, out of the output, and never in the premium.
pub fn least_premium(
    settings: &PoolSettings,
    min_usdc: TokenAmount,
    token_out: Principal,
) -> Option<TokenAmount> {
    let share = min_usdc
        .checked_mul(settings.premium_bps)?
        .checked_div_ceil(10_000_u32)?;
    Some(share.max(path_fees(settings, token_out)?))
}

/// The pool's limits on a registration, read off the ledger: the token out listed, the open
/// registrations of the payer and of the owner below the limit, then for a swap the per-swap
/// cap, the premium's floor, the recipient's own minimum on a ckUSDC payout and enough free
/// ckUSDC behind the queue at this moment; for a stake the TVL cap. A registration holds
/// nothing (Jeff, 2026-10-21, interface section 4): `max_held_bps` limits nothing.
pub fn check_terms(ledger: &PoolLedger, terms: &Registration) -> Result<(), RegisterRefusal> {
    let settings = ledger
        .settings
        .as_ref()
        .ok_or(RegisterRefusal::NoSettings)?;
    if let DepositKind::Swap { token_out, .. } = terms.kind {
        if !settings.is_listed(token_out) {
            return Err(RegisterRefusal::TokenNotListed(token_out));
        }
    }
    for party in terms.parties() {
        if ledger.open_of(&party) >= settings.max_open_per_payer {
            return Err(RegisterRefusal::NotAllowed(
                "too many open deposits".to_string(),
            ));
        }
    }
    match terms.kind {
        DepositKind::Swap {
            token_out, min_out, ..
        } => {
            if terms.min_usdc > settings.per_swap_cap {
                return Err(RegisterRefusal::AboveSwapCap {
                    cap: settings.per_swap_cap,
                });
            }
            let least = least_premium(settings, terms.min_usdc, token_out)
                .ok_or_else(|| RegisterRefusal::NotAllowed("no ckUSDC fee was read".to_string()))?;
            if terms.premium < least {
                return Err(RegisterRefusal::PremiumTooLow { least });
            }
            let reserve = terms.reserve();
            if token_out == settings.ckusdc_ledger && reserve < TokenAmount::from(min_out) {
                return Err(RegisterRefusal::NotAllowed(
                    "below the recipient's minimum".to_string(),
                ));
            }
            // holds nothing: the free ckUSDC behind the queue must cover it at this moment
            if ledger.icp_available() < reserve {
                return Err(RegisterRefusal::PocketShort);
            }
        }
        DepositKind::Stake { .. } => {
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
