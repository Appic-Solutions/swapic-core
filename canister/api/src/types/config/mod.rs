use crate::types::events::EvmAddressError;
use candid::{CandidType, Nat};
use serde::{Deserialize, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;
use types::address::{RedactedRpcUrl, REDACTED};
use types::config::{
    BooksInterval, BooksTolerance, ChainTable, DomainText, EcoEnabled, EvmChain, PerAction,
};
use types::rail::{CctpDomain, CctpMinFee};
use types::{BlockDepth, ChainId, CheckedAmountOf, EvmAddress, GasAmount, TokenAmount};

/// Every knob the canister reads at runtime. The rpc urls, the chain table and the key name
/// are what a deploy fills in.
#[derive(CandidType, Deserialize, Serialize, Clone, PartialEq)]
pub struct Config {
    /// How long the outbox holds signed transactions back to send them together.
    pub batch_window_ms: u64,
    pub max_batch_items: u32,
    /// How often the engine's backstop drives while a payout, an ICP op or a move is open,
    /// at most 60. Nothing drives on it while no work is open.
    pub backstop_interval_s: u64,
    /// Whether the canister simulates a transaction itself before it signs it. False by
    /// default: the watcher and the submit API simulate before they ask.
    pub simulate_before_sign: bool,
    pub confirmations: BTreeMap<u64, u64>,
    pub rpc_urls: BTreeMap<u64, String>,
    pub ecdsa_key_name: String,
    /// CCTP's domain id per chain: Circle's own numbering, never a chain id.
    pub cctp_domains: BTreeMap<u64, u32>,
    /// Eco's `Portal`, the same address on every chain.
    pub eco_portal: Option<String>,
    /// Whether the Eco rail may be used. False until its route is designed.
    pub eco_enabled: bool,
    /// The minimum fee each chain's `TokenMessengerV2` holds a burn from it to, as Circle
    /// stores it (`minFee()`, in thousandths of a basis point, below 10,000,000). A chain
    /// with no entry is one whose messenger charges none. Every burn offers at least this
    /// much of its amount as its `maxFee`, the Standard path included.
    pub cctp_min_fees: BTreeMap<u64, u32>,
    /// How often the pool's Base USDC and ICP ckUSDC are checked against the books: 3,600
    /// by default.
    pub books_check_interval_s: u64,
    /// How far below what the books expect a balance may read, in micro-USDC, before the
    /// pool pauses.
    #[serde(serialize_with = "nat_as_decimal")]
    pub books_tolerance: Nat,
    /// Every EVM chain the canister signs on (design C). A chain with no entry signs
    /// nothing.
    pub evm_chains: BTreeMap<u64, EvmChainConfig>,
    /// The chain the pool's EVM pocket is on, whose books the pool's guard reads. None:
    /// the pool is closed.
    pub pool_chain: Option<u64>,
    /// The crediting depth per chain: how deep money arriving at the canister (a deposit,
    /// a ck minter withdrawal's USDC, the books' balance read) must be before it is
    /// credited. Absent, or a chain with no entry: the chain's `confirmations`, the swap
    /// depth. Ethereum's is refused below 12 (`DepthTooShallow`), and crediting there never
    /// reads below 12 whatever is set.
    pub credit_confirmations: Option<BTreeMap<u64, u64>>,
}

/// One EVM chain as design C runs on it: its Swapic deployment, CCTP v2, its USDC, its
/// block time, and per action the gas limit, the gas budget and the least fee. Addresses
/// are EVM addresses.
#[derive(CandidType, Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct EvmChainConfig {
    /// The door users approve, and the EIP-712 verifying contract of every order here.
    pub swapic: String,
    /// The one shared Runner: it makes this chain's burns.
    pub runner: String,
    /// The mint recipient and destination caller of every Swap and CkDeposit arriving here.
    pub cctp_receiver: String,
    pub usdc: String,
    /// CCTP v2's `TokenMessengerV2`.
    pub token_messenger: String,
    /// CCTP v2's `MessageTransmitterV2`.
    pub message_transmitter: String,
    /// The most gas the canister gives each action's transaction. `relay_gasless` and
    /// `finish_cctp` sign with exactly the limit they are given and refuse one above this,
    /// but a release, which is signed at this when it is asked for more.
    pub gas_limit: ActionGas,
    /// The most each action's transaction may cost in wei (`gas_limit * max_fee_per_gas`):
    /// the gas rule. Above it the canister signs nothing and the caller tries again.
    pub gas_budget_wei: ActionAmounts,
    /// The least fee step, in USDC units, each action needs.
    pub min_network_fee: ActionAmounts,
    /// USDC's EIP-712 domain name on this chain, as its contract answers `name()`: what a
    /// 3009 authorization is signed under. None: "USD Coin". 1 to 32 printable ASCII
    /// characters.
    pub usdc_domain_name: Option<String>,
    /// USDC's EIP-712 domain version on this chain, as its contract answers `version()`.
    /// None: "2". 1 to 32 printable ASCII characters.
    pub usdc_domain_version: Option<String>,
    /// How often the chain makes a block, 100 to 60,000: the outbox reads a transaction's
    /// receipt at this pace.
    pub block_time_ms: u64,
}

/// One gas limit per action the canister signs.
#[derive(CandidType, Deserialize, Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActionGas {
    pub relay_cctp: u64,
    pub relay_eco: u64,
    pub relay_local: u64,
    pub complete_swap: u64,
    pub complete_ck: u64,
    pub relay_deliver: u64,
    pub release: u64,
    /// The pool's rows, optional: an absent one refuses its action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_payout: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_transfer: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_approve: Option<u64>,
    /// The rebalancing's rows, optional as the pool's: a move's own order, its CCTP finish
    /// (the limit the watcher finishes the canister's own move with), and a Base to ICP Eco
    /// move's ck deposit on Ethereum.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_move: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_move_finish: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_ck_deposit: Option<u64>,
    /// Eco's `cancelAndProve` of an unfilled intent, optional as the pool's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eco_rescue: Option<u64>,
    /// A controller's sweep (`sweep_evm`): a native transfer or a `USDC.transfer`, optional
    /// as the pool's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sweep: Option<u64>,
}

/// One amount per action the canister signs.
#[derive(CandidType, Deserialize, Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ActionAmounts {
    #[serde(serialize_with = "nat_as_decimal")]
    pub relay_cctp: Nat,
    #[serde(serialize_with = "nat_as_decimal")]
    pub relay_eco: Nat,
    #[serde(serialize_with = "nat_as_decimal")]
    pub relay_local: Nat,
    #[serde(serialize_with = "nat_as_decimal")]
    pub complete_swap: Nat,
    #[serde(serialize_with = "nat_as_decimal")]
    pub complete_ck: Nat,
    #[serde(serialize_with = "nat_as_decimal")]
    pub relay_deliver: Nat,
    #[serde(serialize_with = "nat_as_decimal")]
    pub release: Nat,
    /// The pool's rows, optional: an absent one refuses its action.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "opt_nat_as_decimal"
    )]
    pub pool_payout: Option<Nat>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "opt_nat_as_decimal"
    )]
    pub pool_transfer: Option<Nat>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "opt_nat_as_decimal"
    )]
    pub pool_approve: Option<Nat>,
    /// The rebalancing's rows, optional as the pool's.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "opt_nat_as_decimal"
    )]
    pub pool_move: Option<Nat>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "opt_nat_as_decimal"
    )]
    pub pool_move_finish: Option<Nat>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "opt_nat_as_decimal"
    )]
    pub pool_ck_deposit: Option<Nat>,
    /// Eco's `cancelAndProve` of an unfilled intent, optional as the pool's.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "opt_nat_as_decimal"
    )]
    pub eco_rescue: Option<Nat>,
    /// A controller's sweep, optional as the pool's.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "opt_nat_as_decimal"
    )]
    pub sweep: Option<Nat>,
}

impl From<PerAction<GasAmount>> for ActionGas {
    fn from(gas: PerAction<GasAmount>) -> Self {
        let limit = |gas: GasAmount| {
            gas.try_into_u128()
                .and_then(|gas| u64::try_from(gas).ok())
                .unwrap_or(u64::MAX)
        };
        Self {
            relay_cctp: limit(gas.relay_cctp),
            relay_eco: limit(gas.relay_eco),
            relay_local: limit(gas.relay_local),
            complete_swap: limit(gas.complete_swap),
            complete_ck: limit(gas.complete_ck),
            relay_deliver: limit(gas.relay_deliver),
            release: limit(gas.release),
            pool_payout: gas.pool_payout.map(limit),
            pool_transfer: gas.pool_transfer.map(limit),
            pool_approve: gas.pool_approve.map(limit),
            pool_move: gas.pool_move.map(limit),
            pool_move_finish: gas.pool_move_finish.map(limit),
            pool_ck_deposit: gas.pool_ck_deposit.map(limit),
            eco_rescue: gas.eco_rescue.map(limit),
            sweep: gas.sweep.map(limit),
        }
    }
}

impl From<ActionGas> for PerAction<GasAmount> {
    fn from(gas: ActionGas) -> Self {
        Self {
            relay_cctp: GasAmount::from(gas.relay_cctp),
            relay_eco: GasAmount::from(gas.relay_eco),
            relay_local: GasAmount::from(gas.relay_local),
            complete_swap: GasAmount::from(gas.complete_swap),
            complete_ck: GasAmount::from(gas.complete_ck),
            relay_deliver: GasAmount::from(gas.relay_deliver),
            release: GasAmount::from(gas.release),
            pool_payout: gas.pool_payout.map(GasAmount::from),
            pool_transfer: gas.pool_transfer.map(GasAmount::from),
            pool_approve: gas.pool_approve.map(GasAmount::from),
            pool_move: gas.pool_move.map(GasAmount::from),
            pool_move_finish: gas.pool_move_finish.map(GasAmount::from),
            pool_ck_deposit: gas.pool_ck_deposit.map(GasAmount::from),
            eco_rescue: gas.eco_rescue.map(GasAmount::from),
            sweep: gas.sweep.map(GasAmount::from),
        }
    }
}

impl<Unit> From<PerAction<CheckedAmountOf<Unit>>> for ActionAmounts {
    fn from(amounts: PerAction<CheckedAmountOf<Unit>>) -> Self {
        Self {
            relay_cctp: amounts.relay_cctp.into(),
            relay_eco: amounts.relay_eco.into(),
            relay_local: amounts.relay_local.into(),
            complete_swap: amounts.complete_swap.into(),
            complete_ck: amounts.complete_ck.into(),
            relay_deliver: amounts.relay_deliver.into(),
            release: amounts.release.into(),
            pool_payout: amounts.pool_payout.map(Into::into),
            pool_transfer: amounts.pool_transfer.map(Into::into),
            pool_approve: amounts.pool_approve.map(Into::into),
            pool_move: amounts.pool_move.map(Into::into),
            pool_move_finish: amounts.pool_move_finish.map(Into::into),
            pool_ck_deposit: amounts.pool_ck_deposit.map(Into::into),
            eco_rescue: amounts.eco_rescue.map(Into::into),
            sweep: amounts.sweep.map(Into::into),
        }
    }
}

fn per_action<Unit>(
    field: &'static str,
    amounts: ActionAmounts,
) -> Result<PerAction<CheckedAmountOf<Unit>>, types::ConfigError> {
    let amount = |nat: Nat| {
        CheckedAmountOf::<Unit>::try_from(nat)
            .map_err(|_| types::ConfigError::AmountTooLarge { field })
    };
    Ok(PerAction {
        relay_cctp: amount(amounts.relay_cctp)?,
        relay_eco: amount(amounts.relay_eco)?,
        relay_local: amount(amounts.relay_local)?,
        complete_swap: amount(amounts.complete_swap)?,
        complete_ck: amount(amounts.complete_ck)?,
        relay_deliver: amount(amounts.relay_deliver)?,
        release: amount(amounts.release)?,
        pool_payout: amounts.pool_payout.map(amount).transpose()?,
        pool_transfer: amounts.pool_transfer.map(amount).transpose()?,
        pool_approve: amounts.pool_approve.map(amount).transpose()?,
        pool_move: amounts.pool_move.map(amount).transpose()?,
        pool_move_finish: amounts.pool_move_finish.map(amount).transpose()?,
        pool_ck_deposit: amounts.pool_ck_deposit.map(amount).transpose()?,
        eco_rescue: amounts.eco_rescue.map(amount).transpose()?,
        sweep: amounts.sweep.map(amount).transpose()?,
    })
}

impl From<EvmChain> for EvmChainConfig {
    fn from(chain: EvmChain) -> Self {
        Self {
            swapic: chain.swapic.to_string(),
            runner: chain.runner.to_string(),
            cctp_receiver: chain.cctp_receiver.to_string(),
            usdc: chain.usdc.to_string(),
            token_messenger: chain.token_messenger.to_string(),
            message_transmitter: chain.message_transmitter.to_string(),
            gas_limit: chain.gas_limit.into(),
            gas_budget_wei: chain.gas_budget.into(),
            min_network_fee: chain.min_network_fee.into(),
            usdc_domain_name: chain.usdc_name.map(|name| name.to_string()),
            usdc_domain_version: chain.usdc_version.map(|version| version.to_string()),
            block_time_ms: millis(chain.block_time),
        }
    }
}

fn evm_chain(chain: ChainId, config: EvmChainConfig) -> Result<EvmChain, types::ConfigError> {
    let at = |field: &'static str, text: &str| evm_address(field, Some(chain), text);
    let domain_text = |field: &'static str, text: Option<String>| {
        text.map(|text| {
            text.parse::<DomainText>()
                .map_err(|reason| types::ConfigError::NotDomainText {
                    field,
                    chain,
                    reason,
                })
        })
        .transpose()
    };
    Ok(EvmChain {
        swapic: at("evm_chains.swapic", &config.swapic)?,
        runner: at("evm_chains.runner", &config.runner)?,
        cctp_receiver: at("evm_chains.cctp_receiver", &config.cctp_receiver)?,
        usdc: at("evm_chains.usdc", &config.usdc)?,
        token_messenger: at("evm_chains.token_messenger", &config.token_messenger)?,
        message_transmitter: at(
            "evm_chains.message_transmitter",
            &config.message_transmitter,
        )?,
        gas_limit: config.gas_limit.into(),
        gas_budget: per_action("evm_chains.gas_budget_wei", config.gas_budget_wei)?,
        min_network_fee: per_action("evm_chains.min_network_fee", config.min_network_fee)?,
        usdc_name: domain_text("evm_chains.usdc_domain_name", config.usdc_domain_name)?,
        usdc_version: domain_text("evm_chains.usdc_domain_version", config.usdc_domain_version)?,
        block_time: Duration::from_millis(config.block_time_ms),
    })
}

impl Default for Config {
    fn default() -> Self {
        Self::unredacted(types::Config::default())
    }
}

/// Every rpc url prints as `***`: the full config crosses the wire in `get_config_full`
/// and `set_config`, and a stray `{:?}` of either must not print the api keys.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // exhaustive: a new field fails to compile until it decides how it prints
        let Config {
            batch_window_ms,
            max_batch_items,
            backstop_interval_s,
            simulate_before_sign,
            confirmations,
            rpc_urls,
            ecdsa_key_name,
            cctp_domains,
            eco_portal,
            eco_enabled,
            cctp_min_fees,
            books_check_interval_s,
            books_tolerance,
            evm_chains,
            pool_chain,
            credit_confirmations,
        } = self;
        let rpc_urls: BTreeMap<&u64, &str> =
            rpc_urls.keys().map(|chain| (chain, REDACTED)).collect();
        f.debug_struct("Config")
            .field("batch_window_ms", batch_window_ms)
            .field("max_batch_items", max_batch_items)
            .field("backstop_interval_s", backstop_interval_s)
            .field("simulate_before_sign", simulate_before_sign)
            .field("confirmations", confirmations)
            .field("rpc_urls", &rpc_urls)
            .field("ecdsa_key_name", ecdsa_key_name)
            .field("cctp_domains", cctp_domains)
            .field("eco_portal", eco_portal)
            .field("eco_enabled", eco_enabled)
            .field("cctp_min_fees", cctp_min_fees)
            .field("books_check_interval_s", books_check_interval_s)
            .field("books_tolerance", books_tolerance)
            .field("evm_chains", evm_chains)
            .field("pool_chain", pool_chain)
            .field("credit_confirmations", credit_confirmations)
            .finish()
    }
}

/// A `Nat` as a string of its plain decimal digits. candid's `Display` groups them with
/// underscores, and a JSON number past 2^53 loses digits in most readers.
fn nat_as_decimal<S: Serializer>(value: &Nat, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.0.to_str_radix(10))
}

/// [`nat_as_decimal`] of a row that is set (an absent row is never serialized).
fn opt_nat_as_decimal<S: Serializer>(
    value: &Option<Nat>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => nat_as_decimal(value, serializer),
        None => serializer.serialize_none(),
    }
}

impl Config {
    /// The whole config, rpc urls included. Only for callers allowed to hold the api keys.
    pub fn unredacted(config: types::Config) -> Self {
        let rpc_urls = config
            .rpc_urls
            .iter()
            .map(|(chain, url)| (chain.get(), url.expose().to_string()))
            .collect();
        Self {
            rpc_urls,
            ..Self::from(config)
        }
    }
}

/// The public view: every rpc url prints as `***` through `RpcUrl`, and the chain ids stay.
impl From<types::Config> for Config {
    fn from(config: types::Config) -> Self {
        let types::Config {
            batch_window,
            max_batch_items,
            backstop_interval,
            simulate_before_sign,
            confirmations,
            rpc_urls,
            ecdsa_key_name,
            cctp_domains,
            eco_portal,
            eco_enabled,
            cctp_min_fees,
            books_check_interval,
            books_tolerance,
            evm_chains,
            pool_chain,
            credit_confirmations,
        } = config;
        Self {
            batch_window_ms: u64::try_from(batch_window.as_millis())
                .expect("BUG: batch_window enters as u64 milliseconds"),
            max_batch_items,
            backstop_interval_s: backstop_interval.as_secs(),
            simulate_before_sign,
            confirmations: confirmations
                .into_iter()
                .map(|(chain, depth)| (chain.get(), depth.get()))
                .collect(),
            rpc_urls: rpc_urls
                .into_iter()
                .map(|(chain, url)| (chain.get(), url.to_string()))
                .collect(),
            ecdsa_key_name,
            cctp_domains: cctp_domains
                .0
                .into_iter()
                .map(|(chain, domain)| (chain.get(), domain.get()))
                .collect(),
            eco_portal: eco_portal.map(|address| address.to_string()),
            eco_enabled: eco_enabled.is_on(),
            cctp_min_fees: cctp_min_fees
                .0
                .into_iter()
                .map(|(chain, min_fee)| (chain.get(), min_fee.get()))
                .collect(),
            books_check_interval_s: books_check_interval.0.as_secs(),
            books_tolerance: books_tolerance.0.into(),
            evm_chains: evm_chains
                .0
                .into_iter()
                .map(|(chain, config)| (chain.get(), config.into()))
                .collect(),
            pool_chain: pool_chain.map(ChainId::get),
            credit_confirmations: Some(
                credit_confirmations
                    .0
                    .into_iter()
                    .map(|(chain, depth)| (chain.get(), depth.get()))
                    .collect(),
            ),
        }
    }
}

/// `text` as an EVM address, or the refusal naming the knob (and the chain, for a table).
fn evm_address(
    field: &'static str,
    chain: Option<ChainId>,
    text: &str,
) -> Result<EvmAddress, types::ConfigError> {
    text.parse()
        .map_err(|reason| types::ConfigError::NotAnAddress {
            field,
            chain,
            reason,
        })
}

impl TryFrom<Config> for types::Config {
    type Error = types::ConfigError;

    fn try_from(config: Config) -> Result<Self, Self::Error> {
        Ok(Self {
            batch_window: Duration::from_millis(config.batch_window_ms),
            max_batch_items: config.max_batch_items,
            backstop_interval: Duration::from_secs(config.backstop_interval_s),
            simulate_before_sign: config.simulate_before_sign,
            confirmations: config
                .confirmations
                .into_iter()
                .map(|(chain, depth)| (ChainId::new(chain), BlockDepth::new(depth)))
                .collect(),
            rpc_urls: config
                .rpc_urls
                .into_iter()
                .map(|(chain, url)| {
                    let chain = ChainId::new(chain);
                    url.parse()
                        .map(|url| (chain, url))
                        .map_err(|RedactedRpcUrl| types::ConfigError::RedactedRpcUrl { chain })
                })
                .collect::<Result<_, _>>()?,
            ecdsa_key_name: config.ecdsa_key_name,
            cctp_domains: ChainTable(
                config
                    .cctp_domains
                    .into_iter()
                    .map(|(chain, domain)| (ChainId::new(chain), CctpDomain::new(domain)))
                    .collect(),
            ),
            eco_portal: config
                .eco_portal
                .as_deref()
                .map(|address| evm_address("eco_portal", None, address))
                .transpose()?,
            eco_enabled: EcoEnabled::new(config.eco_enabled),
            cctp_min_fees: ChainTable(
                config
                    .cctp_min_fees
                    .into_iter()
                    .map(|(chain, min_fee)| (ChainId::new(chain), CctpMinFee::new(min_fee)))
                    .collect(),
            ),
            books_check_interval: BooksInterval(Duration::from_secs(config.books_check_interval_s)),
            books_tolerance: BooksTolerance(
                TokenAmount::try_from(config.books_tolerance).map_err(|_| {
                    types::ConfigError::AmountTooLarge {
                        field: "books_tolerance",
                    }
                })?,
            ),
            evm_chains: ChainTable(
                config
                    .evm_chains
                    .into_iter()
                    .map(|(chain, entry)| {
                        let chain = ChainId::new(chain);
                        evm_chain(chain, entry).map(|entry| (chain, entry))
                    })
                    .collect::<Result<_, _>>()?,
            ),
            pool_chain: config.pool_chain.map(ChainId::new),
            credit_confirmations: ChainTable(
                config
                    .credit_confirmations
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(chain, depth)| (ChainId::new(chain), BlockDepth::new(depth)))
                    .collect(),
            ),
        })
    }
}

/// Why a config was refused, naming the knob at fault.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    EmptyEcdsaKeyName,
    /// The canister has already derived its EVM address under the key name it holds, and
    /// the Swapic contracts on every chain name that address.
    EcdsaKeyNameFixed {
        current: String,
        requested: String,
    },
    TimerIntervalTooLong {
        field: String,
        interval_s: u64,
    },
    /// In milliseconds, so the one knob set in milliseconds is reported as it was set.
    DurationAboveCap {
        field: String,
        duration_ms: u64,
        cap_ms: u64,
    },
    /// In milliseconds, like the cap: the knob is below the least it may be.
    DurationBelowFloor {
        field: String,
        duration_ms: u64,
        floor_ms: u64,
    },
    CapOutOfRange {
        field: String,
        cap: u32,
        ceiling: u32,
    },
    /// The crediting depth configured for `chain_id` (`credit_confirmations`) is below the
    /// floor a reorg on that chain makes necessary, so money arriving there could be
    /// credited and then leave the chain.
    DepthTooShallow {
        chain_id: u64,
        depth: u64,
        floor: u64,
    },
    /// The minimum fee listed for `chain_id` is at or above `ceiling`, the whole amount in
    /// Circle's units, which Circle's own setter refuses.
    CctpMinFeeTooHigh {
        chain_id: u64,
        min_fee: u32,
        ceiling: u32,
    },
    AmountTooLarge {
        field: String,
    },
    DurationTooLong {
        field: String,
    },
    RedactedRpcUrl {
        chain_id: u64,
    },
    EmptyRpcUrl {
        chain_id: u64,
    },
    /// The text in `field` at `chain_id` is not an EIP-712 domain name or version: 1 to 32
    /// printable ASCII characters.
    NotDomainText {
        field: String,
        chain_id: u64,
    },
    /// The text in `field` (for a table, at `chain_id`) is not an EVM address.
    NotAnAddress {
        field: String,
        chain_id: Option<u64>,
        reason: EvmAddressError,
    },
    /// `evm_chains[chain_id].block_time_ms` is outside 100 to 60,000.
    BlockTimeOutOfRange {
        chain_id: u64,
        block_time_ms: u64,
    },
}

/// Whole milliseconds, for an error or a wire field to carry. A duration past `u64::MAX` of
/// them, which is five hundred million years, reports as `u64::MAX`.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl From<types::ConfigError> for ConfigError {
    fn from(error: types::ConfigError) -> Self {
        use types::ConfigError as Domain;
        match error {
            Domain::EmptyEcdsaKeyName => Self::EmptyEcdsaKeyName,
            Domain::EcdsaKeyNameFixed { current, requested } => {
                Self::EcdsaKeyNameFixed { current, requested }
            }
            Domain::TimerIntervalTooLong { field, interval } => Self::TimerIntervalTooLong {
                field: field.to_string(),
                interval_s: interval.as_secs(),
            },
            Domain::DurationAboveCap {
                field,
                duration,
                cap,
            } => Self::DurationAboveCap {
                field: field.to_string(),
                duration_ms: millis(duration),
                cap_ms: millis(cap),
            },
            Domain::DurationBelowFloor {
                field,
                duration,
                floor,
            } => Self::DurationBelowFloor {
                field: field.to_string(),
                duration_ms: millis(duration),
                floor_ms: millis(floor),
            },
            Domain::CapOutOfRange {
                field,
                cap,
                ceiling,
            } => Self::CapOutOfRange {
                field: field.to_string(),
                cap,
                ceiling,
            },
            Domain::DepthTooShallow {
                chain,
                depth,
                floor,
            } => Self::DepthTooShallow {
                chain_id: chain.get(),
                depth: depth.get(),
                floor: floor.get(),
            },
            Domain::CctpMinFeeTooHigh {
                chain,
                min_fee,
                ceiling,
            } => Self::CctpMinFeeTooHigh {
                chain_id: chain.get(),
                min_fee: min_fee.get(),
                ceiling,
            },
            Domain::AmountTooLarge { field } => Self::AmountTooLarge {
                field: field.to_string(),
            },
            Domain::DurationTooLong { field } => Self::DurationTooLong {
                field: field.to_string(),
            },
            Domain::RedactedRpcUrl { chain } => Self::RedactedRpcUrl {
                chain_id: chain.get(),
            },
            Domain::EmptyRpcUrl { chain } => Self::EmptyRpcUrl {
                chain_id: chain.get(),
            },
            Domain::NotDomainText { field, chain, .. } => Self::NotDomainText {
                field: field.to_string(),
                chain_id: chain.get(),
            },
            Domain::NotAnAddress {
                field,
                chain,
                reason,
            } => Self::NotAnAddress {
                field: field.to_string(),
                chain_id: chain.map(ChainId::get),
                reason: reason.into(),
            },
            Domain::BlockTimeOutOfRange { chain, block_time } => Self::BlockTimeOutOfRange {
                chain_id: chain.get(),
                block_time_ms: millis(block_time),
            },
        }
    }
}

#[cfg(test)]
mod tests;
