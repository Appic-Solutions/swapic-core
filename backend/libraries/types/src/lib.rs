//! Domain types of the settlement canister. Everything here is pure: no canister calls,
//! and every rule a value must satisfy is checked where the value is built.

pub mod address;
pub mod canonical;
pub mod chain;
pub mod checked_amount;
pub mod config;
pub mod events;
pub mod hash;
pub mod numeric;
pub mod quote;
pub mod rail;
pub mod swap;

pub use address::{Address, RpcUrl, TokenId};
pub use chain::ChainId;
pub use checked_amount::CheckedAmountOf;
pub use config::{Config, ConfigError};
pub use events::{Choice, Event, EventType};
pub use hash::{EventHash, QuoteHash, TxHash};
pub use numeric::{
    Attempt, BasisPoints, BlockDepth, BlockNumber, EventIndex, Timestamp, TokenAmount, UnixSeconds,
    UsdAmount,
};
pub use quote::{GasMode, Quote, QuoteError};
pub use rail::Rail;
pub use swap::{Pocket, Swap, SwapStatus};
