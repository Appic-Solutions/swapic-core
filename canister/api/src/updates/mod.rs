//! Update endpoints, grouped by domain.
//!
//! Group modules are private and only their leaves are re-exported, so
//! `settlement_api::updates::set_config` keeps resolving while the group names never
//! reach the crate root, which is what lets queries and updates share group names
//! like `config` without `lib.rs`'s glob imports becoming ambiguous.

mod config;
mod icrc21;
mod ops;
mod roles;
mod sweep;

pub use config::set_config;
pub use icrc21::icrc21_canister_call_consent_message;
pub use ops::{
    derive_evm_address, set_halted, set_sanctioned, test_rpc_batch, test_send, test_sign,
};
pub use roles::set_roles;
pub use sweep::{sweep_evm, sweep_icp};
