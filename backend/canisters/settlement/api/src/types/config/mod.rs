use candid::{CandidType, Nat};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;
use types::address::{RedactedRpcUrl, TextTooLong};
use types::config::ConfigError;
use types::{BasisPoints, BlockDepth, ChainId, UsdAmount};

/// Every knob the canister reads at runtime. Numbers are the spec defaults; the two
/// address maps and the key name are what a deploy fills in.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
pub struct Config {
    pub platform_fee_bps: u16,
    pub max_fee_bps: u16,
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
}

impl Default for Config {
    fn default() -> Self {
        Self::unredacted(types::Config::default())
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
        }
    }
}

impl TryFrom<Config> for types::Config {
    type Error = ConfigError;

    fn try_from(config: Config) -> Result<Self, Self::Error> {
        Ok(Self {
            platform_fee: BasisPoints::new(config.platform_fee_bps),
            max_fee: BasisPoints::new(config.max_fee_bps),
            max_swap: UsdAmount::try_from(config.max_swap_usd).map_err(|_| {
                ConfigError::AmountTooLarge {
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
                .ok_or(ConfigError::DurationTooLong {
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
                        .map_err(|RedactedRpcUrl| ConfigError::RedactedRpcUrl { chain })
                })
                .collect::<Result<_, _>>()?,
            vault_addresses: config
                .vault_addresses
                .into_iter()
                .map(|(chain, address)| {
                    let chain = ChainId::new(chain);
                    address.parse().map(|address| (chain, address)).map_err(
                        |TextTooLong { len }| ConfigError::VaultAddressTooLong { chain, len },
                    )
                })
                .collect::<Result<_, _>>()?,
            ecdsa_key_name: config.ecdsa_key_name,
        })
    }
}

#[cfg(test)]
mod tests;
