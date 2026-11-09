#[cfg(test)]
mod tests;

pub mod usdc_domain;

pub use usdc_domain::{DomainText, NotDomainText, DEFAULT_USDC_NAME, DEFAULT_USDC_VERSION};

use crate::address::RpcUrl;
use crate::cctp::{CctpDomain, CctpMinFee, MIN_FEE_MULTIPLIER};
use crate::chain::ChainId;
use crate::evm::EvmAddress;
use crate::numeric::{BlockDepth, GasAmount, TokenAmount, Wei};
use minicbor::data::Type;
use minicbor::{Decode, Decoder, Encode, Encoder};
use std::collections::BTreeMap;
use std::time::Duration;
use thiserror::Error;

/// The longest a timer interval may be: one year. Nothing legitimate waits longer, and a
/// tight bound fails at set time rather than when the timer is wired.
pub const MAX_TIMER_INTERVAL: Duration = Duration::from_secs(31_536_000);

/// The longest the engine's backstop waits between two drives while work is open, whatever
/// `backstop_interval` says.
pub const ENGINE_EVERY_MAX: Duration = Duration::from_secs(60);

/// The least any repeating timer waits between two runs: a zero interval would run on every
/// round of the subnet and burn cycles for nothing.
pub const TIMER_FLOOR: Duration = Duration::from_secs(1);

/// The longest a batching window may be: one minute. A window is how long the canister holds
/// work back to send it together, which nobody gains by measuring in hours.
pub const MAX_BATCH_WINDOW: Duration = Duration::from_secs(60);

/// The most items one batch may send. A batch of nothing sends nothing, and an unbounded
/// one is the trap every cap here exists to prevent.
pub const MAX_BATCH_ITEMS: u32 = 1_000;

/// The shortest block time a chain entry may name.
pub const MIN_BLOCK_TIME: Duration = Duration::from_millis(100);

/// The longest block time a chain entry may name.
pub const MAX_BLOCK_TIME: Duration = Duration::from_secs(60);

/// The least deep money arriving at the canister on Ethereum mainnet may be credited at: a
/// deposit, a ck minter withdrawal's USDC, the books' balance read. A one-block reorg
/// there is routine, and crediting below this would credit money that then leaves the
/// chain, which no later step takes back. A swap is acted on at its own depth, 1 block on
/// Ethereum too: that depth closes the canister's own transactions in
/// the outbox, and credits no money that arrived. The other chains' depths are the
/// deploy's to size; this one bounds `credit_confirmations` at the door, and crediting
/// reads at least this on Ethereum whatever the config holds.
pub const MIN_ETHEREUM_CONFIRMATIONS: BlockDepth = BlockDepth::new(12);

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

/// A set of chains, stored as the list, and as nothing at all while it is empty, so a config
/// written before the set existed reads back with it empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChainSet(pub std::collections::BTreeSet<ChainId>);

impl ChainSet {
    pub fn contains(&self, chain_id: ChainId) -> bool {
        self.0.contains(&chain_id)
    }
}

impl<C> Encode<C> for ChainSet {
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

impl<'b, C> Decode<'b, C> for ChainSet {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        if matches!(d.datatype()?, Type::Null | Type::Undefined) {
            d.skip()?;
            return Ok(Self::default());
        }
        std::collections::BTreeSet::decode(d, ctx).map(Self)
    }

    fn nil() -> Option<Self> {
        Some(Self::default())
    }
}

/// The chains whose transactions may wait for the chain's finalized block instead of a block
/// count: Polygon, whose finalized block trails its head by a few seconds, and BNB Chain,
/// whose finalized block trails by one or two (0.45 s each). Base's would follow Ethereum's
/// finality and slow every pool credit to minutes.
pub const FINALIZABLE: [ChainId; 2] = [ChainId::POLYGON, ChainId::BSC];

/// AX13 and AX14's bounds on an Across deposit, written by the kit from the one source the
/// quoter's settings take too: the least and the most a relayer keeps, in millionths of what
/// is deposited, and per destination an allowance for the arrival's gas, in micro USD.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct AcrossFee {
    #[n(0)]
    pub least_ppm: u32,
    #[n(1)]
    pub most_ppm: u32,
    #[n(2)]
    pub gas_allowance: ChainTable<TokenAmount>,
}

/// Across's contracts on one chain (BSC design 3.1): the periphery a deposit step calls, its
/// swap proxy, the SpokePool the periphery deposits into, and the handler an arrival's message
/// runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub struct AcrossPins {
    #[n(0)]
    pub periphery: EvmAddress,
    #[n(1)]
    pub swap_proxy: EvmAddress,
    #[n(2)]
    pub spoke_pool: EvmAddress,
    #[n(3)]
    pub multicall_handler: EvmAddress,
}

/// The most a relayer may keep, AX14's ceiling: 10% of what is deposited.
pub const MAX_ACROSS_MOST_PPM: u32 = 100_000;

/// Every knob the canister reads at runtime. `Debug` is safe to log: [`RpcUrl`] prints
/// as `***`.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Config {
    /// How long the outbox holds signed transactions back to send them together.
    #[n(6)]
    pub batch_window: Duration,
    /// The most items one batch sends, 1 to [`MAX_BATCH_ITEMS`].
    #[n(7)]
    pub max_batch_items: u32,
    /// How often the engine's backstop drives while a payout, an ICP op or a move is open,
    /// at most [`ENGINE_EVERY_MAX`]. Nothing drives on it while no work is open.
    #[n(9)]
    pub backstop_interval: Duration,
    /// Whether the canister simulates a transaction itself before it signs it. Off by
    /// default: the watcher and the submit API simulate `runFor` and every CCTP finish
    /// with an `eth_call` from the canister's address before they ask, and the canister's
    /// own simulation would be one more unreplicated outcall per signature.
    #[n(10)]
    pub simulate_before_sign: bool,
    /// The swap depth per chain: how deep the canister's own transactions must be before
    /// the outbox takes them as final. Crediting reads it too for a chain
    /// `credit_confirmations` has no entry for.
    #[n(13)]
    pub confirmations: BTreeMap<ChainId, BlockDepth>,
    /// Secrets: each url carries its provider's api key.
    #[n(14)]
    pub rpc_urls: BTreeMap<ChainId, RpcUrl>,
    /// The name of the threshold ECDSA key the canister signs with.
    #[n(16)]
    pub ecdsa_key_name: String,
    /// CCTP's domain id for each chain a burn may leave from or arrive on.
    #[n(21)]
    pub cctp_domains: ChainTable<CctpDomain>,
    /// Eco's `Portal`, one address on every chain Eco deploys to.
    #[n(25)]
    pub eco_portal: Option<EvmAddress>,
    /// Whether the Eco rail may be used. Off until its route is designed: a quote naming
    /// it is refused, and a swap already on it is paused until the rail is on again.
    #[n(26)]
    pub eco_enabled: EcoEnabled,
    /// The minimum fee each chain's `TokenMessengerV2` holds a burn from it to, as Circle
    /// stores it (`minFee`, thousandths of a basis point). A chain with no entry is one
    /// whose messenger charges none.
    #[n(28)]
    pub cctp_min_fees: ChainTable<CctpMinFee>,
    /// How often the pool's Base USDC and ICP ckUSDC are checked against the books: an hour
    /// unless set.
    #[n(31)]
    pub books_check_interval: BooksInterval,
    /// How far below what the books expect a balance may read before the pool pauses.
    #[n(32)]
    pub books_tolerance: BooksTolerance,
    /// Every EVM chain the canister signs on, design C: the Swapic deployment, CCTP v2,
    /// USDC, its block time, and per action the gas limit, the gas budget and the least
    /// fee. A chain with no entry signs nothing.
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
    /// The chains whose transactions are final at the chain's own finalized block (the
    /// `finalized` tag) instead of a block count: their `confirmations` and
    /// `credit_confirmations` decide nothing. At most [`FINALIZABLE`].
    #[n(36)]
    pub finalized: ChainSet,
    /// The 2 bytes Across credits Swapic's deposits to (D13), appended to the data of every
    /// transaction the canister signs with an Across step. None: nothing appended.
    #[n(37)]
    pub across_integrator: Option<u16>,
    /// AX13 and AX14's bounds. None: no order with an Across step is relayed.
    #[n(38)]
    pub across_fee: Option<AcrossFee>,
    /// The chains whose signed transactions the canister never broadcasts itself (6.2.13): the
    /// watcher sends them through its private node. Empty: every chain as today.
    #[n(39)]
    pub private_send: ChainSet,
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
    /// `Swapic.run` of a rebalance move's own order, from the canister's address.
    #[n(10)]
    PoolMove,
    /// The CCTP finish of a rebalance move on its destination, the canister's own: Ethereum's
    /// `completeCctp` with a CkDeposit, Base's `receiveMessage`.
    #[n(11)]
    PoolMoveFinish,
    /// The ck helper's `depositErc20` for the canister's own principal and rebalance
    /// subaccount on Ethereum, after a Base to ICP move's Eco fill.
    #[n(12)]
    PoolCkDeposit,
    /// Eco's `cancelAndProve` on an unfilled intent's destination, the proof's fee paid by the
    /// canister.
    #[n(13)]
    EcoRescue,
    /// A controller's sweep from the canister's address: its native gas, or `USDC.transfer`
    /// of booked fee income.
    #[n(14)]
    Sweep,
    /// `Swapic.runFor` of an order that deposits on Across.
    #[n(15)]
    RelayAcross,
}

impl GasAction {
    /// Every action, in code order.
    pub const ALL: [GasAction; 16] = [
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
        GasAction::EcoRescue,
        GasAction::Sweep,
        GasAction::RelayAcross,
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
            GasAction::EcoRescue => 13,
            GasAction::Sweep => 14,
            GasAction::RelayAcross => 15,
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
            GasAction::EcoRescue => "eco_rescue",
            GasAction::Sweep => "sweep",
            GasAction::RelayAcross => "relay_across",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.name() == name)
    }
}

/// One value per [`GasAction`]. The pool's three rows are optional, so a config
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
    /// The rebalancing's rows, optional as the pool's.
    #[n(10)]
    pub pool_move: Option<T>,
    #[n(11)]
    pub pool_move_finish: Option<T>,
    #[n(12)]
    pub pool_ck_deposit: Option<T>,
    /// The Eco rescue's row, optional as the pool's.
    #[n(13)]
    pub eco_rescue: Option<T>,
    /// The sweep's row, optional as the pool's.
    #[n(14)]
    pub sweep: Option<T>,
    /// The Across relay's row, optional as the pool's: absent, an Across order is refused.
    #[n(15)]
    pub relay_across: Option<T>,
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
            GasAction::EcoRescue => self.eco_rescue,
            GasAction::Sweep => self.sweep,
            GasAction::RelayAcross => self.relay_across,
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
            eco_rescue: Some(value),
            sweep: Some(value),
            relay_across: Some(value),
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
    /// USDC's EIP-712 domain name here, as its contract answers `name()`; none is
    /// [`DEFAULT_USDC_NAME`]. A config stored before the field reads as none.
    #[n(9)]
    pub usdc_name: Option<DomainText>,
    /// USDC's EIP-712 domain version here, as its contract answers `version()`; none is
    /// [`DEFAULT_USDC_VERSION`].
    #[n(10)]
    pub usdc_version: Option<DomainText>,
    /// How often the chain makes a block: the outbox reads a transaction's receipt at this
    /// pace, its block time times the depth it still needs.
    #[n(11)]
    pub block_time: Duration,
    /// The decimals of `usdc` here; none is 6. BNB Chain's (Binance's) USDC has 18: a fee
    /// in it is counted in micro USD by these.
    #[n(12)]
    pub usdc_decimals: Option<u8>,
    /// Across's contracts here. None: no Across step from, to or on this chain is relayed.
    #[n(13)]
    pub across: Option<AcrossPins>,
}

impl EvmChain {
    /// The decimals of this chain's `usdc`: 6 unless set.
    pub fn usdc_decimals(&self) -> u8 {
        self.usdc_decimals.unwrap_or(6)
    }

    /// `raw` units of this chain's `usdc` in micro USD, floored: what fees and books count.
    pub fn usdc_to_usd6(&self, raw: TokenAmount) -> TokenAmount {
        match self.usdc_decimals().saturating_sub(6) {
            0 => raw,
            extra => {
                let scale = alloy_primitives::U256::from(10u8).pow(alloy_primitives::U256::from(extra));
                TokenAmount::from_u256(raw.to_u256() / scale)
            }
        }
    }

    /// `usd6` micro USD in raw units of this chain's `usdc`, exact (a sweep of booked fees).
    pub fn usd6_to_usdc(&self, usd6: TokenAmount) -> alloy_primitives::U256 {
        let extra = self.usdc_decimals().saturating_sub(6);
        usd6.to_u256()
            .saturating_mul(alloy_primitives::U256::from(10u8).pow(alloy_primitives::U256::from(extra)))
    }
}

impl EvmChain {
    /// This chain's USDC as a 3009 authorization is signed under: its contract, and the
    /// name and version the entry names or Circle's usual ones.
    pub fn usdc_domain(&self) -> crate::swapic::UsdcDomain<'_> {
        crate::swapic::UsdcDomain {
            address: self.usdc,
            name: self
                .usdc_name
                .as_ref()
                .map_or(DEFAULT_USDC_NAME, DomainText::as_str),
            version: self
                .usdc_version
                .as_ref()
                .map_or(DEFAULT_USDC_VERSION, DomainText::as_str),
        }
    }
}

/// How often the pool's Base USDC and ICP ckUSDC are checked: an hour unless set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BooksInterval(pub Duration);

impl BooksInterval {
    pub const DEFAULT: Self = Self(Duration::from_secs(3_600));
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
    #[error("rpc_urls[{chain}] is empty")]
    EmptyRpcUrl { chain: ChainId },
    #[error("{field}[{chain}] {reason}")]
    NotDomainText {
        field: &'static str,
        chain: ChainId,
        reason: NotDomainText,
    },
    #[error("{field}{} is not an EVM address: {reason}", chain.map(|chain| format!("[{chain}]")).unwrap_or_default())]
    NotAnAddress {
        field: &'static str,
        chain: Option<ChainId>,
        reason: crate::evm::EvmAddressError,
    },
    #[error("finalized_chains names chain {chain}, which may not wait for a finalized block")]
    NotFinalizable { chain: ChainId },
    #[error("evm_chains[{chain}].usdc_decimals is {decimals}, outside 6 to 18")]
    UsdcDecimalsOutOfRange { chain: ChainId, decimals: u8 },
    #[error("across_fee: least_ppm {least_ppm} must be under most_ppm {most_ppm}, at most {MAX_ACROSS_MOST_PPM}")]
    AcrossFeeOutOfRange { least_ppm: u32, most_ppm: u32 },
    #[error("across_integrator is not 0x and 4 hex digits")]
    NotAnIntegratorId,
    #[error("evm_chains[{chain}].block_time is {block_time:?}, outside {MIN_BLOCK_TIME:?} to {MAX_BLOCK_TIME:?}")]
    BlockTimeOutOfRange {
        chain: ChainId,
        block_time: Duration,
    },
}

/// Which repeating timers a config write moved, so only those restart. The backstop is a
/// one-shot that reads its interval each time it arms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntervalChanges {
    /// The hourly pass, on `books_check_interval`.
    pub books: bool,
}

impl Default for Config {
    /// The spec numbers. The address maps and the key name are what a deploy fills in.
    fn default() -> Self {
        Self {
            batch_window: Duration::from_millis(2_000),
            max_batch_items: 10,
            backstop_interval: Duration::from_secs(30),
            // the watcher and the submit API simulate before they ask
            simulate_before_sign: false,
            confirmations: BTreeMap::from([
                // a swap on Ethereum is acted on at 1 block; crediting
                // there never reads below MIN_ETHEREUM_CONFIRMATIONS
                (ChainId::ETHEREUM, BlockDepth::new(1)),
                (ChainId::BASE, BlockDepth::new(1)),
                (ChainId::BSC, BlockDepth::new(1)),
                (ChainId::POLYGON, BlockDepth::new(6)),
                (ChainId::ARBITRUM, BlockDepth::new(1)),
            ]),
            rpc_urls: BTreeMap::new(),
            // "key_1" in production
            ecdsa_key_name: "dfx_test_key".to_string(),
            cctp_domains: ChainTable::default(),
            eco_portal: None,
            eco_enabled: EcoEnabled::OFF,
            cctp_min_fees: ChainTable::default(),
            books_check_interval: BooksInterval::DEFAULT,
            books_tolerance: BooksTolerance::DEFAULT,
            evm_chains: ChainTable::default(),
            pool_chain: None,
            // crediting reads the swap depths, held to the floor on Ethereum
            credit_confirmations: ChainTable::default(),
            finalized: ChainSet::default(),
            across_integrator: None,
            across_fee: None,
            private_send: ChainSet::default(),
        }
    }
}

impl Config {
    /// What a stored config must satisfy.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.ecdsa_key_name.is_empty() {
            return Err(ConfigError::EmptyEcdsaKeyName);
        }
        if let Some(chain) = self
            .finalized
            .0
            .iter()
            .find(|chain| !FINALIZABLE.contains(chain))
        {
            return Err(ConfigError::NotFinalizable { chain: *chain });
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
        let interval = self.books_check_interval.0;
        if interval > MAX_TIMER_INTERVAL {
            return Err(ConfigError::TimerIntervalTooLong {
                field: "books_check_interval_s",
                interval,
            });
        }
        for (field, duration, cap) in [
            (
                "backstop_interval_s",
                self.backstop_interval,
                ENGINE_EVERY_MAX,
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
        if self.max_batch_items == 0 || self.max_batch_items > MAX_BATCH_ITEMS {
            return Err(ConfigError::CapOutOfRange {
                field: "max_batch_items",
                cap: self.max_batch_items,
                ceiling: MAX_BATCH_ITEMS,
            });
        }
        if let Some((chain, entry)) = self
            .evm_chains
            .0
            .iter()
            .find(|(_, entry)| !(MIN_BLOCK_TIME..=MAX_BLOCK_TIME).contains(&entry.block_time))
        {
            return Err(ConfigError::BlockTimeOutOfRange {
                chain: *chain,
                block_time: entry.block_time,
            });
        }
        if let Some(chain) = self
            .rpc_urls
            .iter()
            .find_map(|(chain, url)| url.expose().is_empty().then_some(*chain))
        {
            return Err(ConfigError::EmptyRpcUrl { chain });
        }
        // a chain's USDC counts in 6 to 18 decimals; outside that a fee's micro USD is wrong
        if let Some((chain, decimals)) = self.evm_chains.0.iter().find_map(|(chain, entry)| {
            entry
                .usdc_decimals
                .filter(|d| !(6..=18).contains(d))
                .map(|d| (*chain, d))
        }) {
            return Err(ConfigError::UsdcDecimalsOutOfRange { chain, decimals });
        }
        // a relayer keeps more than nothing and at most 10%
        if let Some(fee) = &self.across_fee {
            if fee.least_ppm >= fee.most_ppm || fee.most_ppm > MAX_ACROSS_MOST_PPM {
                return Err(ConfigError::AcrossFeeOutOfRange {
                    least_ppm: fee.least_ppm,
                    most_ppm: fee.most_ppm,
                });
            }
        }
        Ok(())
    }

    /// How often the engine's backstop drives while work is open: `backstop_interval`, at
    /// most [`ENGINE_EVERY_MAX`], at least [`TIMER_FLOOR`].
    pub fn engine_every(&self) -> Duration {
        self.backstop_interval
            .min(ENGINE_EVERY_MAX)
            .max(TIMER_FLOOR)
    }

    /// Which timer intervals `new` moves, one flag per timer.
    pub fn interval_changes(&self, new: &Config) -> IntervalChanges {
        IntervalChanges {
            books: self.books_check_interval != new.books_check_interval,
        }
    }
}

crate::storable_as_cbor!(Config);
