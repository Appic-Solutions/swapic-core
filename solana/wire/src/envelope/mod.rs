//! The arrival instructions a CCTP burn carries as its hook data.
//!
//! - [`Envelope`]: the 320 bytes the EVM `CctpReceiver` decodes (`libraries/Envelope.sol`), for
//!   burns out of Solana. `abi.encode` of 10 static words, every narrow word with clean high bytes.
//! - [`SolanaEnvelope`]: the 110 bytes the Solana program reads, for burns into Solana.
//! - [`ForwarderHook`]: the two hooks Circle's Forwarder accepts, for USDC-only arrivals.

use crate::errors::SwapicError;
use crate::Pubkey;

pub const ENVELOPE_LEN: usize = 320;
pub const ENVELOPE_VERSION: u8 = 3;

/// What the arrival does, as `SwapicTypes.sol`'s `Action`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Action {
    Deliver = 0,
    Swap = 1,
    CkDeposit = 2,
}

impl Action {
    pub const fn from_bits(bits: u8) -> Result<Self, SwapicError> {
        match bits {
            0 => Ok(Self::Deliver),
            1 => Ok(Self::Swap),
            2 => Ok(Self::CkDeposit),
            _ => Err(SwapicError::BadAction),
        }
    }
}

/// The decoded Envelope. Its fields are `Envelope.Data` of the Solidity library.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub ref_bytes: [u8; 32],
    pub action: Action,
    pub deadline: u64,
    pub recipient: [u8; 20],
    pub token_out: [u8; 20],
    pub min_out: [u8; 32],
    pub principal: [u8; 32],
    pub subaccount: [u8; 32],
    pub min_gas: u64,
}

impl Envelope {
    pub fn encode(&self) -> [u8; ENVELOPE_LEN] {
        let mut w = [0u8; ENVELOPE_LEN];
        w[31] = ENVELOPE_VERSION;
        w[32..64].copy_from_slice(&self.ref_bytes);
        w[95] = self.action as u8;
        w[120..128].copy_from_slice(&self.deadline.to_be_bytes());
        w[140..160].copy_from_slice(&self.recipient);
        w[172..192].copy_from_slice(&self.token_out);
        w[192..224].copy_from_slice(&self.min_out);
        w[224..256].copy_from_slice(&self.principal);
        w[256..288].copy_from_slice(&self.subaccount);
        w[312..320].copy_from_slice(&self.min_gas.to_be_bytes());
        w
    }

    /// As `Envelope.decode`: exactly 320 bytes, version 3, every narrow word clean.
    pub fn parse(bytes: &[u8]) -> Result<Self, SwapicError> {
        if bytes.len() != ENVELOPE_LEN {
            return Err(SwapicError::BadHook);
        }
        let clean = |range: core::ops::Range<usize>| bytes[range].iter().all(|b| *b == 0);
        if !clean(0..31)
            || bytes[31] != ENVELOPE_VERSION
            || !clean(64..95)
            || !clean(96..120)
            || !clean(128..140)
            || !clean(160..172)
            || !clean(288..312)
        {
            return Err(SwapicError::BadHook);
        }
        Ok(Self {
            ref_bytes: take::<32>(&bytes[32..64]),
            action: Action::from_bits(bytes[95]).map_err(|_| SwapicError::BadHook)?,
            deadline: u64::from_be_bytes(take::<8>(&bytes[120..128])),
            recipient: take::<20>(&bytes[140..160]),
            token_out: take::<20>(&bytes[172..192]),
            min_out: take::<32>(&bytes[192..224]),
            principal: take::<32>(&bytes[224..256]),
            subaccount: take::<32>(&bytes[256..288]),
            min_gas: u64::from_be_bytes(take::<8>(&bytes[312..320])),
        })
    }
}

fn take<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(bytes);
    out
}

pub const SOLANA_ENVELOPE_LEN: usize = 110;
pub const SOLANA_ENVELOPE_VERSION: u8 = 1;
/// A Solana envelope's action: the program swaps, or only `release` may pay it.
pub const SOLANA_ACTION_DELIVER: u8 = 0;
pub const SOLANA_ACTION_SWAP: u8 = 1;

/// The arrival hook of a burn into the Swapic program (design 4.7).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SolanaEnvelope {
    pub ref_bytes: [u8; 32],
    pub action: u8,
    pub deadline: u32,
    pub recipient: Pubkey,
    pub mint_out: Pubkey,
    pub min_out: u64,
}

impl SolanaEnvelope {
    pub fn encode(&self) -> [u8; SOLANA_ENVELOPE_LEN] {
        let mut w = [0u8; SOLANA_ENVELOPE_LEN];
        w[0] = SOLANA_ENVELOPE_VERSION;
        w[1..33].copy_from_slice(&self.ref_bytes);
        w[33] = self.action;
        w[34..38].copy_from_slice(&self.deadline.to_le_bytes());
        w[38..70].copy_from_slice(&self.recipient);
        w[70..102].copy_from_slice(&self.mint_out);
        w[102..110].copy_from_slice(&self.min_out.to_le_bytes());
        w
    }

    /// Strict: `complete` and the canister read a hook only through this.
    pub fn parse(bytes: &[u8]) -> Result<Self, SwapicError> {
        if bytes.len() != SOLANA_ENVELOPE_LEN || bytes[0] != SOLANA_ENVELOPE_VERSION {
            return Err(SwapicError::BadHook);
        }
        Ok(Self {
            ref_bytes: take::<32>(&bytes[1..33]),
            action: bytes[33],
            deadline: u32::from_le_bytes(take::<4>(&bytes[34..38])),
            recipient: take::<32>(&bytes[38..70]),
            mint_out: take::<32>(&bytes[70..102]),
            min_out: u64::from_le_bytes(take::<8>(&bytes[102..110])),
        })
    }
}

/// What `release` reads of a hook, and nothing more (design 4.8): only a version, a deadline and a
/// recipient. A hook this does not read is paid to Swapic's fee account for a refund by hand.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ReleaseTerms {
    pub ref_bytes: [u8; 32],
    pub deadline: u32,
    pub recipient: Pubkey,
}

/// `None` when the hook is unreadable.
pub fn read_release_terms(hook: &[u8]) -> Option<ReleaseTerms> {
    if hook.len() < 70 || hook[0] != SOLANA_ENVELOPE_VERSION {
        return None;
    }
    Some(ReleaseTerms {
        ref_bytes: take::<32>(&hook[1..33]),
        deadline: u32::from_le_bytes(take::<4>(&hook[34..38])),
        recipient: take::<32>(&hook[38..70]),
    })
}

const FORWARD_MAGIC: &[u8; 12] = b"cctp-forward";
pub const FORWARD_HOOK_SHORT_LEN: usize = 32;
pub const FORWARD_HOOK_LONG_LEN: usize = 65;

/// The two forms of Circle's Forwarder hook (design decision 7), all big-endian.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ForwarderHook {
    /// The token account already exists: 32 bytes, nothing but the magic.
    Existing,
    /// The Forwarder creates the owner's USDC account: 65 bytes, it charges the setup fee.
    CreateFor(Pubkey),
}

impl ForwarderHook {
    /// Writes the hook into `out`, which must hold 65 bytes; answers its length.
    pub fn write(&self, out: &mut [u8; FORWARD_HOOK_LONG_LEN]) -> usize {
        *out = [0u8; FORWARD_HOOK_LONG_LEN];
        out[..12].copy_from_slice(FORWARD_MAGIC);
        match self {
            Self::Existing => FORWARD_HOOK_SHORT_LEN,
            Self::CreateFor(owner) => {
                out[28..32].copy_from_slice(&33u32.to_be_bytes());
                out[32] = 1;
                out[33..65].copy_from_slice(owner);
                FORWARD_HOOK_LONG_LEN
            }
        }
    }

    pub fn parse(hook: &[u8]) -> Option<Self> {
        if hook.len() < FORWARD_HOOK_SHORT_LEN
            || &hook[..12] != FORWARD_MAGIC
            || hook[12..24].iter().any(|b| *b != 0)
            || hook[24..28].iter().any(|b| *b != 0)
        {
            return None;
        }
        let declared = u32::from_be_bytes(take::<4>(&hook[28..32]));
        match (hook.len(), declared) {
            (FORWARD_HOOK_SHORT_LEN, 0) => Some(Self::Existing),
            (FORWARD_HOOK_LONG_LEN, 33) if hook[32] == 1 => Some(Self::CreateFor(take::<32>(&hook[33..65]))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
