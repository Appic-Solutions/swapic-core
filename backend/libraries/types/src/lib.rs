//! Domain types of the settlement canister: identifiers, amounts, the canonical codecs,
//! and the rules values must satisfy. Everything here is pure, with no canister calls.

pub mod abi;
pub mod address;
pub mod canonical;
pub mod chain;
pub mod chain_data;
pub mod checked_amount;
pub mod config;
pub mod events;
pub mod evm;
pub mod hash;
pub mod ledger;
pub mod numeric;
pub mod quote;
pub mod rail;
pub mod storable;
pub mod swap;

pub use address::{Address, RpcUrl, TokenId};
pub use chain::ChainId;
pub use chain_data::{ChainData, ChainDataError, ChainReading};
pub use checked_amount::CheckedAmountOf;
pub use config::{Config, ConfigError};
pub use events::{Choice, Event, EventType};
pub use evm::{EcdsaSignature, Eip1559Tx, EvmAddress, SignedTx};
pub use hash::{EventHash, QuoteHash, TxHash};
pub use ledger::LedgerMeta;
pub use numeric::{
    Attempt, BasisPoints, BlockDepth, BlockNumber, EventIndex, GasAmount, Nonce, Timestamp,
    TokenAmount, UnixSeconds, UsdAmount, Wei, WeiPerGas,
};
pub use quote::{ExpiryKey, GasMode, Quote, QuoteError};
pub use rail::Rail;
pub use swap::{Pocket, PocketError, Swap, SwapStatus, TransitionError, WaitingKey};
