//! Update endpoints, grouped by domain.
//!
//! Group modules are private and re-exported flat, so `export_candid!()` still sees
//! every endpoint's types at crate scope, and queries and updates can share group names
//! without `lib.rs`'s glob imports becoming ambiguous.

mod chain;
mod config;
mod ops;
mod quotes;
mod rails;
mod roles;
mod swaps;

pub use chain::*;
pub use config::*;
pub use ops::*;
pub use quotes::*;
pub use rails::*;
pub use roles::*;
pub use swaps::*;
