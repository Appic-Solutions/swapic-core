#[cfg(test)]
mod tests;

use crate::address::{Address, RpcUrl};
use crate::chain::ChainId;
use crate::numeric::{BasisPoints, BlockDepth, UsdAmount};
use minicbor::{Decode, Encode};
use std::collections::BTreeMap;
use std::time::Duration;
use thiserror::Error;

/// The longest a timer interval may be: one year. Nothing legitimate waits longer, and a
/// tight bound fails at set time rather than when the timer is wired.
pub const MAX_TIMER_INTERVAL: Duration = Duration::from_secs(31_536_000);

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
    #[n(7)]
    pub max_batch_items: u32,
    /// How long a paused swap waits for its user.
    #[n(8)]
    pub decision_timeout: Duration,
    #[n(9)]
    pub rail_status_max_age: Duration,
    #[n(10)]
    pub simulate_before_sign: bool,
    #[n(11)]
    pub expiry_check_interval: Duration,
    #[n(12)]
    pub replay_audit_interval: Duration,
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
    #[error("{field} is {}s, above the one-year cap of {}s", interval.as_secs(), MAX_TIMER_INTERVAL.as_secs())]
    TimerIntervalTooLong {
        field: &'static str,
        interval: Duration,
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
}

/// Which timer intervals a config write moved, so only those timers restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntervalChanges {
    pub expiry: bool,
    pub audit: bool,
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
            // the simulation layer is built later
            simulate_before_sign: false,
            expiry_check_interval: Duration::from_secs(60),
            replay_audit_interval: Duration::from_secs(21_600),
            confirmations: BTreeMap::from([
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
        for (field, interval) in [
            ("expiry_check_interval_s", self.expiry_check_interval),
            ("replay_audit_interval_s", self.replay_audit_interval),
        ] {
            if interval > MAX_TIMER_INTERVAL {
                return Err(ConfigError::TimerIntervalTooLong { field, interval });
            }
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

    /// Which timer intervals `new` moves, one flag per timer.
    pub fn interval_changes(&self, new: &Config) -> IntervalChanges {
        IntervalChanges {
            expiry: self.expiry_check_interval != new.expiry_check_interval,
            audit: self.replay_audit_interval != new.replay_audit_interval,
        }
    }
}

crate::storable_as_cbor!(Config);
