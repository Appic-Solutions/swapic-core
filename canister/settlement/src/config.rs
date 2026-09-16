use crate::events::Event;
use crate::log::{self, Memory, CONFIG_MEMORY};
use candid::CandidType;
use ic_stable_structures::StableCell;
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;

/// Every knob the canister reads at runtime. Numbers are the spec defaults; the two
/// address maps and the key name are what a deploy fills in.
// No `Debug` in the derive: it is hand-written below, because `rpc_urls` holds secrets.
// (Kept out of the doc comment above, which the extractor copies into settlement.did.)
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

    /// Whether the two configs wire the timers differently, which is when `set_config`
    /// has to restart them.
    pub fn timer_intervals_differ(&self, other: &Config) -> bool {
        self.expiry_check_interval_s != other.expiry_check_interval_s
            || self.replay_audit_interval_s != other.replay_audit_interval_s
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

fn encode(config: &Config) -> Vec<u8> {
    candid::encode_one(config).expect("config encodes")
}

thread_local! {
    // The live copy every reader sees. It is a cache of STORED, not a second source.
    static CONFIG: RefCell<Config> = RefCell::new(Config::default());

    // The copy that survives an upgrade: config is deploy-time truth, not something the
    // log replays, so it is held here rather than folded back out of the events.
    static STORED: RefCell<StableCell<Vec<u8>, Memory>> = RefCell::new(
        StableCell::init(log::memory(CONFIG_MEMORY), encode(&Config::default()))
            .expect("config cell init"),
    );
}

/// Called from `init` and `post_upgrade`: on a fresh install this writes the defaults to
/// the cell, on an upgrade it reads back whatever was set. Update contexts only, because
/// the first touch of the cell grows stable memory.
pub fn load() {
    // a config that no longer decodes traps the upgrade, which leaves the canister on its
    // working wasm; the alternative, falling back to defaults, would silently drop the
    // deploy's rpc urls and vault addresses
    let stored: Config = candid::decode_one(STORED.with(|s| s.borrow().get().clone()).as_slice())
        .expect("config decodes");
    CONFIG.with(|c| *c.borrow_mut() = stored);
}

/// Cloned snapshot: there is no handle onto the live value.
pub fn get() -> Config {
    CONFIG.with(|c| c.borrow().clone())
}

/// Writes the cell and the heap copy, and records the change in the log. Callers do the
/// authorization; this is the storage path. Returns whether a timer interval changed, so
/// the caller knows to rewire the timers.
pub fn set(new: Config) -> Result<bool, String> {
    // before anything is written: a rejected config must leave no event and no cell write
    new.validate()?;
    let intervals_changed = get().timer_intervals_differ(&new);
    // the log first, because it is the step that can refuse: the event records THAT the
    // config changed and to what, while the cell below stays the operative copy. The log
    // is world-readable through `events_page`, so what goes in it is the redacted view.
    log::append_event(Event::ConfigChanged {
        json: format!("{:?}", new.redacted()),
    })?;
    // out of stable memory is not a caller error, so it traps instead of returning Err:
    // a trap rolls the append above back with it, and an `Ok(Err(_))` would not
    STORED.with(|s| s.borrow_mut().set(encode(&new)).expect("config cell write"));
    CONFIG.with(|c| *c.borrow_mut() = new);
    Ok(intervals_changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults are spec numbers, not preferences: this test is the spec.
    #[test]
    fn defaults_match_spec() {
        let c = Config::default();
        assert_eq!(c.platform_fee_bps, 0);
        assert_eq!(c.max_fee_bps, 30);
        assert_eq!(c.max_swap_usd, 1_000);
        assert_eq!(c.quote_ttl_s, 45);
        assert_eq!(c.permit_deadline_s, 120);
        assert_eq!(c.chain_data_max_age_s, 10);
        assert_eq!(c.batch_window_ms, 2_000);
        assert_eq!(c.max_batch_items, 10);
        assert_eq!(c.decision_timeout_min, 30);
        assert_eq!(c.rail_status_max_age_s, 30);
        assert!(!c.simulate_before_sign);
        assert_eq!(c.expiry_check_interval_s, 60);
        assert_eq!(c.replay_audit_interval_s, 21_600);
        assert_eq!(
            c.confirmations,
            BTreeMap::from([(1, 1), (8453, 1), (56, 1), (137, 6), (42161, 1)])
        );
        // deploy-time values, empty in code
        assert!(c.rpc_urls.is_empty());
        assert!(c.vault_addresses.is_empty());
        assert_eq!(c.ecdsa_key_name, "dfx_test_key");
    }

    const SECRET: &str = "https://eth-mainnet.g.alchemy.com/v2/secret-key";

    /// What a real deploy looks like: two provider urls, both of them secrets.
    fn secret_bearing() -> Config {
        Config {
            platform_fee_bps: 10,
            rpc_urls: BTreeMap::from([
                (1, SECRET.to_string()),
                (8453, "https://base.example/rpc?key=hunter2".to_string()),
            ]),
            // a vault address is public on-chain data, not a secret: it must survive
            vault_addresses: BTreeMap::from([(1, "0xvault".to_string())]),
            ecdsa_key_name: "key_1".to_string(),
            ..Config::default()
        }
    }

    /// An rpc url embeds its api key, so the public view must blank the values and keep
    /// the chain ids, and must touch nothing else.
    #[test]
    fn redacted_blanks_rpc_urls_and_nothing_else() {
        let full = secret_bearing();
        let public = full.redacted();

        assert_eq!(
            public.rpc_urls,
            BTreeMap::from([(1, "***".to_string()), (8453, "***".to_string())]),
            "every value blanked, every chain id kept"
        );
        // put the original urls back: if that restores the whole record, nothing else moved
        assert_eq!(
            Config {
                rpc_urls: full.rpc_urls.clone(),
                ..public
            },
            full
        );
    }

    /// `redacted()` guards the api surface, but a stray `{:?}` anywhere would still print
    /// the keys, so Debug blanks them too.
    #[test]
    fn debug_never_prints_an_rpc_url() {
        let shown = format!("{:?}", secret_bearing());
        assert!(
            !shown.contains(SECRET),
            "a stray debug print leaked: {shown}"
        );
        assert!(
            !shown.contains("hunter2"),
            "a stray debug print leaked: {shown}"
        );
        assert!(shown.contains("***"));
        // the two blanking paths must agree, or the logged event json would drift
        assert_eq!(shown, format!("{:?}", secret_bearing().redacted()));
        // everything that is not a secret still prints
        assert!(shown.contains("0xvault") && shown.contains("key_1"));
    }

    #[test]
    fn defaults_are_valid() {
        Config::default()
            .validate()
            .expect("the defaults are valid");
    }

    fn rejects(config: Config, field: &str) {
        let err = config
            .validate()
            .expect_err("validate should reject this config");
        assert!(err.contains(field), "the error must name {field}: {err}");
    }

    #[test]
    fn validate_rejects_a_ceiling_above_one_hundred_percent() {
        rejects(
            Config {
                max_fee_bps: 10_001,
                ..Config::default()
            },
            "max_fee_bps",
        );
    }

    #[test]
    fn validate_rejects_a_platform_fee_above_the_ceiling() {
        rejects(
            Config {
                platform_fee_bps: 31,
                max_fee_bps: 30,
                ..Config::default()
            },
            "platform_fee_bps",
        );
    }

    #[test]
    fn validate_rejects_an_empty_ecdsa_key_name() {
        rejects(
            Config {
                ecdsa_key_name: String::new(),
                ..Config::default()
            },
            "ecdsa_key_name",
        );
    }

    /// The read-modify-write footgun: writing the public view back would brick rpc access.
    #[test]
    fn validate_rejects_the_redacted_placeholder_as_an_rpc_url() {
        rejects(secret_bearing().redacted(), "rpc_urls");
    }

    #[test]
    fn only_a_timer_interval_change_counts_as_a_timer_change() {
        let base = Config::default();
        let fee_only = Config {
            platform_fee_bps: 10,
            ..Config::default()
        };
        assert!(!base.timer_intervals_differ(&fee_only));
        let expiry = Config {
            expiry_check_interval_s: 61,
            ..Config::default()
        };
        assert!(base.timer_intervals_differ(&expiry));
        let audit = Config {
            replay_audit_interval_s: 21_601,
            ..Config::default()
        };
        assert!(base.timer_intervals_differ(&audit));
    }

    #[test]
    fn validate_rejects_a_timer_interval_above_a_year() {
        rejects(
            Config {
                expiry_check_interval_s: MAX_TIMER_INTERVAL_S + 1,
                ..Config::default()
            },
            "expiry_check_interval_s",
        );
        rejects(
            Config {
                replay_audit_interval_s: MAX_TIMER_INTERVAL_S + 1,
                ..Config::default()
            },
            "replay_audit_interval_s",
        );
        Config {
            expiry_check_interval_s: MAX_TIMER_INTERVAL_S,
            replay_audit_interval_s: MAX_TIMER_INTERVAL_S,
            ..Config::default()
        }
        .validate()
        .expect("a year exactly is allowed");
    }
}
