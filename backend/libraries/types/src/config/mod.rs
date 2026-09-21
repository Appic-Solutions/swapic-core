#[cfg(test)]
mod tests;

use crate::address::{Address, RpcUrl};
use crate::chain::ChainId;
use crate::numeric::{BasisPoints, BlockDepth, UsdAmount};
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
    #[error("{field} is {cap}, outside the range 1 to {ceiling}")]
    CapOutOfRange {
        field: &'static str,
        cap: u32,
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
            max_refunds_per_sweep: RefundsPerSweep::DEFAULT,
            max_evictions_per_sweep: EvictionsPerSweep::DEFAULT,
            audit_chunk_events: AuditChunk::DEFAULT,
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

    /// Which timer intervals `new` moves, one flag per timer.
    pub fn interval_changes(&self, new: &Config) -> IntervalChanges {
        IntervalChanges {
            expiry: self.expiry_check_interval != new.expiry_check_interval,
            audit: self.replay_audit_interval != new.replay_audit_interval,
        }
    }
}

crate::storable_as_cbor!(Config);
