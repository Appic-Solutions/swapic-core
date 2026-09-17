//! Domain types of the settlement canister: identifiers, amounts, the canonical codecs,
//! and the rules values must satisfy. Everything here is pure, with no canister calls.

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
pub mod storable;
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
pub use swap::{Pocket, PocketError, Swap, SwapStatus, TransitionError, WaitingKey};
