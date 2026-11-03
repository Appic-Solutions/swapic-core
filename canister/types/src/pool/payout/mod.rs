//! The pool's payouts from its Base pocket (spec 3.2 and 3.3, the fixed interface section 3b):
//! an ICP user's swap to any EVM token, and an EVM staker's withdrawal, both paid by ONE
//! canister signature of `run(order)` on Base, the order sized exactly after any ICPSwap leg.
//!
//! A payout is opened with what undoes it, learns its exact amount, waits in a first in first
//! out queue while the Base pocket cannot cover it, then waits for an order, is signed once
//! (never again: a revert undoes it, 2026-10-23), and is paid or undone. Pure: the rules are
//! the pool ledger's (`admits`, `apply`); this module holds the records and their policy.
//!
//! Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a new
//! field is optional.

#[cfg(test)]
mod tests;

use super::{principal_cbor, IcpAccount, PayoutProgress, PoolAccount, Shares};
use crate::chain::ChainId;
use crate::evm::EvmAddress;
use crate::hash::PoolRef;
use crate::hash::TxHash;
use crate::numeric::{BlockNumber, Timestamp, TokenAmount};
use candid::Principal;
use ic_stable_structures::storable::{Bound, Storable};
use minicbor::{Decode, Encode};
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::time::Duration;
use thiserror::Error;

/// What a payout pays and to whom: the user's own terms, which the order the watcher hands
/// over is checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct PayoutTerms {
    /// Where the user is paid: a chain the canister signs on, Base included.
    #[n(0)]
    pub dst_chain: ChainId,
    #[n(1)]
    pub recipient: EvmAddress,
    /// An EVM token, or `0xEeee...EEeE` for the native coin.
    #[n(2)]
    pub token_out: EvmAddress,
    /// In `token_out`'s units.
    #[n(3)]
    pub min_out: TokenAmount,
    /// After this, nothing is signed and the payout is undone.
    #[n(4)]
    pub deadline: Timestamp,
}

/// An ICP user's swap through the pool, as `swap_from_icp` takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct IcpSwapTerms {
    /// ckUSDC, or a token of `icp_routes`.
    #[cbor(n(0), with = "principal_cbor")]
    pub token_in: Principal,
    #[n(1)]
    pub amount_in: TokenAmount,
    /// The caller's account the token is pulled from (`icrc2_transfer_from`).
    #[n(2)]
    pub from: IcpAccount,
    /// The least ckUSDC the route's ICPSwap legs must give (`amount_in` for ckUSDC). A
    /// planned swap's is the sale floor the canister set.
    #[n(3)]
    pub min_usdc: TokenAmount,
    /// Micro USDC the pool keeps. Zero for a planned swap: its premium is the settings'
    /// share of what its sale made, taken when the amount is known.
    #[n(4)]
    pub premium: TokenAmount,
    /// Micro USDC the canister keeps for the payout's gas: a planned swap's is the payouts'
    /// flat floor, the CCTP one for a plan that burns.
    #[n(5)]
    pub network_fee: TokenAmount,
    /// Where an undone swap's money goes back to.
    #[n(6)]
    pub refund_to: IcpAccount,
    /// The payout plan flow: the user's slippage, at most 500 bps, half of it
    /// the sale's. None: a swap opened before the plan flow.
    #[n(7)]
    pub slippage_bps: Option<u16>,
    /// The plan's `amountIn`: the payout the quoter built it for.
    #[n(8)]
    pub planned: Option<TokenAmount>,
}

impl IcpSwapTerms {
    /// Whether the swap was opened with a payout plan.
    pub fn is_planned(&self) -> bool {
        self.planned.is_some()
    }
}

/// An EVM staker's withdrawal, as the staker signed it, with the shares it burned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct EvmWithdrawal {
    /// The EVM address owning the shares, the EIP-712 signer.
    #[n(0)]
    pub staker: EvmAddress,
    /// Micro USDC taken out of the pool.
    #[n(1)]
    pub usdc: TokenAmount,
    #[n(2)]
    pub max_shares: Shares,
    /// Swapic's, the cost of one ICP to Base move: booked to the Base fee line once paid.
    #[n(3)]
    pub move_fee: TokenAmount,
    /// Swapic's, the payout's gas: booked to the Base fee line once paid.
    #[n(4)]
    pub network_fee: TokenAmount,
    /// Once per staker.
    #[n(5)]
    pub nonce: u64,
    /// `ceil(usdc * S / A)` at the burn.
    #[n(6)]
    pub burned: Shares,
}

impl EvmWithdrawal {
    /// What the payout order spends on Base: the withdrawal less its move fee and its
    /// network fee. None when they take all of it.
    pub fn payout_amount(&self) -> Option<TokenAmount> {
        self.usdc
            .checked_sub(self.move_fee)?
            .checked_sub(self.network_fee)
            .filter(|amount| *amount > TokenAmount::ZERO)
    }
}

/// What a payout is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum PayoutKind {
    #[n(0)]
    IcpSwap(#[n(0)] IcpSwapTerms),
    #[n(1)]
    Withdrawal(#[n(0)] EvmWithdrawal),
}

/// What undoes a payout that cannot be paid by its deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum Undo {
    /// An ICP swap: what it pulled (as ckUSDC once its legs made it ckUSDC, else the token
    /// itself) goes back to `to`, less the ledger's fee.
    #[n(0)]
    Refund {
        #[n(0)]
        to: IcpAccount,
    },
    /// A withdrawal: exactly the shares it burned, minted back to their owner.
    #[n(1)]
    Remint {
        #[n(0)]
        owner: PoolAccount,
        #[n(1)]
        shares: Shares,
    },
}

/// Where a payout stands.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum PayoutStatus {
    /// Opened: an ICP swap's pull is out, or a withdrawal's amount is about to be known.
    #[n(0)]
    Opened,
    /// An ICP swap's pull landed and its ICPSwap legs run.
    #[n(1)]
    Swapping,
    /// Its exact amount is known, and it is in neither the queue nor waiting yet.
    #[n(2)]
    Known,
    /// Waits, first in first out, for the Base pocket to cover it.
    #[n(3)]
    Queued,
    /// Its USDC is reserved on Base; it waits for an order.
    #[n(4)]
    Waiting,
    /// Its order was signed: the line that recorded the transaction's bytes.
    #[n(5)]
    Signed(#[n(0)] u64),
    #[n(6)]
    Paid,
    /// Its undo started (an ICP refund is out): nothing is signed for it any more.
    #[n(7)]
    Undoing(#[n(0)] String),
    #[n(8)]
    Undone(#[n(0)] String),
    /// Stopped for a human.
    #[n(9)]
    Failed(#[n(0)] String),
    /// Its transaction reverted at depth: undone once the outbox let it go at
    /// the crediting depth and Swapic says its order never ran. Never signed again.
    #[n(10)]
    Reverted,
    /// Its order ran on Base and published an Eco intent: its ckUSDC, its owed
    /// line, its USDC in flight and a withdrawal's fee stay as they were until the intent's
    /// fill is read past its route deadline.
    #[n(11)]
    Delivering,
    /// Its Eco intent never filled: undone next, at once; its USDC comes back from Eco
    /// (`reclaim`).
    #[n(12)]
    Unfilled,
}

/// The most times a planned swap's plan is tried before its payout is left to the watcher's
/// order: a scaler read that failed once does not give up the fast path.
pub const MAX_FIT_TRIES: u8 = 3;

/// Why a payout a controller refunded is undone.
pub const REFUNDED: &str = "refunded by a controller";

/// What a controller decided for a payout stopped for a human (`resolve_payout`, Jeff
/// 2026-10-23): its money back to its user, or its money found delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum PayoutResolution {
    /// Undone: an ICP swap's refund (all it made less one ledger fee, `refund_of`), a
    /// withdrawal's shares minted back.
    #[n(0)]
    Refund,
    /// Paid: its signed order ran after all, and the paid line is booked, once.
    #[n(1)]
    MarkPaid,
}

/// Where the Base pocket's USDC of an unfilled Eco intent stands: owed back by Eco (in the
/// Base pocket's `in_flight`), or credited back, once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum Reclaim {
    #[n(0)]
    Pending,
    #[n(1)]
    Credited,
}

impl PayoutStatus {
    /// Whether nothing more happens to it.
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            PayoutStatus::Paid | PayoutStatus::Undone(_) | PayoutStatus::Failed(_)
        )
    }
}

/// The order a payout was signed with, kept for what reads it after: the undo's `used` read,
/// an Eco rescue, the canister's own CCTP finish.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct PinnedOrder {
    #[cbor(n(0), with = "minicbor::bytes")]
    pub order_id: [u8; 32],
    /// `abi.encode` of the Order.
    #[cbor(n(1), with = "minicbor::bytes")]
    pub order: Vec<u8>,
    #[n(2)]
    pub gas_limit: u64,
    /// The order's own deadline, in seconds.
    #[n(3)]
    pub deadline_s: u64,
}

/// A payout from the Base pocket.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct EvmPayout {
    #[n(0)]
    pub id: u64,
    #[n(1)]
    pub kind: PayoutKind,
    #[n(2)]
    pub terms: PayoutTerms,
    #[n(3)]
    pub undo: Undo,
    #[n(4)]
    pub status: PayoutStatus,
    /// The exact USDC the payout order spends on Base, once known.
    #[n(5)]
    pub amount: Option<TokenAmount>,
    /// A withdrawal's network fee and move fee, Swapic's, held on Base with the payout,
    /// booked to the Base fee line when it is paid and back to `free` when it is undone.
    #[n(6)]
    pub base_fee: Option<TokenAmount>,
    /// An ICP swap's ckUSDC, all of it (`u`), once its legs made it.
    #[n(7)]
    pub received: Option<TokenAmount>,
    #[n(8)]
    pub order: Option<PinnedOrder>,
    #[n(9)]
    pub tx_hash: Option<TxHash>,
    #[n(10)]
    pub opened_at: Timestamp,
    /// When it last started waiting for an order.
    #[n(11)]
    pub waiting_since: Option<Timestamp>,
    /// Its place in the queue while queued.
    #[n(12)]
    pub queued_at: Option<u64>,
    /// The block its transaction was paid in.
    #[n(13)]
    pub paid_block: Option<BlockNumber>,
    /// The Eco intent its order published, read off the paid receipt: an unfilled one is
    /// refunded to the canister, and that refund reopens the payout.
    #[cbor(n(14), with = "minicbor::bytes")]
    pub intent: Option<[u8; 32]>,
    /// The ledger block of an ICP swap's pull, or of its refund.
    #[n(15)]
    pub icp_block: Option<u64>,
    /// Whether its Base hold is reserved in the Base pocket now (while it waits for an
    /// order, and again after a revert).
    #[n(16)]
    pub base_reserved: bool,
    /// An ICP swap's legs from its token to ckUSDC: the pull's dedup pair, and its route's
    /// path read the way the token goes in. A path through ICP's `first` is the token's ICP
    /// pool (its first leg, the token into ICP, and its way back, ICP into the token) and its
    /// `second` the ICP/ckUSDC pool (ICP into ckUSDC).
    /// None for a payout with no leg (ckUSDC in, a withdrawal).
    #[n(17)]
    pub legs: Option<PayoutProgress>,
    /// When its undo began: an ICP refund's `created_at_time`, within the ledger's window.
    #[n(18)]
    pub undoing_at: Option<Timestamp>,
    /// An ICP swap paid once: its ckUSDC went free to the pool then, so an undo after an Eco
    /// refund reopened it takes the refund from the free ckUSDC.
    #[n(19)]
    pub icp_released: Flag,
    /// What an unfilled Eco intent owes the Base pocket: its reward, the payout's `amount`,
    /// pending until Eco's refund is credited, once.
    #[n(20)]
    pub reclaim: Option<Reclaim>,
    /// A planned ICP swap's payout plan as the user handed it in (`amountIn` = `planned`).
    #[n(21)]
    pub plan: Option<PinnedOrder>,
    /// How many times its plan was tried and waited (a failed scaler read, a refused check),
    /// at most [`MAX_FIT_TRIES`]: none, never.
    #[n(22)]
    pub fit_tries: Option<u8>,
    /// When its Eco intent's fill was last read too early (no block every node agreed on was
    /// past the route's deadline yet): not read again within one wait.
    #[n(23)]
    pub fill_early_at: Option<Timestamp>,
    /// The block its transaction last reverted in: whether its order ran is read at it or
    /// after, never before (a reorg could have turned that revert into a payment).
    #[n(24)]
    pub reverted_block: Option<BlockNumber>,
    /// When the canister recorded its last revert: whether its order ran is first read one
    /// agreed block's wait on the pool chain after it (no agreed block is at or past the
    /// revert's before). None on a revert recorded before it was, read at once.
    #[n(25)]
    pub reverted_at: Option<Timestamp>,
}

/// A flag stored as nothing while it is false, so a record written before it existed reads
/// back with it false and writes back the same bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flag(pub bool);

impl<C> Encode<C> for Flag {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.bool(self.0)?.ok()
    }

    fn is_nil(&self) -> bool {
        !self.0
    }
}

impl<'b, C> Decode<'b, C> for Flag {
    fn decode(
        d: &mut minicbor::Decoder<'b>,
        _ctx: &mut C,
    ) -> Result<Self, minicbor::decode::Error> {
        if matches!(
            d.datatype()?,
            minicbor::data::Type::Null | minicbor::data::Type::Undefined
        ) {
            d.skip()?;
            return Ok(Self(false));
        }
        d.bool().map(Self)
    }

    fn nil() -> Option<Self> {
        Some(Self(false))
    }
}

crate::storable_as_cbor!(EvmPayout);

impl EvmPayout {
    pub fn new(id: u64, kind: PayoutKind, terms: PayoutTerms, undo: Undo, at: Timestamp) -> Self {
        Self {
            id,
            kind,
            terms,
            undo,
            status: PayoutStatus::Opened,
            amount: None,
            base_fee: None,
            received: None,
            order: None,
            tx_hash: None,
            opened_at: at,
            waiting_since: None,
            queued_at: None,
            paid_block: None,
            intent: None,
            icp_block: None,
            base_reserved: false,
            legs: None,
            undoing_at: None,
            icp_released: Flag(false),
            reclaim: None,
            plan: None,
            fit_tries: None,
            fill_early_at: None,
            reverted_block: None,
            reverted_at: None,
        }
    }

    /// What the payout holds of the Base pocket: the order's amount, and a withdrawal's
    /// network fee, which stays at the canister's Base address.
    pub fn base_hold(&self) -> TokenAmount {
        self.amount
            .unwrap_or(TokenAmount::ZERO)
            .checked_add(self.base_fee.unwrap_or(TokenAmount::ZERO))
            .unwrap_or(TokenAmount::MAX)
    }

    /// What the payout puts on the pool's `owed` line once its amount is known: the order's
    /// amount and a withdrawal's fees, none of them the stakers'.
    pub fn owed(&self) -> TokenAmount {
        match self.amount {
            Some(_) => self.base_hold(),
            None => TokenAmount::ZERO,
        }
    }

    /// An ICP swap's ckUSDC held in the ICP pocket until the payout is paid or undone: all it
    /// made but the network fee, which went to the ICP fee line.
    pub fn icp_hold(&self) -> TokenAmount {
        match (&self.kind, self.received) {
            (PayoutKind::IcpSwap(terms), Some(received)) => received
                .checked_sub(terms.network_fee)
                .unwrap_or(TokenAmount::ZERO),
            _ => TokenAmount::ZERO,
        }
    }

    /// An ICP swap's network fee, on the ICP fee line once its ckUSDC is known.
    pub fn icp_fee(&self) -> TokenAmount {
        match (&self.kind, self.received) {
            (PayoutKind::IcpSwap(terms), Some(_)) => terms.network_fee,
            _ => TokenAmount::ZERO,
        }
    }

    /// The network fee the payout's user paid: an ICP swap's, or a withdrawal's.
    pub fn network_fee(&self) -> TokenAmount {
        match &self.kind {
            PayoutKind::IcpSwap(terms) => terms.network_fee,
            PayoutKind::Withdrawal(withdrawal) => withdrawal.network_fee,
        }
    }

    pub fn is_icp_swap(&self) -> bool {
        matches!(self.kind, PayoutKind::IcpSwap(_))
    }
}

/// What a Base payout's steps that name a ref (its ICPSwap legs) name it by: a fixed tag and
/// its id. No deposit's ref or operation's id is one: those are keccak hashes.
pub fn evm_payout_ref(id: u64) -> PoolRef {
    let mut bytes = [0; 32];
    bytes[..24].copy_from_slice(EVM_PAYOUT_TAG);
    bytes[24..].copy_from_slice(&id.to_be_bytes());
    PoolRef::new(bytes)
}

/// The Base payout a ref names, if it names one.
pub fn evm_payout_id(r#ref: &PoolRef) -> Option<u64> {
    let bytes = r#ref.as_ref();
    (bytes[..24] == *EVM_PAYOUT_TAG)
        .then(|| u64::from_be_bytes(bytes[24..].try_into().expect("BUG: eight bytes")))
}

const EVM_PAYOUT_TAG: &[u8; 24] = b"swapic.pool.v1.evm.pay\0\0";

/// The payouts' knobs (interface section 3b).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct PayoutSettings {
    /// The least network fee a payout may carry, micro USDC.
    #[n(0)]
    pub min_network_fee: TokenAmount,
    /// How long a waiting payout waits for a valid order before it is listed again.
    #[n(2)]
    pub order_wait: Duration,
    /// The most gas a payout order may be signed with on Base.
    #[n(3)]
    pub max_payout_gas: u64,
    /// Whether a payout may leave Base over CCTP. Its least network fee is derived per
    /// destination chain from the chains' own least fees ([`cctp_payout_floor`]).
    #[n(4)]
    pub cctp_payouts: bool,
}

/// The least network fee a CCTP payout from the pool chain to a destination carries: the
/// pool chain's least fee for an order that burns on CCTP (`relay_cctp`), plus the
/// destination's least fee for the canister's own finish there (`complete_swap` for a token
/// out, `relay_deliver` for USDC out). None when the sum leaves 256 bits.
pub fn cctp_payout_floor(
    pool_chain_relay_cctp: TokenAmount,
    destination_finish: TokenAmount,
) -> Option<TokenAmount> {
    pool_chain_relay_cctp.checked_add(destination_finish)
}

/// The least gas any transaction has.
pub const MIN_PAYOUT_GAS: u64 = 21_000;
/// The most gas the settings may let a payout order take: a Base block's worth is far above.
pub const MAX_PAYOUT_GAS: u64 = 10_000_000;
/// The longest a waiting payout may wait for an order before it is listed again.
pub const MAX_ORDER_WAIT: Duration = Duration::from_secs(3_600);

impl Default for PayoutSettings {
    /// The launch values: 5 cents of network fee at the least, two minutes for an order, two
    /// million gas, CCTP payouts off until the chains' least fees are set.
    fn default() -> Self {
        Self {
            min_network_fee: TokenAmount::new(50_000),
            order_wait: Duration::from_secs(120),
            max_payout_gas: 2_000_000,
            cctp_payouts: false,
        }
    }
}

/// Why payout settings are not ones the pool runs on.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PayoutSettingsError {
    #[error("order_wait_s must be between 1 and {}", MAX_ORDER_WAIT.as_secs())]
    OrderWait,
    #[error("max_payout_gas must be between {MIN_PAYOUT_GAS} and {MAX_PAYOUT_GAS}")]
    Gas,
}

impl PayoutSettings {
    /// Whether a CCTP payout of `network_fee` is one the user paid the canister's finish
    /// for: only while CCTP payouts are on, and at `floor` or above (the destination's
    /// [`cctp_payout_floor`]).
    pub fn admits_cctp(&self, network_fee: TokenAmount, floor: TokenAmount) -> Result<(), String> {
        if !self.cctp_payouts {
            return Err("no CCTP payout is signed: CCTP payouts are off".to_string());
        }
        if network_fee < floor {
            return Err(format!(
                "a CCTP payout's network fee must be at least {floor}, not {network_fee}"
            ));
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), PayoutSettingsError> {
        if self.order_wait.is_zero() || self.order_wait > MAX_ORDER_WAIT {
            return Err(PayoutSettingsError::OrderWait);
        }
        if !(MIN_PAYOUT_GAS..=MAX_PAYOUT_GAS).contains(&self.max_payout_gas) {
            return Err(PayoutSettingsError::Gas);
        }
        Ok(())
    }
}

/// The pool's lines for its Base payouts, in the pool ledger (absent until the first one).
#[derive(Clone, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct PayoutLines {
    #[n(0)]
    pub settings: Option<PayoutSettings>,
    /// The id the next payout opens with.
    #[n(1)]
    pub next_id: u64,
    /// The ICP swaps' network fees: ckUSDC at the canister, not the stakers'.
    #[n(2)]
    pub fees_icp: TokenAmount,
    /// The withdrawals' network fees: USDC at the canister's Base address, not the stakers'.
    #[n(3)]
    pub fees_base: TokenAmount,
    /// What the queued payouts will hold of the Base pocket.
    #[n(4)]
    pub queued: TokenAmount,
    /// How many payouts are queued.
    #[n(5)]
    pub queue_len: u64,
    /// The place the next queued payout takes.
    #[n(6)]
    pub next_seq: u64,
    /// The payouts not paid, undone or stopped: what the timer and `payouts_waiting` read.
    #[n(7)]
    pub open: BTreeSet<u64>,
    /// When the queue's head became its head.
    #[n(8)]
    pub head_since: Option<Timestamp>,
}

/// A withdrawal nonce a staker used: the staker's address and the nonce, big endian, so a
/// stable set orders them by staker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
pub struct NonceUse {
    #[n(0)]
    pub staker: EvmAddress,
    #[n(1)]
    pub nonce: u64,
}

impl NonceUse {
    const BYTES: usize = 20 + 8;
}

impl Storable for NonceUse {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        let mut out = Vec::with_capacity(Self::BYTES);
        out.extend_from_slice(self.staker.as_bytes());
        out.extend_from_slice(&self.nonce.to_be_bytes());
        Cow::Owned(out)
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        let bytes = bytes.as_ref();
        assert_eq!(
            bytes.len(),
            Self::BYTES,
            "BUG: a stored nonce use is 28 bytes"
        );
        let staker: [u8; 20] = bytes[..20].try_into().expect("BUG: twenty bytes");
        let nonce = u64::from_be_bytes(bytes[20..].try_into().expect("BUG: eight bytes"));
        Self {
            staker: EvmAddress::new(staker),
            nonce,
        }
    }

    const BOUND: Bound = Bound::Bounded {
        max_size: Self::BYTES as u32,
        is_fixed_size: true,
    };
}

/// An orphan: USDC at the canister's Base address that no deposit could take, held for a
/// controller to refund to its payer or book (nothing automatic).
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Orphan {
    #[n(0)]
    pub amount: TokenAmount,
    #[n(1)]
    pub block: BlockNumber,
    /// Who paid it, where the chain showed it.
    #[n(2)]
    pub payer: Option<EvmAddress>,
    #[n(3)]
    pub why: String,
    #[n(4)]
    pub status: OrphanStatus,
}

crate::storable_as_cbor!(Orphan);

/// Where an orphan stands.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum OrphanStatus {
    /// Held for a controller.
    #[n(0)]
    Held,
    /// Its refund was signed: `sent` to `to`, the rest kept for the transfer's gas.
    #[n(1)]
    Refunding {
        #[n(0)]
        to: EvmAddress,
        #[n(1)]
        sent: TokenAmount,
    },
    #[n(2)]
    Refunded {
        #[n(0)]
        to: EvmAddress,
        #[n(1)]
        sent: TokenAmount,
    },
    /// Booked to the Base books' unattributed line.
    #[n(3)]
    Booked,
    /// A deposit's transfer the canister's own read of Base found without its terms (
    /// nothing is registered): credited to the deposit under `r#ref` when anyone hands the
    /// terms in, held for a controller once `until` passes.
    #[n(4)]
    AwaitingTerms {
        #[n(0)]
        r#ref: PoolRef,
        #[n(1)]
        until: Timestamp,
    },
    /// Its terms came: its deposit's arrival line takes it next, in the same message.
    #[n(5)]
    Claimed,
    /// Credited to its deposit.
    #[n(6)]
    Credited,
}

impl OrphanStatus {
    /// Whether a controller may resolve it at `now`: held, or awaiting terms past its time.
    pub fn resolvable(&self, now: Timestamp) -> bool {
        match self {
            OrphanStatus::Held => true,
            OrphanStatus::AwaitingTerms { until, .. } => now > *until,
            _ => false,
        }
    }
}

/// The key an orphan's refund transaction is known by: the hash of its credit key's bytes.
pub fn orphan_key(credit: &super::CreditKey) -> [u8; 32] {
    use ic_stable_structures::Storable;
    alloy_primitives::keccak256(credit.to_bytes()).0
}
