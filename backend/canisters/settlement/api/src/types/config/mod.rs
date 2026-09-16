use candid::CandidType;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt;

/// Every knob the canister reads at runtime. Numbers are the spec defaults; the two
/// address maps and the key name are what a deploy fills in.
// No `Debug` in the derive: it is hand-written below, because `rpc_urls` holds secrets.
// (Kept out of the doc comment above, which the extractor copies into can.did.)
#[derive(CandidType, Deserialize, Clone, PartialEq)]
pub struct Config {
    pub platform_fee_bps: u16,
    pub max_fee_bps: u16,
    pub max_swap_usd: u64,
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
        Self {
            platform_fee_bps: 0, // adjustable knob, zero at launch
            max_fee_bps: 30,     // hard ceiling the canister enforces on itself
            max_swap_usd: 1_000, // launch cap, canister-side backstop
            quote_ttl_s: 45,
            permit_deadline_s: 120,
            chain_data_max_age_s: 10,
            batch_window_ms: 2_000,
            max_batch_items: 10,
            decision_timeout_min: 30,
            rail_status_max_age_s: 30,
            simulate_before_sign: false, // the layer is built later, disabled by config
            expiry_check_interval_s: 60,
            replay_audit_interval_s: 21_600,
            confirmations: BTreeMap::from([
                (1, 1),
                (8453, 1),
                (56, 1),
                (137, 6),
                (42161, 1),
                // remaining chains: numbers from jeff pending, keep flexible
            ]),
            rpc_urls: BTreeMap::new(),
            vault_addresses: BTreeMap::new(),
            ecdsa_key_name: "dfx_test_key".to_string(), // "key_1" in prod
        }
    }
}

/// The longest a timer interval may be: one year. The timers crate only traps centuries
/// out, but nothing legitimate waits this long, and a tight bound fails at set time.
pub const MAX_TIMER_INTERVAL_S: u64 = 31_536_000;

/// Which timer intervals a config write moved, so `set_config` restarts only those timers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IntervalChanges {
    pub expiry: bool,
    pub audit: bool,
}

/// What a blanked secret reads as. Values only, so a reader still learns which chains are
/// configured.
const REDACTED: &str = "***";

/// The blanking rule itself: chain ids kept, values gone. Shared by the two mechanisms
/// that must never leak, so they cannot drift apart.
fn blank_rpc_urls(rpc_urls: &BTreeMap<u64, String>) -> BTreeMap<u64, String> {
    rpc_urls
        .keys()
        .map(|chain| (*chain, REDACTED.to_string()))
        .collect()
}

impl Config {
    /// The only view the public may see. An rpc url *is* its api key (an Alchemy url
    /// carries the key in the path), so the values are blanked and the chain ids kept.
    /// This is the single redaction point for the api surface: any field added later that
    /// can hold a secret must be blanked here.
    pub fn redacted(&self) -> Config {
        // exhaustive destructure: a new field breaks this line, forcing a redact-or-not
        // decision. `..self.clone()` would have forwarded it into the public view silently.
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
        } = self.clone();
        Config {
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
            // the one secret in the record
            rpc_urls: blank_rpc_urls(&rpc_urls),
            // both of these are public by nature: an address is on-chain already, and the
            // key *name* selects a key it never reveals
            vault_addresses,
            ecdsa_key_name,
        }
    }

    /// Which timer intervals `new` moves, one flag per timer.
    pub fn interval_changes(&self, new: &Config) -> IntervalChanges {
        IntervalChanges {
            expiry: self.expiry_check_interval_s != new.expiry_check_interval_s,
            audit: self.replay_audit_interval_s != new.replay_audit_interval_s,
        }
    }

    /// What a stored config must satisfy. Called at the write chokepoint, so nothing that
    /// fails here reaches the cell or the log. The fee relation is the record-coherence
    /// half only; the fee computation clamps against `max_fee_bps` on its own.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_fee_bps > 10_000 {
            return Err(format!(
                "max_fee_bps is {}, above the 10000 bps that is 100%",
                self.max_fee_bps
            ));
        }
        if self.platform_fee_bps > self.max_fee_bps {
            return Err(format!(
                "platform_fee_bps is {}, above max_fee_bps {}",
                self.platform_fee_bps, self.max_fee_bps
            ));
        }
        if self.ecdsa_key_name.is_empty() {
            return Err("ecdsa_key_name is empty".to_string());
        }
        for (field, seconds) in [
            ("expiry_check_interval_s", self.expiry_check_interval_s),
            ("replay_audit_interval_s", self.replay_audit_interval_s),
        ] {
            if seconds > MAX_TIMER_INTERVAL_S {
                return Err(format!(
                    "{field} is {seconds}, above the one-year cap of {MAX_TIMER_INTERVAL_S}"
                ));
            }
        }
        // the read-modify-write guard: storing the public view back would put "***" where
        // a provider url belongs and cut the canister off from its rpc
        for (chain, url) in &self.rpc_urls {
            if url == REDACTED {
                return Err(format!(
                    "rpc_urls[{chain}] is the redacted placeholder {REDACTED:?}: \
                     read with get_config_full, not get_config, before writing"
                ));
            }
        }
        Ok(())
    }
}

// Hand-written so a stray `{:?}` cannot leak an api key: into a log line, a trap message,
// a test failure. It blanks exactly what `redacted()` blanks, which keeps the logged event
// json identical whichever of the two the caller went through. `redacted()` stays the
// api-surface mechanism; this is the backstop under it.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // exhaustive destructure: a new field breaks this line, forcing a redact-or-not
        // decision
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
        } = self;
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
            .field("rpc_urls", &blank_rpc_urls(rpc_urls))
            .field("vault_addresses", vault_addresses)
            .field("ecdsa_key_name", ecdsa_key_name)
            .finish()
    }
}

#[cfg(test)]
mod tests;
