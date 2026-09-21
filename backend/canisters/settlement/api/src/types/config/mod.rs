use candid::{CandidType, Nat};
use serde::{Deserialize, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;
use types::address::{RedactedRpcUrl, TextTooLong, REDACTED};
use types::config::{AuditChunk, EvictionsPerSweep, RefundsPerSweep};
use types::{BasisPoints, BlockDepth, ChainId, UsdAmount};

/// Every knob the canister reads at runtime. Numbers are the spec defaults; the two
/// address maps and the key name are what a deploy fills in.
#[derive(CandidType, Deserialize, Serialize, Clone, PartialEq)]
pub struct Config {
    pub platform_fee_bps: u16,
    pub max_fee_bps: u16,
    // a decimal string in the ConfigChanged JSON, so no reader rounds it
    #[serde(serialize_with = "nat_as_decimal")]
    pub max_swap_usd: Nat,
    pub quote_ttl_s: u64,
    pub permit_deadline_s: u64,
    pub chain_data_max_age_s: u64,
    pub batch_window_ms: u64,
    pub max_batch_items: u32,
    pub decision_timeout_min: u64,
    pub rail_status_max_age_s: u64,
    pub simulate_before_sign: bool,
    pub expiry_check_interval_s: u64,
    pub replay_audit_interval_s: u64,
    pub confirmations: BTreeMap<u64, u64>,
    pub rpc_urls: BTreeMap<u64, String>,
    pub vault_addresses: BTreeMap<u64, String>,
    pub ecdsa_key_name: String,
    pub max_refunds_per_sweep: u32,
    pub max_evictions_per_sweep: u32,
    pub audit_chunk_events: u32,
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
            platform_fee_bps,
            max_fee_bps,
            max_swap_usd,
            quote_ttl_s,
            permit_deadline_s,
            chain_data_max_age_s,
            batch_window_ms,
            max_batch_items,
            decision_timeout_min,
            rail_status_max_age_s,
            simulate_before_sign,
            expiry_check_interval_s,
            replay_audit_interval_s,
            confirmations,
            rpc_urls,
            vault_addresses,
            ecdsa_key_name,
            max_refunds_per_sweep,
            max_evictions_per_sweep,
            audit_chunk_events,
        } = self;
        let rpc_urls: BTreeMap<&u64, &str> =
            rpc_urls.keys().map(|chain| (chain, REDACTED)).collect();
        f.debug_struct("Config")
            .field("platform_fee_bps", platform_fee_bps)
            .field("max_fee_bps", max_fee_bps)
            .field("max_swap_usd", max_swap_usd)
            .field("quote_ttl_s", quote_ttl_s)
            .field("permit_deadline_s", permit_deadline_s)
            .field("chain_data_max_age_s", chain_data_max_age_s)
            .field("batch_window_ms", batch_window_ms)
            .field("max_batch_items", max_batch_items)
            .field("decision_timeout_min", decision_timeout_min)
            .field("rail_status_max_age_s", rail_status_max_age_s)
            .field("simulate_before_sign", simulate_before_sign)
            .field("expiry_check_interval_s", expiry_check_interval_s)
            .field("replay_audit_interval_s", replay_audit_interval_s)
            .field("confirmations", confirmations)
            .field("rpc_urls", &rpc_urls)
            .field("vault_addresses", vault_addresses)
            .field("ecdsa_key_name", ecdsa_key_name)
            .field("max_refunds_per_sweep", max_refunds_per_sweep)
            .field("max_evictions_per_sweep", max_evictions_per_sweep)
            .field("audit_chunk_events", audit_chunk_events)
            .finish()
    }
}

/// A `Nat` as a string of its plain decimal digits. candid's `Display` groups them with
/// underscores, and a JSON number past 2^53 loses digits in most readers.
fn nat_as_decimal<S: Serializer>(value: &Nat, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.0.to_str_radix(10))
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
            platform_fee,
            max_fee,
            max_swap,
            quote_ttl,
            permit_deadline,
            chain_data_max_age,
            batch_window,
            max_batch_items,
            decision_timeout,
            rail_status_max_age,
            simulate_before_sign,
            expiry_check_interval,
            replay_audit_interval,
            confirmations,
            rpc_urls,
            vault_addresses,
            ecdsa_key_name,
            max_refunds_per_sweep,
            max_evictions_per_sweep,
            audit_chunk_events,
        } = config;
        Self {
            platform_fee_bps: platform_fee.get(),
            max_fee_bps: max_fee.get(),
            max_swap_usd: max_swap.into(),
            quote_ttl_s: quote_ttl.as_secs(),
            permit_deadline_s: permit_deadline.as_secs(),
            chain_data_max_age_s: chain_data_max_age.as_secs(),
            batch_window_ms: u64::try_from(batch_window.as_millis())
                .expect("BUG: batch_window enters as u64 milliseconds"),
            max_batch_items,
            decision_timeout_min: decision_timeout.as_secs() / 60,
            rail_status_max_age_s: rail_status_max_age.as_secs(),
            simulate_before_sign,
            expiry_check_interval_s: expiry_check_interval.as_secs(),
            replay_audit_interval_s: replay_audit_interval.as_secs(),
            confirmations: confirmations
                .into_iter()
                .map(|(chain, depth)| (chain.get(), depth.get()))
                .collect(),
            rpc_urls: rpc_urls
                .into_iter()
                .map(|(chain, url)| (chain.get(), url.to_string()))
                .collect(),
            vault_addresses: vault_addresses
                .into_iter()
                .map(|(chain, address)| (chain.get(), address.to_string()))
                .collect(),
            ecdsa_key_name,
            max_refunds_per_sweep: max_refunds_per_sweep.get(),
            max_evictions_per_sweep: max_evictions_per_sweep.get(),
            audit_chunk_events: audit_chunk_events.get(),
        }
    }
}

impl TryFrom<Config> for types::Config {
    type Error = types::ConfigError;

    fn try_from(config: Config) -> Result<Self, Self::Error> {
        Ok(Self {
            platform_fee: BasisPoints::new(config.platform_fee_bps),
            max_fee: BasisPoints::new(config.max_fee_bps),
            max_swap: UsdAmount::try_from(config.max_swap_usd).map_err(|_| {
                types::ConfigError::AmountTooLarge {
                    field: "max_swap_usd",
                }
            })?,
            quote_ttl: Duration::from_secs(config.quote_ttl_s),
            permit_deadline: Duration::from_secs(config.permit_deadline_s),
            chain_data_max_age: Duration::from_secs(config.chain_data_max_age_s),
            batch_window: Duration::from_millis(config.batch_window_ms),
            max_batch_items: config.max_batch_items,
            decision_timeout: config
                .decision_timeout_min
                .checked_mul(60)
                .map(Duration::from_secs)
                .ok_or(types::ConfigError::DurationTooLong {
                    field: "decision_timeout_min",
                })?,
            rail_status_max_age: Duration::from_secs(config.rail_status_max_age_s),
            simulate_before_sign: config.simulate_before_sign,
            expiry_check_interval: Duration::from_secs(config.expiry_check_interval_s),
            replay_audit_interval: Duration::from_secs(config.replay_audit_interval_s),
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
            vault_addresses: config
                .vault_addresses
                .into_iter()
                .map(|(chain, address)| {
                    let chain = ChainId::new(chain);
                    address.parse().map(|address| (chain, address)).map_err(
                        |TextTooLong { len }| types::ConfigError::VaultAddressTooLong {
                            chain,
                            len,
                        },
                    )
                })
                .collect::<Result<_, _>>()?,
            ecdsa_key_name: config.ecdsa_key_name,
            max_refunds_per_sweep: RefundsPerSweep::new(config.max_refunds_per_sweep),
            max_evictions_per_sweep: EvictionsPerSweep::new(config.max_evictions_per_sweep),
            audit_chunk_events: AuditChunk::new(config.audit_chunk_events),
        })
    }
}

/// Why a config was refused, naming the knob at fault.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    MaxFeeAboveHundredPercent(u16),
    PlatformFeeAboveMax {
        platform_fee_bps: u16,
        max_fee_bps: u16,
    },
    EmptyEcdsaKeyName,
    TimerIntervalTooLong {
        field: String,
        interval_s: u64,
    },
    DurationAboveCap {
        field: String,
        duration_s: u64,
    },
    CapOutOfRange {
        field: String,
        cap: u32,
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
    VaultAddressTooLong {
        chain_id: u64,
        len: u64,
    },
    EmptyVaultAddress {
        chain_id: u64,
    },
    EmptyRpcUrl {
        chain_id: u64,
    },
}

impl From<types::ConfigError> for ConfigError {
    fn from(error: types::ConfigError) -> Self {
        use types::ConfigError as Domain;
        match error {
            Domain::MaxFeeAboveHundredPercent(max_fee) => {
                Self::MaxFeeAboveHundredPercent(max_fee.get())
            }
            Domain::PlatformFeeAboveMax {
                platform_fee,
                max_fee,
            } => Self::PlatformFeeAboveMax {
                platform_fee_bps: platform_fee.get(),
                max_fee_bps: max_fee.get(),
            },
            Domain::EmptyEcdsaKeyName => Self::EmptyEcdsaKeyName,
            Domain::TimerIntervalTooLong { field, interval } => Self::TimerIntervalTooLong {
                field: field.to_string(),
                interval_s: interval.as_secs(),
            },
            Domain::DurationAboveCap { field, duration } => Self::DurationAboveCap {
                field: field.to_string(),
                duration_s: duration.as_secs(),
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
            Domain::AmountTooLarge { field } => Self::AmountTooLarge {
                field: field.to_string(),
            },
            Domain::DurationTooLong { field } => Self::DurationTooLong {
                field: field.to_string(),
            },
            Domain::RedactedRpcUrl { chain } => Self::RedactedRpcUrl {
                chain_id: chain.get(),
            },
            Domain::VaultAddressTooLong { chain, len } => Self::VaultAddressTooLong {
                chain_id: chain.get(),
                len: crate::types::wire_len(len),
            },
            Domain::EmptyVaultAddress { chain } => Self::EmptyVaultAddress {
                chain_id: chain.get(),
            },
            Domain::EmptyRpcUrl { chain } => Self::EmptyRpcUrl {
                chain_id: chain.get(),
            },
        }
    }
}

#[cfg(test)]
mod tests;
