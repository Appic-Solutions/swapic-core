//! Query endpoints, grouped by domain.
//!
//! Group modules are private and re-exported flat, so `export_candid!()` still sees
//! every endpoint's types at crate scope while the group names never reach the crate
//! root (`lib.rs` glob-imports queries and updates together).

mod books;
mod config;
mod events;
mod finalized;
mod flows;
mod icrc10;
mod ops;
mod pool;
mod rebalance;
mod solana;

pub use books::*;
pub use config::*;
pub use events::*;
pub use finalized::*;
pub use flows::*;
pub use icrc10::*;
pub use ops::*;
pub use pool::*;
pub use rebalance::*;
pub use solana::*;
