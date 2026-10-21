//! Update endpoints, grouped by domain.
//!
//! Group modules are private and re-exported flat, so `export_candid!()` still sees
//! every endpoint's types at crate scope, and queries and updates can share group names
//! without `lib.rs`'s glob imports becoming ambiguous.

mod books;
mod chain;
mod config;
mod flows;
mod ops;
mod pool;
mod quotes;
mod rebalance;
mod rescue;
mod roles;

pub use books::*;
pub use chain::*;
pub use config::*;
pub use flows::*;
pub use ops::*;
pub use pool::*;
pub use quotes::*;
pub use rebalance::*;
pub use rescue::*;
pub use roles::*;
