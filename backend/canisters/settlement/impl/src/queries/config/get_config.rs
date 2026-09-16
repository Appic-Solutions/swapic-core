use crate::storage::config;
use ic_cdk::query;
pub use settlement_api::types::config::Config;

/// Public, so it answers with the redacted view; `get_config_full` is the ops door onto
/// the real thing.
#[query]
pub fn get_config() -> Config {
    Config::from(config::get())
}
