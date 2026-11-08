//! CCTP V2 as Swapic touches it: the message offsets, and the instruction data of the burn, the
//! receive and the reclaim. Offsets are Circle's (and `CctpReceiver.sol`'s), big-endian.

use crate::errors::SwapicError;
use crate::Pubkey;

pub const HEADER_VERSION: core::ops::Range<usize> = 0..4;
pub const SOURCE_DOMAIN: core::ops::Range<usize> = 4..8;
pub const DESTINATION_DOMAIN: core::ops::Range<usize> = 8..12;
pub const NONCE: core::ops::Range<usize> = 12..44;
pub const SENDER: core::ops::Range<usize> = 44..76;
pub const RECIPIENT: core::ops::Range<usize> = 76..108;
pub const DESTINATION_CALLER: core::ops::Range<usize> = 108..140;
pub const BODY_VERSION: core::ops::Range<usize> = 148..152;
pub const BURN_TOKEN: core::ops::Range<usize> = 152..184;
pub const MINT_RECIPIENT: core::ops::Range<usize> = 184..216;
pub const AMOUNT: core::ops::Range<usize> = 216..248;
pub const MESSAGE_SENDER: core::ops::Range<usize> = 248..280;
pub const MAX_FEE: core::ops::Range<usize> = 280..312;
pub const FEE_EXECUTED: core::ops::Range<usize> = 312..344;
pub const EXPIRATION_BLOCK: core::ops::Range<usize> = 344..376;
pub const HOOK_AT: usize = 376;

/// `sha256("global:deposit_for_burn_with_hook")[..8]`
pub const DEPOSIT_FOR_BURN_WITH_HOOK: [u8; 8] = [111, 245, 62, 131, 204, 108, 223, 155];
/// `sha256("global:receive_message")[..8]`
pub const RECEIVE_MESSAGE: [u8; 8] = [38, 144, 127, 225, 31, 225, 238, 25];
/// `sha256("global:reclaim_event_account")[..8]`
pub const RECLAIM_EVENT_ACCOUNT: [u8; 8] = [94, 198, 180, 159, 131, 236, 15, 174];

/// Anchor's event-CPI tag, then the discriminator of `DepositForBurn`.
pub const EVENT_CPI_TAG: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];
pub const DEPOSIT_FOR_BURN_EVENT: [u8; 8] = [0x90, 0xfc, 0x91, 0x92, 0x06, 0x4a, 0xa7, 0xeb];

pub const FINALITY_FAST: u32 = 1000;
pub const FINALITY_STANDARD: u32 = 2000;

/// A message split into the fields the program and the canister rule on.
#[derive(Copy, Clone, Debug)]
pub struct Message<'a> {
    pub bytes: &'a [u8],
}

impl<'a> Message<'a> {
    /// The fixed part must be there; the hook may be any length.
    pub fn new(bytes: &'a [u8]) -> Result<Self, SwapicError> {
        if bytes.len() < HOOK_AT {
            return Err(SwapicError::BadMessageLength);
        }
        Ok(Self { bytes })
    }

    fn u32_at(&self, range: core::ops::Range<usize>) -> u32 {
        let mut b = [0u8; 4];
        b.copy_from_slice(&self.bytes[range]);
        u32::from_be_bytes(b)
    }

    fn word(&self, range: core::ops::Range<usize>) -> &'a [u8; 32] {
        let bytes: &'a [u8] = self.bytes;
        bytes[range].try_into().expect("a 32-byte range")
    }

    pub fn header_version(&self) -> u32 {
        self.u32_at(HEADER_VERSION)
    }
    pub fn source_domain(&self) -> u32 {
        self.u32_at(SOURCE_DOMAIN)
    }
    pub fn destination_domain(&self) -> u32 {
        self.u32_at(DESTINATION_DOMAIN)
    }
    pub fn nonce(&self) -> &'a [u8; 32] {
        self.word(NONCE)
    }
    pub fn sender(&self) -> &'a [u8; 32] {
        self.word(SENDER)
    }
    pub fn recipient(&self) -> &'a [u8; 32] {
        self.word(RECIPIENT)
    }
    pub fn destination_caller(&self) -> &'a [u8; 32] {
        self.word(DESTINATION_CALLER)
    }
    pub fn body_version(&self) -> u32 {
        self.u32_at(BODY_VERSION)
    }
    pub fn burn_token(&self) -> &'a [u8; 32] {
        self.word(BURN_TOKEN)
    }
    pub fn mint_recipient(&self) -> &'a [u8; 32] {
        self.word(MINT_RECIPIENT)
    }
    pub fn amount_word(&self) -> &'a [u8; 32] {
        self.word(AMOUNT)
    }
    pub fn message_sender(&self) -> &'a [u8; 32] {
        self.word(MESSAGE_SENDER)
    }
    pub fn max_fee_word(&self) -> &'a [u8; 32] {
        self.word(MAX_FEE)
    }
    pub fn fee_executed_word(&self) -> &'a [u8; 32] {
        self.word(FEE_EXECUTED)
    }
    pub fn hook(&self) -> &'a [u8] {
        let bytes: &'a [u8] = self.bytes;
        &bytes[HOOK_AT..]
    }
}

/// A word that fits 64 bits, else `None`.
pub fn word_to_u64(word: &[u8; 32]) -> Option<u64> {
    if word[..24].iter().any(|b| *b != 0) {
        return None;
    }
    let mut b = [0u8; 8];
    b.copy_from_slice(&word[24..]);
    Some(u64::from_be_bytes(b))
}

pub const BURN_DATA_LEN: usize = 420;

/// The arguments of `deposit_for_burn_with_hook` with a 320-byte hook.
#[derive(Copy, Clone, Debug)]
pub struct BurnData<'a> {
    pub amount: u64,
    pub domain: u32,
    pub mint_recipient: &'a [u8; 32],
    pub destination_caller: &'a [u8; 32],
    pub max_fee: u64,
    pub min_finality: u32,
    pub hook: &'a [u8; 320],
}

impl BurnData<'_> {
    pub fn encode(&self) -> [u8; BURN_DATA_LEN] {
        let mut d = [0u8; BURN_DATA_LEN];
        d[0..8].copy_from_slice(&DEPOSIT_FOR_BURN_WITH_HOOK);
        d[8..16].copy_from_slice(&self.amount.to_le_bytes());
        d[16..20].copy_from_slice(&self.domain.to_le_bytes());
        d[20..52].copy_from_slice(self.mint_recipient);
        d[52..84].copy_from_slice(self.destination_caller);
        d[84..92].copy_from_slice(&self.max_fee.to_le_bytes());
        d[92..96].copy_from_slice(&self.min_finality.to_le_bytes());
        d[96..100].copy_from_slice(&320u32.to_le_bytes());
        d[100..420].copy_from_slice(self.hook);
        d
    }
}

/// `ceil(amount * deci_bps / 100_000)`: the burn's `maxFee` from its deci-bps cap.
pub const fn max_fee_of(amount: u64, deci_bps: u16) -> u64 {
    ((amount as u128 * deci_bps as u128 + 99_999) / 100_000) as u64
}

/// The length of `receive_message`'s instruction data for this message and attestation.
pub const fn receive_data_len(message_len: usize, attestation_len: usize) -> usize {
    8 + 4 + message_len + 4 + attestation_len
}

/// Writes `receive_message`'s data into `out`, as long as [`receive_data_len`] says.
pub fn write_receive_data(out: &mut [u8], message: &[u8], attestation: &[u8]) {
    write_two_vecs(out, &RECEIVE_MESSAGE, message, attestation);
}

/// The length of `reclaim_event_account`'s instruction data.
pub const fn reclaim_data_len(attestation_len: usize, message_len: usize) -> usize {
    8 + 4 + attestation_len + 4 + message_len
}

/// Writes `reclaim_event_account`'s data into `out`: its parameters are attestation, then message.
pub fn write_reclaim_data(out: &mut [u8], attestation: &[u8], destination_message: &[u8]) {
    write_two_vecs(out, &RECLAIM_EVENT_ACCOUNT, attestation, destination_message);
}

fn write_two_vecs(out: &mut [u8], discriminator: &[u8; 8], first: &[u8], second: &[u8]) {
    out[..8].copy_from_slice(discriminator);
    let mut at = 8;
    for part in [first, second] {
        out[at..at + 4].copy_from_slice(&(part.len() as u32).to_le_bytes());
        at += 4;
        out[at..at + part.len()].copy_from_slice(part);
        at += part.len();
    }
}

/// CCTP's `DepositForBurn` event, read from the inner instruction of a source transaction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DepositForBurnEvent<'a> {
    pub burn_token: &'a Pubkey,
    pub amount: u64,
    pub depositor: &'a Pubkey,
    pub mint_recipient: &'a Pubkey,
    pub destination_domain: u32,
    pub destination_token_messenger: &'a Pubkey,
    pub destination_caller: &'a Pubkey,
    pub max_fee: u64,
    pub min_finality_threshold: u32,
    pub hook: &'a [u8],
}

impl<'a> DepositForBurnEvent<'a> {
    /// `data` is the whole inner instruction data, the event-CPI tag first.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        const FIXED: usize = 16 + 32 + 8 + 32 + 32 + 4 + 32 + 32 + 8 + 4 + 4;
        if data.len() < FIXED || data[..8] != EVENT_CPI_TAG || data[8..16] != DEPOSIT_FOR_BURN_EVENT {
            return None;
        }
        let key = |i: usize| -> &'a Pubkey { data[i..i + 32].try_into().expect("32 bytes") };
        let u64_at = |i: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&data[i..i + 8]);
            u64::from_le_bytes(b)
        };
        let u32_at = |i: usize| {
            let mut b = [0u8; 4];
            b.copy_from_slice(&data[i..i + 4]);
            u32::from_le_bytes(b)
        };
        let hook_len = u32_at(FIXED - 4) as usize;
        if data.len() != FIXED + hook_len {
            return None;
        }
        Some(Self {
            burn_token: key(16),
            amount: u64_at(48),
            depositor: key(56),
            mint_recipient: key(88),
            destination_domain: u32_at(120),
            destination_token_messenger: key(124),
            destination_caller: key(156),
            max_fee: u64_at(188),
            min_finality_threshold: u32_at(196),
            hook: &data[FIXED..],
        })
    }
}

#[cfg(test)]
mod tests;
