use crate::events::Event;
use crate::log::{self, Memory, CONFIG_MEMORY};
use candid::CandidType;
use ic_stable_structures::StableCell;
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::BTreeMap;

/// Every knob the canister reads at runtime. Numbers are the spec defaults; the two
/// address maps and the key name are what a deploy fills in.
#[derive(CandidType, Deserialize, Clone, Debug, PartialEq)]
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

/// What a blanked secret reads as. Values only, so a reader still learns which chains are
/// configured.
const REDACTED: &str = "***";

impl Config {
    /// The only view the public may see. An rpc url *is* its api key (an Alchemy url
    /// carries the key in the path), so the values are blanked and the chain ids kept.
    /// This is the single redaction point: any field added later that can hold a secret
    /// must be blanked here, and every public surface must route through it.
    pub fn redacted(&self) -> Config {
        Config {
            rpc_urls: self
                .rpc_urls
                .keys()
                .map(|chain| (*chain, REDACTED.to_string()))
                .collect(),
            // vault addresses and the ecdsa key *name* are public by nature: an address is
            // on-chain already, and the name selects a key it never reveals
            ..self.clone()
        }
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
/// authorization; this is the storage path.
pub fn set(new: Config) -> Result<(), String> {
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
    Ok(())
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

    /// An rpc url embeds its api key, so the public view must blank the values and keep
    /// the chain ids, and must touch nothing else.
    #[test]
    fn redacted_blanks_rpc_urls_and_nothing_else() {
        let full = Config {
            platform_fee_bps: 10,
            rpc_urls: BTreeMap::from([
                (
                    1,
                    "https://eth-mainnet.g.alchemy.com/v2/secret-key".to_string(),
                ),
                (8453, "https://base.example/rpc?key=hunter2".to_string()),
            ]),
            // a vault address is public on-chain data, not a secret: it must survive
            vault_addresses: BTreeMap::from([(1, "0xvault".to_string())]),
            ecdsa_key_name: "key_1".to_string(),
            ..Config::default()
        };
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
}
