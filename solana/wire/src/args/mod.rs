//! The compact arguments of the program's instructions (design 4.6, 4.7, 4.9, 4.10).
//!
//! A decoded [`BurnArgs`] has passed every rule that needs nothing but its own bytes, so the
//! program, the canister's co-sign checker and the app's guard refuse the same things.

use crate::envelope::{Action, Envelope};
use crate::errors::SwapicError;

pub const TAG_SWAP_BURN: u8 = 1;
pub const TAG_BURN: u8 = 2;
pub const TAG_COMPLETE: u8 = 3;
pub const TAG_RELEASE: u8 = 4;
pub const TAG_RECLAIM: u8 = 5;
pub const TAG_SET_CONFIG: u8 = 6;

const FLAG_ACTION_MASK: u8 = 0b0000_0011;
const FLAG_FAST: u8 = 1 << 2;
const FLAG_SUBACCOUNT: u8 = 1 << 3;
const FLAG_EVENT_KEYPAIR: u8 = 1 << 4;
const FLAG_FLOAT: u8 = 1 << 5;
const FLAG_MEMO: u8 = 1 << 6;
const FLAG_RESERVED: u8 = 1 << 7;

/// Where a burn's ref comes from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RefSource<'a> {
    Direct(&'a [u8; 32]),
    /// A pool deposit's memo: the ref is its keccak256, so the terms are on chain (design decision 3).
    Memo(&'a [u8]),
}

/// What the action names beyond the recipient.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Payload<'a> {
    Deliver,
    /// `min_out` is big-endian, 1 to 32 bytes, not zero.
    Swap { token_out: &'a [u8; 20], min_out: &'a [u8] },
    CkDeposit { principal: &'a [u8; 32], subaccount: Option<&'a [u8; 32]> },
}

impl Payload<'_> {
    pub const fn action(&self) -> Action {
        match self {
            Self::Deliver => Action::Deliver,
            Self::Swap { .. } => Action::Swap,
            Self::CkDeposit { .. } => Action::CkDeposit,
        }
    }
}

/// The arguments of `swap_burn` (tag 1) and `burn` (tag 2).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BurnArgs<'a> {
    pub tag: u8,
    pub fast: bool,
    pub event_keypair: bool,
    pub float: bool,
    pub domain: u8,
    pub ref_source: RefSource<'a>,
    pub deadline: u32,
    pub recipient: &'a [u8; 20],
    pub payload: Payload<'a>,
    pub min_gas: u32,
    pub max_fee_deci_bps: u16,
    /// `swap_burn`: the least USDC the route must produce. `burn`: the USDC to take from the user.
    pub amount: u64,
    pub fee: u32,
    /// `swap_burn`: Jupiter's instruction data. `burn`: empty.
    pub jupiter: &'a [u8],
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SwapicError> {
        if self.bytes.len() < n {
            return Err(SwapicError::ShortData);
        }
        let (head, tail) = self.bytes.split_at(n);
        self.bytes = tail;
        Ok(head)
    }
    fn array<const N: usize>(&mut self) -> Result<&'a [u8; N], SwapicError> {
        Ok(self.take(N)?.try_into().expect("exactly N bytes"))
    }
    fn u8(&mut self) -> Result<u8, SwapicError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, SwapicError> {
        Ok(u16::from_le_bytes(*self.array::<2>()?))
    }
    fn u32(&mut self) -> Result<u32, SwapicError> {
        Ok(u32::from_le_bytes(*self.array::<4>()?))
    }
    fn u64(&mut self) -> Result<u64, SwapicError> {
        Ok(u64::from_le_bytes(*self.array::<8>()?))
    }
}

impl<'a> BurnArgs<'a> {
    pub fn decode(data: &'a [u8]) -> Result<Self, SwapicError> {
        let mut r = Reader { bytes: data };
        let tag = r.u8()?;
        if tag != TAG_SWAP_BURN && tag != TAG_BURN {
            return Err(SwapicError::BadAction);
        }
        let flags = r.u8()?;
        if flags & FLAG_RESERVED != 0 {
            return Err(SwapicError::ReservedFlags);
        }
        let action = Action::from_bits(flags & FLAG_ACTION_MASK)?;
        let domain = r.u8()?;

        let ref_source = if flags & FLAG_MEMO != 0 {
            if action != Action::Deliver {
                return Err(SwapicError::MemoWithoutDeliver);
            }
            let m = r.u8()? as usize;
            if m == 0 {
                return Err(SwapicError::BadMemoLength);
            }
            RefSource::Memo(r.take(m)?)
        } else {
            RefSource::Direct(r.array::<32>()?)
        };

        let deadline = r.u32()?;
        let recipient = r.array::<20>()?;
        if recipient == &[0u8; 20] {
            return Err(SwapicError::BadRecipient);
        }

        if flags & FLAG_SUBACCOUNT != 0 && action != Action::CkDeposit {
            return Err(SwapicError::ReservedFlags);
        }
        let payload = match action {
            Action::Deliver => Payload::Deliver,
            Action::Swap => {
                let token_out = r.array::<20>()?;
                if token_out == &[0u8; 20] {
                    return Err(SwapicError::BadTokenOutOrMinOut);
                }
                let n = r.u8()? as usize;
                if n == 0 || n > 32 {
                    return Err(SwapicError::BadTokenOutOrMinOut);
                }
                let min_out = r.take(n)?;
                if min_out.iter().all(|b| *b == 0) {
                    return Err(SwapicError::BadTokenOutOrMinOut);
                }
                Payload::Swap { token_out, min_out }
            }
            Action::CkDeposit => {
                let principal = r.array::<32>()?;
                if principal == &[0u8; 32] {
                    return Err(SwapicError::BadPrincipal);
                }
                let subaccount = if flags & FLAG_SUBACCOUNT != 0 { Some(r.array::<32>()?) } else { None };
                Payload::CkDeposit { principal, subaccount }
            }
        };

        let min_gas = r.u32()?;
        if (action == Action::Deliver) != (min_gas == 0) {
            return Err(SwapicError::BadMinGas);
        }
        let max_fee_deci_bps = r.u16()?;
        let amount = r.u64()?;
        let fee = r.u32()?;

        let jupiter = r.bytes;
        if tag == TAG_BURN && !jupiter.is_empty() {
            return Err(SwapicError::ShortData);
        }
        Ok(Self {
            tag,
            fast: flags & FLAG_FAST != 0,
            event_keypair: flags & FLAG_EVENT_KEYPAIR != 0,
            float: flags & FLAG_FLOAT != 0,
            domain,
            ref_source,
            deadline,
            recipient,
            payload,
            min_gas,
            max_fee_deci_bps,
            amount,
            fee,
            jupiter,
        })
    }

    /// The Envelope this burn carries, given its ref (which a memo burn derives by keccak).
    pub fn envelope(&self, ref_bytes: &[u8; 32]) -> Envelope {
        let mut envelope = Envelope {
            ref_bytes: *ref_bytes,
            action: self.payload.action(),
            deadline: self.deadline as u64,
            recipient: *self.recipient,
            token_out: [0; 20],
            min_out: [0; 32],
            principal: [0; 32],
            subaccount: [0; 32],
            min_gas: self.min_gas as u64,
        };
        match self.payload {
            Payload::Deliver => {}
            Payload::Swap { token_out, min_out } => {
                envelope.token_out = *token_out;
                envelope.min_out[32 - min_out.len()..].copy_from_slice(min_out);
            }
            Payload::CkDeposit { principal, subaccount } => {
                envelope.principal = *principal;
                if let Some(subaccount) = subaccount {
                    envelope.subaccount = *subaccount;
                }
            }
        }
        envelope
    }
}

#[cfg(feature = "alloc")]
impl BurnArgs<'_> {
    pub fn encode(&self) -> alloc::vec::Vec<u8> {
        let mut flags = self.payload.action() as u8;
        if self.fast {
            flags |= FLAG_FAST;
        }
        if self.event_keypair {
            flags |= FLAG_EVENT_KEYPAIR;
        }
        if self.float {
            flags |= FLAG_FLOAT;
        }
        if matches!(self.ref_source, RefSource::Memo(_)) {
            flags |= FLAG_MEMO;
        }
        if matches!(self.payload, Payload::CkDeposit { subaccount: Some(_), .. }) {
            flags |= FLAG_SUBACCOUNT;
        }
        let mut out = alloc::vec::Vec::with_capacity(128 + self.jupiter.len());
        out.push(self.tag);
        out.push(flags);
        out.push(self.domain);
        match self.ref_source {
            RefSource::Direct(r) => out.extend_from_slice(r),
            RefSource::Memo(m) => {
                out.push(m.len() as u8);
                out.extend_from_slice(m);
            }
        }
        out.extend_from_slice(&self.deadline.to_le_bytes());
        out.extend_from_slice(self.recipient);
        match self.payload {
            Payload::Deliver => {}
            Payload::Swap { token_out, min_out } => {
                out.extend_from_slice(token_out);
                out.push(min_out.len() as u8);
                out.extend_from_slice(min_out);
            }
            Payload::CkDeposit { principal, subaccount } => {
                out.extend_from_slice(principal);
                if let Some(subaccount) = subaccount {
                    out.extend_from_slice(subaccount);
                }
            }
        }
        out.extend_from_slice(&self.min_gas.to_le_bytes());
        out.extend_from_slice(&self.max_fee_deci_bps.to_le_bytes());
        out.extend_from_slice(&self.amount.to_le_bytes());
        out.extend_from_slice(&self.fee.to_le_bytes());
        out.extend_from_slice(self.jupiter);
        out
    }
}

/// The arguments of `complete` (tag 3) and `release` (tag 4).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ArrivalArgs<'a> {
    pub tag: u8,
    pub message: &'a [u8],
    pub attestation: &'a [u8],
    /// `complete`: Jupiter's instruction data. `release`: empty.
    pub jupiter: &'a [u8],
}

impl<'a> ArrivalArgs<'a> {
    pub fn decode(data: &'a [u8]) -> Result<Self, SwapicError> {
        let mut r = Reader { bytes: data };
        let tag = r.u8()?;
        if tag != TAG_COMPLETE && tag != TAG_RELEASE {
            return Err(SwapicError::BadAction);
        }
        let message_len = r.u16()? as usize;
        let message = r.take(message_len)?;
        let attestation_len = r.u8()? as usize;
        if attestation_len == 0 || attestation_len % 65 != 0 || attestation_len > 65 * 3 {
            return Err(SwapicError::BadMessageLength);
        }
        let attestation = r.take(attestation_len)?;
        let jupiter = r.bytes;
        if tag == TAG_RELEASE && !jupiter.is_empty() {
            return Err(SwapicError::ShortData);
        }
        Ok(Self { tag, message, attestation, jupiter })
    }
}

#[cfg(feature = "alloc")]
impl ArrivalArgs<'_> {
    pub fn encode(&self) -> alloc::vec::Vec<u8> {
        let mut out = alloc::vec::Vec::with_capacity(4 + self.message.len() + self.attestation.len() + self.jupiter.len());
        out.push(self.tag);
        out.extend_from_slice(&(self.message.len() as u16).to_le_bytes());
        out.extend_from_slice(self.message);
        out.push(self.attestation.len() as u8);
        out.extend_from_slice(self.attestation);
        out.extend_from_slice(self.jupiter);
        out
    }
}

/// The arguments of `reclaim` (tag 5).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ReclaimArgs<'a> {
    pub attestation: &'a [u8],
    pub destination_message: &'a [u8],
}

impl<'a> ReclaimArgs<'a> {
    pub fn decode(data: &'a [u8]) -> Result<Self, SwapicError> {
        let mut r = Reader { bytes: data };
        if r.u8()? != TAG_RECLAIM {
            return Err(SwapicError::BadAction);
        }
        let attestation_len = r.u16()? as usize;
        let attestation = r.take(attestation_len)?;
        let message_len = r.u16()? as usize;
        let destination_message = r.take(message_len)?;
        if !r.bytes.is_empty() {
            return Err(SwapicError::ShortData);
        }
        Ok(Self { attestation, destination_message })
    }
}

#[cfg(feature = "alloc")]
impl ReclaimArgs<'_> {
    pub fn encode(&self) -> alloc::vec::Vec<u8> {
        let mut out = alloc::vec::Vec::with_capacity(5 + self.attestation.len() + self.destination_message.len());
        out.push(TAG_RECLAIM);
        out.extend_from_slice(&(self.attestation.len() as u16).to_le_bytes());
        out.extend_from_slice(self.attestation);
        out.extend_from_slice(&(self.destination_message.len() as u16).to_le_bytes());
        out.extend_from_slice(self.destination_message);
        out
    }
}

pub const CONFIG_LEN: usize = 40;
pub const CONFIG_VERSION: u8 = 1;

/// The `config` account (design 4.4), little-endian. No money field: receiver, fee account and
/// keys are compiled into the program.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub paused: bool,
    pub event_keypair_allowed: bool,
    pub bump: u8,
    pub swap_domains: u64,
    pub ck_domains: u64,
    pub min_burn: u64,
    pub min_deadline_s: u32,
    pub max_deadline_s: u32,
}

impl Config {
    pub fn encode(&self) -> [u8; CONFIG_LEN] {
        let mut w = [0u8; CONFIG_LEN];
        w[0] = CONFIG_VERSION;
        w[1] = self.paused as u8;
        w[2] = self.event_keypair_allowed as u8;
        w[3] = self.bump;
        w[4..12].copy_from_slice(&self.swap_domains.to_le_bytes());
        w[12..20].copy_from_slice(&self.ck_domains.to_le_bytes());
        w[20..28].copy_from_slice(&self.min_burn.to_le_bytes());
        w[28..32].copy_from_slice(&self.min_deadline_s.to_le_bytes());
        w[32..36].copy_from_slice(&self.max_deadline_s.to_le_bytes());
        w
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, SwapicError> {
        if bytes.len() != CONFIG_LEN || bytes[0] != CONFIG_VERSION || bytes[1] > 1 || bytes[2] > 1 {
            return Err(SwapicError::WrongConfig);
        }
        let u64_at = |i: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes[i..i + 8]);
            u64::from_le_bytes(b)
        };
        let u32_at = |i: usize| {
            let mut b = [0u8; 4];
            b.copy_from_slice(&bytes[i..i + 4]);
            u32::from_le_bytes(b)
        };
        Ok(Self {
            paused: bytes[1] == 1,
            event_keypair_allowed: bytes[2] == 1,
            bump: bytes[3],
            swap_domains: u64_at(4),
            ck_domains: u64_at(12),
            min_burn: u64_at(20),
            min_deadline_s: u32_at(28),
            max_deadline_s: u32_at(32),
        })
    }

    pub const fn allows_swap_to(&self, domain: u8) -> bool {
        domain < 64 && self.swap_domains & (1 << domain) != 0
    }

    pub const fn allows_ck_to(&self, domain: u8) -> bool {
        domain < 64 && self.ck_domains & (1 << domain) != 0
    }
}

/// `set_config` data: the tag, then the config's bytes from offset 1 (design 4.10).
pub const SET_CONFIG_DATA_LEN: usize = 1 + (CONFIG_LEN - 1);

pub fn encode_set_config(config: &Config) -> [u8; SET_CONFIG_DATA_LEN] {
    let mut out = [0u8; SET_CONFIG_DATA_LEN];
    let bytes = config.encode();
    out[0] = TAG_SET_CONFIG;
    out[1..].copy_from_slice(&bytes[1..]);
    out
}

/// Decodes `set_config` data: `version` here is implicit, the bump is the program's to set.
pub fn decode_set_config(data: &[u8]) -> Result<Config, SwapicError> {
    if data.len() != SET_CONFIG_DATA_LEN || data[0] != TAG_SET_CONFIG {
        return Err(SwapicError::ShortData);
    }
    let mut bytes = [0u8; CONFIG_LEN];
    bytes[0] = CONFIG_VERSION;
    bytes[1..].copy_from_slice(&data[1..]);
    let config = Config::parse(&bytes).map_err(|_| SwapicError::ShortData)?;
    if config.min_deadline_s >= config.max_deadline_s {
        return Err(SwapicError::DeadlineOutOfWindow);
    }
    // CkDeposit exists on Ethereum alone
    if config.ck_domains & !1 != 0 {
        return Err(SwapicError::DomainNotAllowed);
    }
    Ok(config)
}

#[cfg(test)]
mod tests;
