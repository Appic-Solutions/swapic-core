//! Update endpoints, grouped by domain.
//!
//! Group modules are private and only their leaves are re-exported, so
//! `settlement_api::updates::set_config` keeps resolving while the group names never
//! reach the crate root, which is what lets queries and updates share group names
//! like `config` without `lib.rs`'s glob imports becoming ambiguous.

mod config;
mod ops;
mod quotes;
mod roles;

pub use config::set_config;
pub use ops::{audit_replay_step, set_halted};
pub use quotes::{clear_pending_quotes, register_quote};
pub use roles::set_roles;
