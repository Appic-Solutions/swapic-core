//! Query endpoints, grouped by domain.
//!
//! The group modules are PRIVATE and only their leaves are re-exported, so
//! `settlement_api::queries::get_config` keeps resolving while the group names
//! never reach the crate root. That matters because `lib.rs` glob-imports
//! `queries::*`, `updates::*` and `types::*` together, and a group name shared
//! with any of them would be ambiguous.

mod chain;
mod config;
mod events;
mod ops;
mod quotes;
mod swaps;

pub use chain::get_chain_data;
pub use config::{get_config, get_config_full};
pub use events::{event_count, events_page, verify_chain, verify_replay};
pub use ops::{evm_address, halted, test_outbox_armed, version};
pub use quotes::get_pending;
pub use swaps::{get_swap, paused_swaps};
