//! Every byte layout of the Swapic Solana line, in one place, so the program, the canister,
//! the quoter and the app's TypeScript port cannot drift apart. The app's port reads the same
//! vector files under `vectors/` in its tests.
//!
//! The default build allocates nothing and links no curve code: it is what the program uses.
//! `alloc` adds the message compilers; `offchain` adds PDA derivation, keccak and base58.
#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod args;
pub mod cctp;
pub mod consts;
pub mod envelope;
pub mod errors;
pub mod jupiter;

#[cfg(feature = "offchain")]
pub mod base58;
#[cfg(feature = "offchain")]
pub mod instructions;
#[cfg(feature = "alloc")]
pub mod message;
#[cfg(feature = "offchain")]
pub mod pda;

/// A 32-byte Solana address.
pub type Pubkey = [u8; 32];
