pub mod books;
pub mod chain_data;
pub mod config;
pub mod errors;
pub mod events;
pub mod evm;
pub mod flow;
pub mod icrc21;
pub mod init;
pub mod pool;
pub mod solana;
pub mod rpc;
pub mod sweep;
pub mod transition;
pub mod tx;

/// A length or position as the wire carries it.
pub fn wire_len(len: usize) -> u64 {
    u64::try_from(len).expect("BUG: usize is at most 64 bits on every target")
}
