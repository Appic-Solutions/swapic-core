//! Update endpoints, grouped by domain.
//!
//! Group modules are private and re-exported flat, so `export_candid!()` still sees
//! every endpoint's types at crate scope, and queries and updates can share group names
//! without `lib.rs`'s glob imports becoming ambiguous.

mod books;
mod config;
mod flows;
mod icrc21;
mod ops;
mod pool;
mod rebalance;
mod rescue;
mod roles;
mod solana;
mod sweep;

pub use books::*;
pub use config::*;
pub use flows::*;
pub use icrc21::*;
pub use ops::*;
pub use pool::*;
pub use rebalance::*;
pub use rescue::*;
pub use roles::*;
pub use solana::*;
pub use sweep::*;
