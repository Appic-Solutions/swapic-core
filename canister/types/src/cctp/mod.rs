//! CCTP v2's burn message, as the source chain's `MessageTransmitterV2` emits it and as
//! Circle attests it: a 148-byte header and a 228-byte burn body, laid out field by field
//! at fixed offsets (a uint32 in four bytes, an address as a 32-byte word, an amount as a
//! 256-bit integer, all big-endian), then the hook data. The layout is Circle's
//! `MessageV2` and `BurnMessageV2` libraries', pinned offset by offset in the tests.
//!
//! This canister never builds a message: it decodes the one the watcher hands in for a
//! swap and binds every field the burn determined to the swap, so a message of another
//! burn cannot be minted under this swap's name.

#[cfg(test)]
mod tests;

use crate::numeric::{BlockNumber, TokenAmount};
use thiserror::Error;

/// The header's length: version, source and destination domains, nonce, sender,
/// recipient, destination caller, and the two finality thresholds.
pub const HEADER_BYTES: usize = 148;

/// The burn body's length: version, burn token, mint recipient, amount, message sender,
/// max fee, fee executed and expiration block. The hook data follows it.
pub const BURN_BODY_BYTES: usize = 228;

/// The message version CCTP v2 writes.
pub const MESSAGE_VERSION: u32 = 1;

/// The burn body version `TokenMessengerV2` writes.
pub const BURN_BODY_VERSION: u32 = 1;

/// Why bytes are not a burn message.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MessageError {
    #[error("the message is {len} bytes, and a burn message is at least {wanted}")]
    TooShort { len: usize, wanted: usize },
    #[error("the message version is {version}, and this canister reads version {MESSAGE_VERSION}")]
    UnknownVersion { version: u32 },
    #[error(
        "the burn body version is {version}, and this canister reads version {BURN_BODY_VERSION}"
    )]
    UnknownBodyVersion { version: u32 },
    #[error("the expiration block does not fit a block number")]
    ExpirationBlockTooLarge,
}

/// The burn body: what `depositForBurn` said, plus the fee and the expiration the
/// attestation service filled in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BurnBody {
    pub version: u32,
    pub burn_token: [u8; 32],
    pub mint_recipient: [u8; 32],
    pub amount: TokenAmount,
    /// Who called `depositForBurn` on the source chain: the vault, for a burn of ours.
    pub message_sender: [u8; 32],
    pub max_fee: TokenAmount,
    /// The fee Circle takes from `amount`, filled in by the attestation service; zero as
    /// emitted.
    pub fee_executed: TokenAmount,
    /// The last block the message may be received in, filled in by the attestation
    /// service; zero as emitted, and zero means never.
    pub expiration_block: BlockNumber,
    pub hook_data: Vec<u8>,
}

/// A whole attested burn message: the header and its burn body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BurnMessage {
    pub version: u32,
    pub source_domain: u32,
    pub destination_domain: u32,
    /// Assigned by the attestation service; zero as emitted.
    pub nonce: [u8; 32],
    /// The source chain's token messenger.
    pub sender: [u8; 32],
    /// The destination chain's token messenger.
    pub recipient: [u8; 32],
    /// The only address that may deliver the message, or the zero word for anyone.
    pub destination_caller: [u8; 32],
    pub min_finality_threshold: u32,
    /// The finality the message was attested at, filled in by the attestation service.
    pub finality_threshold_executed: u32,
    pub body: BurnBody,
}

/// Reads the fixed-width fields of a message in order.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let out: [u8; N] = self.bytes[self.at..self.at + N]
            .try_into()
            .expect("BUG: the length was checked before the read");
        self.at += N;
        out
    }

    fn u32(&mut self) -> u32 {
        u32::from_be_bytes(self.take::<4>())
    }

    fn word(&mut self) -> [u8; 32] {
        self.take::<32>()
    }

    fn amount(&mut self) -> TokenAmount {
        TokenAmount::from_be_bytes(self.take::<32>())
    }
}

impl BurnMessage {
    /// The message `bytes` carry, if they are a version 1 message with a version 1 burn
    /// body.
    pub fn parse(bytes: &[u8]) -> Result<Self, MessageError> {
        for wanted in [HEADER_BYTES, HEADER_BYTES + BURN_BODY_BYTES] {
            if bytes.len() < wanted {
                return Err(MessageError::TooShort {
                    len: bytes.len(),
                    wanted,
                });
            }
        }
        let mut r = Reader { bytes, at: 0 };
        let version = r.u32();
        if version != MESSAGE_VERSION {
            return Err(MessageError::UnknownVersion { version });
        }
        let source_domain = r.u32();
        let destination_domain = r.u32();
        let nonce = r.word();
        let sender = r.word();
        let recipient = r.word();
        let destination_caller = r.word();
        let min_finality_threshold = r.u32();
        let finality_threshold_executed = r.u32();
        let body_version = r.u32();
        if body_version != BURN_BODY_VERSION {
            return Err(MessageError::UnknownBodyVersion {
                version: body_version,
            });
        }
        let burn_token = r.word();
        let mint_recipient = r.word();
        let amount = r.amount();
        let message_sender = r.word();
        let max_fee = r.amount();
        let fee_executed = r.amount();
        let expiration_block = r
            .amount()
            .try_into_u128()
            .and_then(|block| u64::try_from(block).ok())
            .map(BlockNumber::new)
            .ok_or(MessageError::ExpirationBlockTooLarge)?;
        let hook_data = bytes[r.at..].to_vec();
        Ok(Self {
            version,
            source_domain,
            destination_domain,
            nonce,
            sender,
            recipient,
            destination_caller,
            min_finality_threshold,
            finality_threshold_executed,
            body: BurnBody {
                version: body_version,
                burn_token,
                mint_recipient,
                amount,
                message_sender,
                max_fee,
                fee_executed,
                expiration_block,
                hook_data,
            },
        })
    }

    /// The bytes of the message, as the transmitter and the attestation service lay them
    /// out. What a fixture hands in as a real-shaped message.
    pub fn encode(&self) -> Vec<u8> {
        let BurnMessage {
            version,
            source_domain,
            destination_domain,
            nonce,
            sender,
            recipient,
            destination_caller,
            min_finality_threshold,
            finality_threshold_executed,
            body:
                BurnBody {
                    version: body_version,
                    burn_token,
                    mint_recipient,
                    amount,
                    message_sender,
                    max_fee,
                    fee_executed,
                    expiration_block,
                    hook_data,
                },
        } = self;
        let mut out = Vec::with_capacity(HEADER_BYTES + BURN_BODY_BYTES + hook_data.len());
        out.extend_from_slice(&version.to_be_bytes());
        out.extend_from_slice(&source_domain.to_be_bytes());
        out.extend_from_slice(&destination_domain.to_be_bytes());
        out.extend_from_slice(nonce);
        out.extend_from_slice(sender);
        out.extend_from_slice(recipient);
        out.extend_from_slice(destination_caller);
        out.extend_from_slice(&min_finality_threshold.to_be_bytes());
        out.extend_from_slice(&finality_threshold_executed.to_be_bytes());
        out.extend_from_slice(&body_version.to_be_bytes());
        out.extend_from_slice(burn_token);
        out.extend_from_slice(mint_recipient);
        out.extend_from_slice(&amount.to_be_bytes());
        out.extend_from_slice(message_sender);
        out.extend_from_slice(&max_fee.to_be_bytes());
        out.extend_from_slice(&fee_executed.to_be_bytes());
        out.extend_from_slice(&TokenAmount::from(expiration_block.get()).to_be_bytes());
        out.extend_from_slice(hook_data);
        out
    }
}
