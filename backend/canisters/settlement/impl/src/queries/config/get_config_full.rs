use crate::guards::require_controller;
use crate::storage::config;
use ic_cdk::query;
pub use settlement_api::types::config::Config;

/// The unredacted config, for ops. Controller-only, because `rpc_urls` holds api keys.
#[query]
pub fn get_config_full() -> Result<Config, String> {
    require_controller()?;
    Ok(Config::unredacted(config::get()))
}
