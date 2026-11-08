pub mod agreed;
pub mod books;
pub mod cctp;
pub mod consent;
pub mod ecdsa;
pub mod ed25519;
pub mod engine;
pub mod evm_reads;
pub mod gas;
pub mod guards;
pub mod ic;
pub mod lifecycle;
pub mod pool;
pub mod queries;
pub mod relay;
pub mod rescue;
pub mod rpc;
pub mod solana;
pub mod state;
pub mod storage;
pub mod sweep;
pub mod task_manager;
#[cfg(test)]
mod test_util;
#[cfg(test)]
mod tests;
pub mod tx;
pub mod updates;

// what `export_candid!` reads the doors' types through
#[cfg(any(test, feature = "export-candid"))]
use lifecycle::*;
use queries::*;
#[cfg(any(test, feature = "export-candid"))]
use updates::*;

#[cfg(any(test, feature = "export-candid"))]
ic_cdk::export_candid!();
