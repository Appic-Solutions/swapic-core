#[cfg(test)]
mod tests;

use crate::address::{Address, RpcUrl};
use crate::chain::ChainId;
use crate::evm::EvmAddress;
use crate::numeric::{BasisPoints, BlockDepth, GasAmount, TokenAmount, UsdAmount, Wei};
use crate::quote::MAX_QUOTE_LIFETIME;
use crate::rail::{CctpDomain, CctpMinFee, MIN_FEE_MULTIPLIER};
use minicbor::data::Type;
use minicbor::{Decode, Decoder, Encode, Encoder};
use std::collections::BTreeMap;
use std::time::Duration;
use thiserror::Error;

/// The longest a timer interval may be: one year. Nothing legitimate waits longer, and a
/// tight bound fails at set time rather than when the timer is wired.
pub const MAX_TIMER_INTERVAL: Duration = Duration::from_secs(31_536_000);

/// The shortest the expiry sweep may run: ten seconds. A pass reads stable memory and
/// appends, and the work it has to do does not arrive faster than this, so anything shorter
/// burns cycles on passes with nothing to do.
pub const MIN_EXPIRY_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// The longest a window inside one swap may be: one day. Every knob bounded by this one
/// measures a wait a user or a chain is in the middle of, so anything longer is a typo, and
/// a duration near `u64::MAX` overflows the deadline it is added to and never closes.
pub const MAX_SHORT_DURATION: Duration = Duration::from_secs(86_400);

/// The longest a batching window may be: one minute. A window is how long the canister holds
/// work back to send it together, which nobody gains by measuring in hours.
pub const MAX_BATCH_WINDOW: Duration = Duration::from_secs(60);

/// The most items one batch may send. A batch of nothing sends nothing, and an unbounded
/// one is the trap every cap here exists to prevent.
pub const MAX_BATCH_ITEMS: u32 = 1_000;

/// A cap on the work one timer pass may do, carrying its default and its ceiling in the
/// type, so no caller can invent a third pair.
///
/// A cap holding its default is absent from the stored config: the stored layout is
/// append-only, so a config written before a cap existed reads back as that cap's default.
/// Changing a `DEFAULT` therefore changes what every config that omitted the knob reads as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cap<const DEFAULT: u32, const CEILING: u32>(u32);

/// The most refunds one expiry pass starts. A pass that has to append is the expensive one,
/// so this is the tightest of the three caps.
pub type RefundsPerSweep = Cap<50, 500>;

/// The most pending quotes one expiry pass evicts. An eviction is a stable map removal and
/// no append, so a pass affords more of them than of refunds.
pub type EvictionsPerSweep = Cap<200, 5_000>;

/// The most log entries one audit pass verifies. Each entry is read and rehashed, so the
/// ceiling is what a timer message affords with room to spare.
pub type AuditChunk = Cap<1_000, 10_000>;

/// The most blocks one `eth_getLogs` of the deposit read asks for: ten thousand, the range
/// most providers serve in one call. A wider range is read in windows of this many blocks.
pub const LOGS_WINDOW_BLOCKS: u64 = 10_000;

/// The most windows one deposit read walks: forty, four hundred thousand blocks, which is
/// above a day on the fastest chain this canister reads (Arbitrum, four blocks a second).
/// A range wider than that is refused by name before any outcall, never walked for minutes
/// and never cut short in silence.
pub const MAX_LOGS_WINDOWS: u64 = 40;

/// The widest range the deposit read walks, [`MAX_LOGS_WINDOWS`] windows of
/// [`LOGS_WINDOW_BLOCKS`], and so the most a lookback may be: spelled from the two, so the
/// knob's ceiling cannot drift from what the read can cover.
pub const MAX_LOOKBACK_BLOCKS: u32 = {
    let blocks = MAX_LOGS_WINDOWS * LOGS_WINDOW_BLOCKS;
    assert!(blocks <= u32::MAX as u64, "a lookback is a u32 of blocks");
    blocks as u32
};

/// How many blocks back from a chain's head the deposit read looks for a user's deposit.
///
/// A quote is claimable for its lifetime (up to a day) plus the permit window and the
/// grace, so the deposit a claim looks for can be more than a day old: a canister halted
/// for an hour, a quoter outage, or a user who deposits late must not put the funds out of
/// reach. `Config::validate` holds the lookback to that whole span at the fastest chain's
/// rate ([`FASTEST_BLOCKS_PER_SECOND`], Arbitrum's), which covers it on every slower chain
/// many times over. The default is the ceiling, [`MAX_LOOKBACK_BLOCKS`], the widest range
/// the read walks: 100,000 seconds of Arbitrum, room for the longest quote, the permit
/// window and a grace of up to about three and three quarter hours (a lookback above the ceiling
/// would have every read refused as too wide, every claim and every arrival check with
/// it). It is read in windows of [`LOGS_WINDOW_BLOCKS`] and a claim for a quote the store
/// still holds starts a margin below that quote's registration block instead, so the width
/// costs outcalls only where no registration height is known or nothing turns up above it.
///
/// Raised for fix wave 6 (M1) from 345,600, a day of Arbitrum, which fell short of a
/// day's quote claimed at its deadline. A stored config at the old default wrote nothing
/// for the knob, so it reads back as the new one.
pub type DepositLookback = Cap<MAX_LOOKBACK_BLOCKS, MAX_LOOKBACK_BLOCKS>;

/// The most blocks a second any chain this canister reads makes: four, Arbitrum's, a block
/// every 250 milliseconds. A span of time is at most this many blocks a second on every
/// chain, so a lookback that covers a span at this rate covers it everywhere.
pub const FASTEST_BLOCKS_PER_SECOND: u64 = 4;

/// The least deep money arriving at the canister on Ethereum mainnet may be credited at: a
/// deposit, a ck minter withdrawal's USDC, the books' balance read. A one-block reorg
/// there is routine, and crediting below this would credit money that then leaves the
/// chain, which no later step takes back. A swap is acted on at its own depth, 1 block on
/// Ethereum too (Jeff, 2026-10-17): that depth closes the canister's own transactions in
/// the outbox, and credits no money that arrived. The other chains' depths are the
/// deploy's to size; this one bounds `credit_confirmations` at the door, and crediting
/// reads at least this on Ethereum whatever the config holds.
pub const MIN_ETHEREUM_CONFIRMATIONS: BlockDepth = BlockDepth::new(12);

impl<const DEFAULT: u32, const CEILING: u32> Cap<DEFAULT, CEILING> {
    /// What a config that never set the knob reads as.
    pub const DEFAULT: Self = Self(DEFAULT);
    /// The largest cap this knob accepts.
    pub const CEILING: u32 = CEILING;

    pub const fn new(items: u32) -> Self {
        Self(items)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// The cap as a `take` bound. A u32 fits every usize this canister runs on.
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }

    /// At least one item per pass, at most the type's ceiling: a zero cap makes no progress
    /// at all, and an unbounded one is the trap the cap exists to prevent.
    pub fn validate(self, field: &'static str) -> Result<(), ConfigError> {
        if self.0 == 0 || self.0 > CEILING {
            return Err(ConfigError::CapOutOfRange {
                field,
                cap: self.0,
                ceiling: CEILING,
            });
        }
        Ok(())
    }
}

impl<const DEFAULT: u32, const CEILING: u32> Default for Cap<DEFAULT, CEILING> {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Stored as one unsigned integer, and as nothing at all when it holds the default.
impl<C, const DEFAULT: u32, const CEILING: u32> Encode<C> for Cap<DEFAULT, CEILING> {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.u32(self.0)?;
        Ok(())
    }

    fn is_nil(&self) -> bool {
        self.0 == DEFAULT
    }
}

impl<'b, C, const DEFAULT: u32, const CEILING: u32> Decode<'b, C> for Cap<DEFAULT, CEILING> {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        // a null placeholder is how a record written with the knob absent in the middle
        // reads back, and it means the same as the knob missing off the end
        if matches!(d.datatype()?, Type::Null | Type::Undefined) {
            d.skip()?;
            return Ok(Self::DEFAULT);
        }
        Ok(Self(d.u32()?))
    }

    fn nil() -> Option<Self> {
        Some(Self::DEFAULT)
    }
}

/// How long after a quote's deposit deadline a claim for a deposit that landed by it is
/// still admitted, whoever asks, the controller included.
///
/// The deposit deadline is the quote's expiry plus the permit window, and it is judged by
/// the deposit's own block time. The claim is asked by the services, which can be slow or
/// down: without a grace a watcher that claims late would find even the controller
/// refused a few minutes after the quote, with the user's funds in the vault. The pending
/// store keeps the quote until the grace ends, so the claim still finds it registered.
///
/// At least ten minutes, so a watcher restart fits inside it, and at most a day, the
/// bound every window inside one swap is held to. An hour by default. Absent from the
/// stored config at its default, so every config written before it existed reads back
/// with the default and the golden lines do not move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClaimGrace(Duration);

/// The least a claim's grace may be: ten minutes.
pub const MIN_CLAIM_GRACE: Duration = Duration::from_secs(600);

impl ClaimGrace {
    /// What a config that never set the knob reads as: an hour.
    pub const DEFAULT: Self = Self(Duration::from_secs(3_600));
    /// The shortest grace the knob accepts.
    pub const FLOOR: Duration = MIN_CLAIM_GRACE;
    /// The longest grace the knob accepts.
    pub const CEILING: Duration = MAX_SHORT_DURATION;

    pub const fn new(grace: Duration) -> Self {
        Self(grace)
    }

    pub const fn get(self) -> Duration {
        self.0
    }

    /// Between [`Self::FLOOR`] and [`Self::CEILING`], both included.
    pub fn validate(self) -> Result<(), ConfigError> {
        let field = "claim_grace_s";
        if self.0 < Self::FLOOR {
            return Err(ConfigError::DurationBelowFloor {
                field,
                duration: self.0,
                floor: Self::FLOOR,
            });
        }
        if self.0 > Self::CEILING {
            return Err(ConfigError::DurationAboveCap {
                field,
                duration: self.0,
                cap: Self::CEILING,
            });
        }
        Ok(())
    }
}

impl Default for ClaimGrace {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Stored as a duration, and as nothing at all when it holds the default.
impl<C> Encode<C> for ClaimGrace {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        self.0.encode(e, ctx)
    }

    fn is_nil(&self) -> bool {
        *self == Self::DEFAULT
    }
}

impl<'b, C> Decode<'b, C> for ClaimGrace {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        // a null placeholder is how a record written with the knob absent in the middle
        // reads back, and it means the same as the knob missing off the end
        if matches!(d.datatype()?, Type::Null | Type::Undefined) {
            d.skip()?;
            return Ok(Self::DEFAULT);
        }
        Duration::decode(d, ctx).map(Self)
    }

    fn nil() -> Option<Self> {
        Some(Self::DEFAULT)
    }
}

/// Whether the Eco rail may be used at all. Off until its route is designed (see
/// `impl/src/rails/eco`), and off is what a config that never set it reads as, so it
/// writes nothing of its own while it is off and the stored bytes of every config written
/// before it existed are unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EcoEnabled(bool);

impl EcoEnabled {
    /// The Eco rail turned on, which a deploy does only once the route is designed.
    pub const ON: Self = Self(true);
    /// The default: no quote on the Eco rail is claimed or executed.
    pub const OFF: Self = Self(false);

    pub const fn new(on: bool) -> Self {
        Self(on)
    }

    pub const fn is_on(self) -> bool {
        self.0
    }
}

/// Stored as one bool, and as nothing at all while it is off.
impl<C> Encode<C> for EcoEnabled {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.bool(self.0)?;
        Ok(())
    }

    fn is_nil(&self) -> bool {
        !self.0
    }
}

impl<'b, C> Decode<'b, C> for EcoEnabled {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        // a null placeholder is how a record written with the knob absent in the middle
        // reads back, and it means the same as the knob missing off the end
        if matches!(d.datatype()?, Type::Null | Type::Undefined) {
            d.skip()?;
            return Ok(Self::OFF);
        }
        Ok(Self(d.bool()?))
    }

    fn nil() -> Option<Self> {
        Some(Self::OFF)
    }
}

/// A per-chain table of deploy-time facts. Absent from storage while empty, so a config
/// written before the table existed reads back with it empty, and one that never fills it
/// keeps the bytes the golden file pins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainTable<V>(pub BTreeMap<ChainId, V>);

impl<V: Copy> ChainTable<V> {
    pub fn get(&self, chain_id: ChainId) -> Option<V> {
        self.0.get(&chain_id).copied()
    }
}

// by hand: the derive would ask `V: Default`, and a table of values that have no default
// is still empty by default
impl<V> Default for ChainTable<V> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}

/// Stored as the map, and as nothing at all when it is empty.
impl<C, V: Encode<C>> Encode<C> for ChainTable<V> {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        self.0.encode(e, ctx)
    }

    fn is_nil(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'b, C, V: Decode<'b, C>> Decode<'b, C> for ChainTable<V> {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        // a null placeholder is how a record written with the table absent in the middle
        // reads back, and it means the same as the table missing off the end
        if matches!(d.datatype()?, Type::Null | Type::Undefined) {
            d.skip()?;
            return Ok(Self::default());
        }
        BTreeMap::decode(d, ctx).map(Self)
    }

    fn nil() -> Option<Self> {
        Some(Self::default())
    }
}

/// Every knob the canister reads at runtime. `Debug` is safe to log: [`RpcUrl`] prints
/// as `***`.
///
/// Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
/// new field is optional.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Config {
    /// The fee the platform charges, at most `max_fee`.
    #[n(0)]
    pub platform_fee: BasisPoints,
    /// The hard ceiling the canister holds its own fee to.
    #[n(1)]
    pub max_fee: BasisPoints,
    /// The largest swap the canister accepts.
    #[n(2)]
    pub max_swap: UsdAmount,
    #[n(3)]
    pub quote_ttl: Duration,
    /// How long a permit signed against a quote stays valid past the quote's expiry.
    #[n(4)]
    pub permit_deadline: Duration,
    #[n(5)]
    pub chain_data_max_age: Duration,
    #[n(6)]
    pub batch_window: Duration,
    /// The most items one batch sends, 1 to [`MAX_BATCH_ITEMS`].
    #[n(7)]
    pub max_batch_items: u32,
    /// How long a paused swap waits for its user.
    #[n(8)]
    pub decision_timeout: Duration,
    #[n(9)]
    pub rail_status_max_age: Duration,
    /// Whether the canister simulates a transaction itself before it signs it. Off by
    /// default: the watcher and the submit API simulate `runFor` and every CCTP finish
    /// with an `eth_call` from the canister's address before they ask, and the canister's
    /// own simulation would be one more unreplicated outcall per signature.
    #[n(10)]
    pub simulate_before_sign: bool,
    #[n(11)]
    pub expiry_check_interval: Duration,
    #[n(12)]
    pub replay_audit_interval: Duration,
    /// The swap depth per chain: how deep the canister's own transactions must be before
    /// the outbox takes them as final. Crediting reads it too for a chain
    /// `credit_confirmations` has no entry for.
    #[n(13)]
    pub confirmations: BTreeMap<ChainId, BlockDepth>,
    /// Secrets: each url carries its provider's api key.
    #[n(14)]
    pub rpc_urls: BTreeMap<ChainId, RpcUrl>,
    #[n(15)]
    pub vault_addresses: BTreeMap<ChainId, Address>,
    /// The name of the threshold ECDSA key the canister signs with.
    #[n(16)]
    pub ecdsa_key_name: String,
    /// The most refunds one expiry pass starts, so a short `decision_timeout` cannot put
    /// every timed-out swap in one message.
    #[n(17)]
    pub max_refunds_per_sweep: RefundsPerSweep,
    /// The most pending quotes one expiry pass evicts, so a full store cannot put every
    /// eviction in one message.
    #[n(18)]
    pub max_evictions_per_sweep: EvictionsPerSweep,
    /// The most log entries one audit pass verifies, so the chain audit costs the same
    /// however long the log grows.
    #[n(19)]
    pub audit_chunk_events: AuditChunk,
    /// How many blocks back from the head the deposit read looks for a user's deposit.
    #[n(20)]
    pub deposit_lookback_blocks: DepositLookback,
    /// CCTP's domain id for each chain a burn may leave from or arrive on.
    #[n(21)]
    pub cctp_domains: ChainTable<CctpDomain>,
    /// The USDC contract on each chain: the token the rails carry.
    #[n(22)]
    pub usdc_addresses: ChainTable<EvmAddress>,
    // #[n(23)] and #[n(24)] held one `TokenMessengerV2` and one `MessageTransmitterV2` for
    // every chain, which `evm_chains` replaced per chain before any deploy: retired, never
    // reused
    /// Eco's `Portal`, one address on every chain Eco deploys to.
    #[n(25)]
    pub eco_portal: Option<EvmAddress>,
    /// Whether the Eco rail may be used. Off until its route is designed: a quote naming
    /// it is refused at registration and at the claim, and a swap already on it is paused
    /// until the rail is on again (the `paused_swaps` query counts them).
    #[n(26)]
    pub eco_enabled: EcoEnabled,
    /// How long after a quote's deposit deadline a claim for a deposit that landed by it
    /// is still admitted.
    #[n(27)]
    pub claim_grace: ClaimGrace,
    /// The minimum fee each chain's `TokenMessengerV2` holds a burn from it to, as Circle
    /// stores it (`minFee`, thousandths of a basis point). A chain with no entry is one
    /// whose messenger charges none. The staging canary reads `minFee()` on every chain
    /// and fills it.
    #[n(28)]
    pub cctp_min_fees: ChainTable<CctpMinFee>,
    // #[n(29)] and #[n(30)] held the P10 per-action gas budgets and minimum fees, which
    // `evm_chains` replaced before any deploy: retired, never reused
    #[n(31)]
    pub books_check_interval: BooksInterval,
    /// How far below what the books expect a balance may read before the pool pauses.
    #[n(32)]
    pub books_tolerance: BooksTolerance,
    /// Every EVM chain the canister signs on, design C: the Swapic deployment, CCTP v2,
    /// USDC, and per action the gas limit, the gas budget and the least fee. A chain with
    /// no entry signs nothing.
    #[n(33)]
    pub evm_chains: ChainTable<EvmChain>,
    /// The chain the pool's EVM pocket is on, whose books the pool's guard reads. None:
    /// the pool is closed.
    #[n(34)]
    pub pool_chain: Option<ChainId>,
    /// The crediting depth per chain: how deep money arriving at the canister (a deposit,
    /// a ck minter withdrawal's USDC, the books' balance read) must be before it is
    /// credited. A chain with no entry credits at its swap depth, and Ethereum never below
    /// [`MIN_ETHEREUM_CONFIRMATIONS`].
    #[n(35)]
    pub credit_confirmations: ChainTable<BlockDepth>,
}

/// The kinds of transaction the canister signs and pays the gas of: the relay of a
/// gasless order, by its rail, and the four ways a CCTP arrival is finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum GasAction {
    /// `Swapic.runFor` of an order that burns on CCTP.
    #[n(0)]
    RelayCctp,
    /// `Swapic.runFor` of an order that publishes an Eco intent.
    #[n(1)]
    RelayEco,
    /// `Swapic.runFor` of an order that stays on its chain.
    #[n(2)]
    RelayLocal,
    /// `CctpReceiver.completeCctp` of a Swap.
    #[n(3)]
    CompleteSwap,
    /// `CctpReceiver.completeCctp` of a CkDeposit.
    #[n(4)]
    CompleteCk,
    /// `MessageTransmitterV2.receiveMessage` of a Deliver.
    #[n(5)]
    RelayDeliver,
    /// `CctpReceiver.release` after the envelope's deadline.
    #[n(6)]
    Release,
    /// `Swapic.run` of a pool payout from the canister's own Base pocket.
    #[n(7)]
    PoolPayout,
    /// `USDC.transfer` of an orphan back to its payer, from the canister's Base address.
    #[n(8)]
    PoolTransfer,
    /// `USDC.approve(Swapic, max)` from the canister's Base address, once per deploy.
    #[n(9)]
    PoolApprove,
    /// `Swapic.run` of a rebalance move's own order, from the canister's address (P11c).
    #[n(10)]
    PoolMove,
    /// The CCTP finish of a rebalance move on its destination, the canister's own: Ethereum's
    /// `completeCctp` with a CkDeposit, Base's `receiveMessage` (P11c).
    #[n(11)]
    PoolMoveFinish,
    /// The ck helper's `depositErc20` for the canister's own principal and rebalance
    /// subaccount on Ethereum, after a Base to ICP move's Eco fill (P11c).
    #[n(12)]
    PoolCkDeposit,
}

impl GasAction {
    /// Every action, in code order.
    pub const ALL: [GasAction; 13] = [
        GasAction::RelayCctp,
        GasAction::RelayEco,
        GasAction::RelayLocal,
        GasAction::CompleteSwap,
        GasAction::CompleteCk,
        GasAction::RelayDeliver,
        GasAction::Release,
        GasAction::PoolPayout,
        GasAction::PoolTransfer,
        GasAction::PoolApprove,
        GasAction::PoolMove,
        GasAction::PoolMoveFinish,
        GasAction::PoolCkDeposit,
    ];

    /// The action's one byte in an event's canonical preimage: its index, never reused.
    pub fn code(self) -> u8 {
        match self {
            GasAction::RelayCctp => 0,
            GasAction::RelayEco => 1,
            GasAction::RelayLocal => 2,
            GasAction::CompleteSwap => 3,
            GasAction::CompleteCk => 4,
            GasAction::RelayDeliver => 5,
            GasAction::Release => 6,
            GasAction::PoolPayout => 7,
            GasAction::PoolTransfer => 8,
            GasAction::PoolApprove => 9,
            GasAction::PoolMove => 10,
            GasAction::PoolMoveFinish => 11,
            GasAction::PoolCkDeposit => 12,
        }
    }

    /// The action as the wire spells it.
    pub fn name(self) -> &'static str {
        match self {
            GasAction::RelayCctp => "relay_cctp",
            GasAction::RelayEco => "relay_eco",
            GasAction::RelayLocal => "relay_local",
            GasAction::CompleteSwap => "complete_swap",
            GasAction::CompleteCk => "complete_ck",
            GasAction::RelayDeliver => "relay_deliver",
            GasAction::Release => "release",
            GasAction::PoolPayout => "pool_payout",
            GasAction::PoolTransfer => "pool_transfer",
            GasAction::PoolApprove => "pool_approve",
            GasAction::PoolMove => "pool_move",
            GasAction::PoolMoveFinish => "pool_move_finish",
            GasAction::PoolCkDeposit => "pool_ck_deposit",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.name() == name)
    }
}

/// One value per [`GasAction`]. The pool's three rows (P11b) are optional, so a config
/// written before them reads back with them absent, and an action whose row is absent is
/// refused at use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Encode, Decode)]
pub struct PerAction<T> {
    #[n(0)]
    pub relay_cctp: T,
    #[n(1)]
    pub relay_eco: T,
    #[n(2)]
    pub relay_local: T,
    #[n(3)]
    pub complete_swap: T,
    #[n(4)]
    pub complete_ck: T,
    #[n(5)]
    pub relay_deliver: T,
    #[n(6)]
    pub release: T,
    #[n(7)]
    pub pool_payout: Option<T>,
    #[n(8)]
    pub pool_transfer: Option<T>,
    #[n(9)]
    pub pool_approve: Option<T>,
    /// The rebalancing's rows (P11c), optional as the pool's.
    #[n(10)]
    pub pool_move: Option<T>,
    #[n(11)]
    pub pool_move_finish: Option<T>,
    #[n(12)]
    pub pool_ck_deposit: Option<T>,
}

impl<T: Copy> PerAction<T> {
    /// The action's row; None for a pool row the config does not set.
    pub fn get(&self, action: GasAction) -> Option<T> {
        match action {
            GasAction::RelayCctp => Some(self.relay_cctp),
            GasAction::RelayEco => Some(self.relay_eco),
            GasAction::RelayLocal => Some(self.relay_local),
            GasAction::CompleteSwap => Some(self.complete_swap),
            GasAction::CompleteCk => Some(self.complete_ck),
            GasAction::RelayDeliver => Some(self.relay_deliver),
            GasAction::Release => Some(self.release),
            GasAction::PoolPayout => self.pool_payout,
            GasAction::PoolTransfer => self.pool_transfer,
            GasAction::PoolApprove => self.pool_approve,
            GasAction::PoolMove => self.pool_move,
            GasAction::PoolMoveFinish => self.pool_move_finish,
            GasAction::PoolCkDeposit => self.pool_ck_deposit,
        }
    }

    /// The same value for every action, the pool's rows included.
    pub fn all(value: T) -> Self {
        Self {
            relay_cctp: value,
            relay_eco: value,
            relay_local: value,
            complete_swap: value,
            complete_ck: value,
            relay_deliver: value,
            release: value,
            pool_payout: Some(value),
            pool_transfer: Some(value),
            pool_approve: Some(value),
            pool_move: Some(value),
            pool_move_finish: Some(value),
            pool_ck_deposit: Some(value),
        }
    }
}

impl PerAction<Wei> {
    /// The least of the rows set: what a transaction whose action is not known is held to.
    pub fn strictest(&self) -> Wei {
        GasAction::ALL
            .into_iter()
            .filter_map(|action| self.get(action))
            .min()
            .expect("BUG: the seven relay rows are always set")
    }
}

/// One EVM chain as design C runs on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct EvmChain {
    /// The door users approve, and the EIP-712 verifying contract of every order here.
    #[n(0)]
    pub swapic: EvmAddress,
    /// The one shared Runner: it makes this chain's burns.
    #[n(1)]
    pub runner: EvmAddress,
    /// The mint recipient and destination caller of every Swap and CkDeposit arriving here.
    #[n(2)]
    pub cctp_receiver: EvmAddress,
    #[n(3)]
    pub usdc: EvmAddress,
    /// CCTP v2's `TokenMessengerV2`.
    #[n(4)]
    pub token_messenger: EvmAddress,
    /// CCTP v2's `MessageTransmitterV2`.
    #[n(5)]
    pub message_transmitter: EvmAddress,
    /// The most gas the canister gives each action's transaction: a larger limit asked
    /// for is signed at this one.
    #[n(6)]
    pub gas_limit: PerAction<GasAmount>,
    /// The most each action's transaction may cost in wei, its gas limit times its fee
    /// cap: the gas rule.
    #[n(7)]
    pub gas_budget: PerAction<Wei>,
    /// The least fee step, in USDC units, each action needs: what the quoter charges for
    /// it at the least.
    #[n(8)]
    pub min_network_fee: PerAction<TokenAmount>,
}

/// How often the books read the canister's balances: ten minutes unless set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BooksInterval(pub Duration);

impl BooksInterval {
    pub const DEFAULT: Self = Self(Duration::from_secs(600));
}

impl Default for BooksInterval {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl<C> Encode<C> for BooksInterval {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        self.0.encode(e, ctx)
    }

    fn is_nil(&self) -> bool {
        *self == Self::DEFAULT
    }
}

impl<'b, C> Decode<'b, C> for BooksInterval {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        if matches!(d.datatype()?, Type::Null | Type::Undefined) {
            d.skip()?;
            return Ok(Self::DEFAULT);
        }
        Duration::decode(d, ctx).map(Self)
    }

    fn nil() -> Option<Self> {
        Some(Self::DEFAULT)
    }
}

/// A balance shortfall the books overlook: one USDC unless set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BooksTolerance(pub TokenAmount);

impl BooksTolerance {
    pub const DEFAULT: Self = Self(TokenAmount::new(1_000_000));
}

impl Default for BooksTolerance {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl<C> Encode<C> for BooksTolerance {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        self.0.encode(e, ctx)
    }

    fn is_nil(&self) -> bool {
        *self == Self::DEFAULT
    }
}

impl<'b, C> Decode<'b, C> for BooksTolerance {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        if matches!(d.datatype()?, Type::Null | Type::Undefined) {
            d.skip()?;
            return Ok(Self::DEFAULT);
        }
        TokenAmount::decode(d, ctx).map(Self)
    }

    fn nil() -> Option<Self> {
        Some(Self::DEFAULT)
    }
}

/// Why a config was refused, naming the knob as clients know it.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("max_fee_bps is {0}, above the 10000 bps that is 100%")]
    MaxFeeAboveHundredPercent(BasisPoints),
    #[error("platform_fee_bps is {platform_fee}, above max_fee_bps {max_fee}")]
    PlatformFeeAboveMax {
        platform_fee: BasisPoints,
        max_fee: BasisPoints,
    },
    #[error("ecdsa_key_name is empty")]
    EmptyEcdsaKeyName,
    #[error(
        "ecdsa_key_name is {requested}, but this canister has already derived its address \
         under {current}"
    )]
    EcdsaKeyNameFixed { current: String, requested: String },
    #[error("{field} is {}s, above the one-year cap of {}s", interval.as_secs(), MAX_TIMER_INTERVAL.as_secs())]
    TimerIntervalTooLong {
        field: &'static str,
        interval: Duration,
    },
    #[error("{field} is {}s, below the floor of {}s", interval.as_secs(), floor.as_secs())]
    TimerIntervalTooShort {
        field: &'static str,
        interval: Duration,
        floor: Duration,
    },
    // `Duration`'s `Debug` keeps the fraction, so a window of 60.5s is not reported as 60s
    #[error("{field} is {duration:?}, above the cap of {cap:?}")]
    DurationAboveCap {
        field: &'static str,
        duration: Duration,
        cap: Duration,
    },
    #[error("{field} is {duration:?}, below the floor of {floor:?}")]
    DurationBelowFloor {
        field: &'static str,
        duration: Duration,
        floor: Duration,
    },
    #[error("{field} is {cap}, outside the range 1 to {ceiling}")]
    CapOutOfRange {
        field: &'static str,
        cap: u32,
        ceiling: u32,
    },
    #[error(
        "credit_confirmations[{chain}] is {depth}, below the floor of {floor} a reorg on \
         that chain makes necessary"
    )]
    DepthTooShallow {
        chain: ChainId,
        depth: BlockDepth,
        floor: BlockDepth,
    },
    #[error(
        "cctp_min_fees[{chain}] is {min_fee}, not below the {ceiling} that is the whole \
         amount"
    )]
    CctpMinFeeTooHigh {
        chain: ChainId,
        min_fee: CctpMinFee,
        ceiling: u32,
    },
    #[error("{field} does not fit in 256 bits")]
    AmountTooLarge { field: &'static str },
    #[error("{field} does not fit in a duration")]
    DurationTooLong { field: &'static str },
    #[error(
        "rpc_urls[{chain}] is the redacted placeholder \"***\": read with get_config_full, \
         not get_config, before writing"
    )]
    RedactedRpcUrl { chain: ChainId },
    #[error("vault_addresses[{chain}] is {len} bytes, above the cap of 256")]
    VaultAddressTooLong { chain: ChainId, len: usize },
    #[error("vault_addresses[{chain}] is empty")]
    EmptyVaultAddress { chain: ChainId },
    #[error("rpc_urls[{chain}] is empty")]
    EmptyRpcUrl { chain: ChainId },
    #[error("{field}{} is not an EVM address: {reason}", chain.map(|chain| format!("[{chain}]")).unwrap_or_default())]
    NotAnAddress {
        field: &'static str,
        chain: Option<ChainId>,
        reason: crate::evm::EvmAddressError,
    },
    #[error(
        "deposit_lookback_blocks is {lookback}, and a claim may be asked {}s after the \
         deposit it reads for (the longest quote, permit_deadline_s and claim_grace_s), \
         {needed} blocks at {FASTEST_BLOCKS_PER_SECOND} blocks a second: the lookback \
         must be above that",
        span.as_secs()
    )]
    LookbackShorterThanClaims {
        lookback: u32,
        span: Duration,
        needed: u64,
    },
}

/// Which timer intervals a config write moved, so only those timers restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntervalChanges {
    pub expiry: bool,
    pub audit: bool,
    /// The engine's tick, which runs on `rail_status_max_age`.
    pub rail_status: bool,
    pub books: bool,
}

impl Default for Config {
    /// The spec numbers. The address maps and the key name are what a deploy fills in.
    fn default() -> Self {
        Self {
            platform_fee: BasisPoints::new(0),
            max_fee: BasisPoints::new(30),
            max_swap: UsdAmount::new(1_000),
            quote_ttl: Duration::from_secs(45),
            permit_deadline: Duration::from_secs(120),
            chain_data_max_age: Duration::from_secs(10),
            batch_window: Duration::from_millis(2_000),
            max_batch_items: 10,
            decision_timeout: Duration::from_secs(30 * 60),
            rail_status_max_age: Duration::from_secs(30),
            // the watcher and the submit API simulate before they ask
            simulate_before_sign: false,
            expiry_check_interval: Duration::from_secs(60),
            replay_audit_interval: Duration::from_secs(21_600),
            confirmations: BTreeMap::from([
                // a swap on Ethereum is acted on at 1 block (Jeff, 2026-10-17); crediting
                // there never reads below MIN_ETHEREUM_CONFIRMATIONS
                (ChainId::ETHEREUM, BlockDepth::new(1)),
                (ChainId::BASE, BlockDepth::new(1)),
                (ChainId::BSC, BlockDepth::new(1)),
                (ChainId::POLYGON, BlockDepth::new(6)),
                (ChainId::ARBITRUM, BlockDepth::new(1)),
            ]),
            rpc_urls: BTreeMap::new(),
            vault_addresses: BTreeMap::new(),
            // "key_1" in production
            ecdsa_key_name: "dfx_test_key".to_string(),
            max_refunds_per_sweep: RefundsPerSweep::DEFAULT,
            max_evictions_per_sweep: EvictionsPerSweep::DEFAULT,
            audit_chunk_events: AuditChunk::DEFAULT,
            deposit_lookback_blocks: DepositLookback::DEFAULT,
            cctp_domains: ChainTable::default(),
            usdc_addresses: ChainTable::default(),
            eco_portal: None,
            // the Eco route is a later plan's; until then no Eco quote is claimed
            eco_enabled: EcoEnabled::OFF,
            claim_grace: ClaimGrace::DEFAULT,
            // the staging canary reads each chain's `minFee()` and fills this
            cctp_min_fees: ChainTable::default(),
            books_check_interval: BooksInterval::DEFAULT,
            books_tolerance: BooksTolerance::DEFAULT,
            evm_chains: ChainTable::default(),
            pool_chain: None,
            // crediting reads the swap depths, held to the floor on Ethereum
            credit_confirmations: ChainTable::default(),
        }
    }
}

impl Config {
    /// What a stored config must satisfy. The fee relation is record coherence only; the
    /// fee computation holds itself to `max_fee` on its own.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.max_fee.is_valid() {
            return Err(ConfigError::MaxFeeAboveHundredPercent(self.max_fee));
        }
        if self.platform_fee > self.max_fee {
            return Err(ConfigError::PlatformFeeAboveMax {
                platform_fee: self.platform_fee,
                max_fee: self.max_fee,
            });
        }
        if self.ecdsa_key_name.is_empty() {
            return Err(ConfigError::EmptyEcdsaKeyName);
        }
        // a chain listed with no depth at all is refused where money is decided, so the
        // one depth checked here is the one whose shallow value is a mistake on any deploy:
        // Ethereum's crediting depth (its swap depth may be 1, and crediting never reads
        // that below the floor)
        if let Some(depth) = self
            .credit_confirmations
            .get(ChainId::ETHEREUM)
            .filter(|depth| *depth < MIN_ETHEREUM_CONFIRMATIONS)
        {
            return Err(ConfigError::DepthTooShallow {
                chain: ChainId::ETHEREUM,
                depth,
                floor: MIN_ETHEREUM_CONFIRMATIONS,
            });
        }
        for (field, interval) in [
            ("expiry_check_interval_s", self.expiry_check_interval),
            ("replay_audit_interval_s", self.replay_audit_interval),
            ("books_check_interval_s", self.books_check_interval.0),
        ] {
            if interval > MAX_TIMER_INTERVAL {
                return Err(ConfigError::TimerIntervalTooLong { field, interval });
            }
        }
        // the sweep is the one timer with a floor as well as a ceiling: it reads stable
        // memory and appends, and a zero interval is clamped to a second rather than
        // refused, so without this a config could put that pass on every second
        if self.expiry_check_interval < MIN_EXPIRY_CHECK_INTERVAL {
            return Err(ConfigError::TimerIntervalTooShort {
                field: "expiry_check_interval_s",
                interval: self.expiry_check_interval,
                floor: MIN_EXPIRY_CHECK_INTERVAL,
            });
        }
        // every window inside one swap: unbounded, each of these overflows the deadline it
        // is added to, and the comparison that deadline feeds then never fires
        for (field, duration, cap) in [
            ("quote_ttl_s", self.quote_ttl, MAX_SHORT_DURATION),
            (
                "permit_deadline_s",
                self.permit_deadline,
                MAX_SHORT_DURATION,
            ),
            (
                "chain_data_max_age_s",
                self.chain_data_max_age,
                MAX_SHORT_DURATION,
            ),
            (
                "decision_timeout_min",
                self.decision_timeout,
                MAX_SHORT_DURATION,
            ),
            (
                "rail_status_max_age_s",
                self.rail_status_max_age,
                MAX_SHORT_DURATION,
            ),
            ("batch_window_ms", self.batch_window, MAX_BATCH_WINDOW),
        ] {
            if duration > cap {
                return Err(ConfigError::DurationAboveCap {
                    field,
                    duration,
                    cap,
                });
            }
        }
        self.max_refunds_per_sweep
            .validate("max_refunds_per_sweep")?;
        self.max_evictions_per_sweep
            .validate("max_evictions_per_sweep")?;
        self.audit_chunk_events.validate("audit_chunk_events")?;
        self.deposit_lookback_blocks
            .validate("deposit_lookback_blocks")?;
        self.claim_grace.validate()?;
        self.ensure_lookback_reaches_claims()?;
        // Circle's own setter refuses a minimum at or above the whole amount, so a table
        // holding one is a typo, and every burn from that chain would revert on it
        if let Some((chain, min_fee)) = self
            .cctp_min_fees
            .0
            .iter()
            .find(|(_, min_fee)| min_fee.get() >= MIN_FEE_MULTIPLIER)
        {
            return Err(ConfigError::CctpMinFeeTooHigh {
                chain: *chain,
                min_fee: *min_fee,
                ceiling: MIN_FEE_MULTIPLIER,
            });
        }
        // the batch size is a cap like the three above, held to the same rule
        if self.max_batch_items == 0 || self.max_batch_items > MAX_BATCH_ITEMS {
            return Err(ConfigError::CapOutOfRange {
                field: "max_batch_items",
                cap: self.max_batch_items,
                ceiling: MAX_BATCH_ITEMS,
            });
        }
        // a chain listed with nothing behind it is a deploy mistake, not a way to unset it
        if let Some(chain) = self
            .vault_addresses
            .iter()
            .find_map(|(chain, address)| address.as_str().is_empty().then_some(*chain))
        {
            return Err(ConfigError::EmptyVaultAddress { chain });
        }
        if let Some(chain) = self
            .rpc_urls
            .iter()
            .find_map(|(chain, url)| url.expose().is_empty().then_some(*chain))
        {
            return Err(ConfigError::EmptyRpcUrl { chain });
        }
        Ok(())
    }

    /// The lookback has to reach every deposit a claim may still be asked for. A claim is
    /// admitted until the quote's expiry, the permit window and the grace after it, and the
    /// deposit can be as old as the quote's whole lifetime before that expiry (at most
    /// [`MAX_QUOTE_LIFETIME`], which registration holds a quote to). The read never looks
    /// further back than the lookback, and counts the anchor's own block in it, so the
    /// blocks that span holds at [`FASTEST_BLOCKS_PER_SECOND`] must be fewer than the
    /// lookback: otherwise a claim the door admits reads past the deposit and leaves it in
    /// the vault.
    fn ensure_lookback_reaches_claims(&self) -> Result<(), ConfigError> {
        let span = MAX_QUOTE_LIFETIME.saturating_add(self.claim_window());
        let needed = span.as_secs().saturating_mul(FASTEST_BLOCKS_PER_SECOND);
        let lookback = self.deposit_lookback_blocks.get();
        if needed >= u64::from(lookback) {
            return Err(ConfigError::LookbackShorterThanClaims {
                lookback,
                span,
                needed,
            });
        }
        Ok(())
    }

    /// How long after a quote's expiry a claim for it may still be asked: the permit
    /// window, in which the deposit has to land, and the grace after it.
    pub fn claim_window(&self) -> Duration {
        // each is held to a day by `validate`, and a config nobody validated saturates
        // rather than wraps, which only ever holds a quote longer
        self.permit_deadline.saturating_add(self.claim_grace.get())
    }

    /// Which timer intervals `new` moves, one flag per timer.
    pub fn interval_changes(&self, new: &Config) -> IntervalChanges {
        IntervalChanges {
            expiry: self.expiry_check_interval != new.expiry_check_interval,
            audit: self.replay_audit_interval != new.replay_audit_interval,
            rail_status: self.rail_status_max_age != new.rail_status_max_age,
            books: self.books_check_interval != new.books_check_interval,
        }
    }
}

crate::storable_as_cbor!(Config);
