pub mod chain_data;
pub mod config;
pub mod errors;
pub mod events;
pub mod init;
pub mod quote;
pub mod swap;

/// A length or position as the wire carries it.
pub(crate) fn wire_len(len: usize) -> u64 {
    u64::try_from(len).expect("BUG: usize is at most 64 bits on every target")
}
