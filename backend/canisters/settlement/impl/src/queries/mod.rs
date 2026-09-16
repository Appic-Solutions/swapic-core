//! Query endpoints, grouped by domain.
//!
//! Group modules are private and re-exported flat, so `export_candid!()` still sees
//! every endpoint's types at crate scope while the group names never reach the crate
//! root (`lib.rs` glob-imports queries and updates together).

mod config;
mod events;
mod ops;
mod quotes;
mod swaps;

pub use config::*;
pub use events::*;
pub use ops::*;
pub use quotes::*;
pub use swaps::*;
