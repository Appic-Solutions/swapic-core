//! CCTP v2: the burn on the source chain through the vault, Circle's attestation, and the
//! mint on the destination chain from this canister's own address.
//!
//! The attestation the watcher hands in is bound to the swap before it is minted: every
//! field of the message that the burn determined (the lane, the token, the amount, the
//! recipient, the caller, the threshold, the fee ceiling) must be the swap's own, so a
//! message of another burn cannot be minted under this swap's name and paid out of the
//! destination vault's pooled balance. What the mint delivered is then read off the mint's
//! own receipt and never computed from the burn.

#[cfg(test)]
mod tests;

use super::{ensure_rail_tokens, usdc_on, CallRail, Leg, RailError, RailStep, RailTx, WaitingFor};
use crate::deposits::vault_of;
use thiserror::Error;
use types::abi::{
    cctp_deposit_for_burn, cctp_receive_message, vault_execute, Burn, VaultCall, VaultDelta,
};
use types::cctp::{BurnMessage, MESSAGE_VERSION};
use types::events::TxPurpose;
use types::{BasisPoints, GasAmount, Leg as SwapLeg, Rail, TokenAmount, Wei};

/// A field of a burn message the burn determined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageField {
    SourceDomain,
    DestinationDomain,
    Sender,
    Recipient,
    DestinationCaller,
    BurnToken,
    MintRecipient,
    Amount,
    MessageSender,
    MaxFee,
}

impl std::fmt::Display for MessageField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SourceDomain => "source domain",
            Self::DestinationDomain => "destination domain",
            Self::Sender => "sender",
            Self::Recipient => "recipient",
            Self::DestinationCaller => "destination caller",
            Self::BurnToken => "burn token",
            Self::MintRecipient => "mint recipient",
            Self::Amount => "amount",
            Self::MessageSender => "message sender",
            Self::MaxFee => "max fee",
        })
    }
}

/// Why a burn message is not this swap's: the field the burn determined that reads
/// otherwise, with what the swap's burn wrote and what the message carries.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MessageMismatch {
    #[error(
        "the message is version {found}, and this canister's burns are version {MESSAGE_VERSION}"
    )]
    Version { found: u32 },
    #[error("the message's {field} is {found}, and the swap's is {expected}")]
    Domain {
        field: MessageField,
        expected: u32,
        found: u32,
    },
    #[error(
        "the message's {field} is 0x{}, and the swap's is 0x{}",
        hex::encode(found),
        hex::encode(expected)
    )]
    Word {
        field: MessageField,
        expected: [u8; 32],
        found: [u8; 32],
    },
    #[error("the message asks finality {found}, and the swap's rail asks {expected}")]
    Threshold { expected: u32, found: u32 },
    #[error("the message's {field} is {found}, and the swap's is {expected}")]
    Amount {
        field: MessageField,
        expected: TokenAmount,
        found: TokenAmount,
    },
    #[error("the fee executed is {fee}, above the {max_fee} the burn allowed")]
    FeeAboveMaxFee {
        fee: TokenAmount,
        max_fee: TokenAmount,
    },
    #[error("the message carries {len} bytes of hook data, and this canister's burns carry none")]
    HookData { len: usize },
}

/// CCTP v2's fast finality threshold: attested at confirmation, minutes on every chain.
pub const FAST_FINALITY_THRESHOLD: u32 = 1_000;

/// CCTP v2's standard finality threshold: attested at finality, which on an L2 is the
/// L1's, and free.
pub const STANDARD_FINALITY_THRESHOLD: u32 = 2_000;

/// The most of a fast burn Circle may take as its fee: two basis points, above the one to
/// 1.3 measured on every lane, and the burn reverts rather than pays more.
pub const FAST_FEE_CEILING: BasisPoints = BasisPoints::new(2);

/// The gas a burn through the vault needs: the vault's approve, the token messenger's
/// burn and message, and the approve back to zero, with room over the measured cost.
pub const BURN_GAS_LIMIT: GasAmount = GasAmount::new(350_000);

/// The gas a `receiveMessage` needs: the attestation's signature checks and the mint.
pub const MINT_GAS_LIMIT: GasAmount = GasAmount::new(300_000);

/// The CCTP v2 rail, fast or standard.
pub struct Cctp {
    pub fast: bool,
}

impl Cctp {
    /// The CCTP rail `rail` names, if it names one: what the attestation door asks before
    /// it binds a message, since only a CCTP swap has a burn to attest.
    pub fn of(rail: Rail) -> Option<Self> {
        match rail {
            Rail::CctpV2Fast => Some(Self { fast: true }),
            Rail::CctpV2Standard => Some(Self { fast: false }),
            Rail::Eco => None,
        }
    }

    /// The finality Circle attests the burn at.
    pub fn finality_threshold(&self) -> u32 {
        if self.fast {
            FAST_FINALITY_THRESHOLD
        } else {
            STANDARD_FINALITY_THRESHOLD
        }
    }

    /// The most Circle may take from `amount`: the fast ceiling rounded up, so no burn is
    /// refused for a fee that rounds to nothing, and nothing at all on the standard path.
    pub fn max_fee(&self, amount: TokenAmount) -> Result<TokenAmount, RailError> {
        if !self.fast {
            return Ok(TokenAmount::ZERO);
        }
        amount
            .checked_mul(FAST_FEE_CEILING.get())
            .and_then(|scaled| scaled.checked_div_ceil(BasisPoints::MAX.get()))
            .ok_or(RailError::FeeOverflow { amount })
    }

    /// The least the destination vault receives: the burn less the most Circle may take.
    /// What `PaidInStable` records and the payout is paid out of; the fee Circle really
    /// takes is at most this, and the difference stays in the destination vault as the
    /// platform's.
    pub fn least_minted(&self, amount: TokenAmount) -> Result<TokenAmount, RailError> {
        amount
            .checked_sub(self.max_fee(amount)?)
            .ok_or(RailError::FeeOverflow { amount })
    }

    /// The burn: the vault approves the token messenger for the amount and calls
    /// `depositForBurn`, minting to the destination vault, deliverable only by this
    /// canister's own address, and its USDC balance may fall by exactly the amount.
    pub fn burn(&self, leg: &Leg) -> Result<RailTx, RailError> {
        let quote = leg.quote;
        let config = leg.config;
        let amount = leg.swap.amount_in;
        let source_usdc = usdc_on(config, quote.src_chain)?;
        let destination_domain =
            config
                .cctp_domains
                .get(quote.dst_chain)
                .ok_or(RailError::NoDomain {
                    chain_id: quote.dst_chain,
                })?;
        let destination_vault = vault_of(config, quote.dst_chain)?;
        let messenger = config.token_messenger.ok_or(RailError::NoTokenMessenger)?;
        let burn = Burn {
            amount,
            destination_domain: destination_domain.get(),
            mint_recipient: destination_vault.to_word(),
            burn_token: source_usdc,
            // only this canister delivers the mint, so nobody can spend the message first
            // and leave the mint transaction to revert
            destination_caller: leg.mine.to_word(),
            max_fee: self.max_fee(amount)?,
            min_finality_threshold: self.finality_threshold(),
        };
        let calls = [VaultCall {
            target: messenger,
            value: Wei::ZERO,
            data: cctp_deposit_for_burn(&burn),
            approve_token: source_usdc,
            approve_amount: amount,
        }];
        let min_change = i128::try_from(
            amount
                .try_into_u128()
                .ok_or(RailError::FeeOverflow { amount })?,
        )
        .map_err(|_| RailError::FeeOverflow { amount })?;
        let deltas = [VaultDelta {
            token: source_usdc,
            min_change: -min_change,
        }];
        Ok(RailTx {
            purpose: TxPurpose::Burn(leg.quote_hash),
            chain_id: quote.src_chain,
            to: vault_of(config, quote.src_chain)?,
            value: Wei::ZERO,
            data: vault_execute(leg.quote_hash, &calls, &deltas),
            gas_limit: BURN_GAS_LIMIT,
        })
    }

    /// Whether `message` is the attested message of this swap's own burn: every field the
    /// burn determined must read as the burn wrote it (the lane's domains, the token
    /// messenger on both ends, this canister as the only caller, the rail's threshold, the
    /// source USDC, the destination vault, the swap's amount, the source vault as the
    /// sender, the rail's fee ceiling, no hook data), and the fee the attestation service
    /// executed must be inside that ceiling. The nonce, the finality executed and the
    /// expiration are the service's to fill in. Refused by the field, so a message of
    /// another burn, ours or anyone's, is never minted under this swap's name.
    pub fn ensure_message_binds(&self, leg: &Leg, message: &BurnMessage) -> Result<(), RailError> {
        let quote = leg.quote;
        let config = leg.config;
        let domain = |chain_id| {
            config
                .cctp_domains
                .get(chain_id)
                .map(|domain| domain.get())
                .ok_or(RailError::NoDomain { chain_id })
        };
        let messenger = config
            .token_messenger
            .ok_or(RailError::NoTokenMessenger)?
            .to_word();
        let amount = leg.swap.amount_in;
        let max_fee = self.max_fee(amount)?;
        if message.version != MESSAGE_VERSION {
            return Err(MessageMismatch::Version {
                found: message.version,
            }
            .into());
        }
        for (field, expected, found) in [
            (
                MessageField::SourceDomain,
                domain(quote.src_chain)?,
                message.source_domain,
            ),
            (
                MessageField::DestinationDomain,
                domain(quote.dst_chain)?,
                message.destination_domain,
            ),
        ] {
            if expected != found {
                return Err(MessageMismatch::Domain {
                    field,
                    expected,
                    found,
                }
                .into());
            }
        }
        for (field, expected, found) in [
            (MessageField::Sender, messenger, message.sender),
            (MessageField::Recipient, messenger, message.recipient),
            (
                MessageField::DestinationCaller,
                leg.mine.to_word(),
                message.destination_caller,
            ),
            (
                MessageField::BurnToken,
                usdc_on(config, quote.src_chain)?.to_word(),
                message.body.burn_token,
            ),
            (
                MessageField::MintRecipient,
                vault_of(config, quote.dst_chain)?.to_word(),
                message.body.mint_recipient,
            ),
            (
                MessageField::MessageSender,
                vault_of(config, quote.src_chain)?.to_word(),
                message.body.message_sender,
            ),
        ] {
            if expected != found {
                return Err(MessageMismatch::Word {
                    field,
                    expected,
                    found,
                }
                .into());
            }
        }
        if message.min_finality_threshold != self.finality_threshold() {
            return Err(MessageMismatch::Threshold {
                expected: self.finality_threshold(),
                found: message.min_finality_threshold,
            }
            .into());
        }
        for (field, expected, found) in [
            (MessageField::Amount, amount, message.body.amount),
            (MessageField::MaxFee, max_fee, message.body.max_fee),
        ] {
            if expected != found {
                return Err(MessageMismatch::Amount {
                    field,
                    expected,
                    found,
                }
                .into());
            }
        }
        if message.body.fee_executed > max_fee {
            return Err(MessageMismatch::FeeAboveMaxFee {
                fee: message.body.fee_executed,
                max_fee,
            }
            .into());
        }
        if !message.body.hook_data.is_empty() {
            return Err(MessageMismatch::HookData {
                len: message.body.hook_data.len(),
            }
            .into());
        }
        Ok(())
    }

    /// The mint: `receiveMessage` on the destination's message transmitter, from this
    /// canister's own address, carrying what the watcher handed in, once the message is
    /// bound to this swap's own burn. The inbox door binds it at the push; binding it here
    /// again is rule A2's counterpart for a line no fold can check, so nothing that
    /// reaches the inbox another way is minted either.
    pub fn mint(&self, leg: &Leg, attestation: &types::Attestation) -> Result<RailTx, RailError> {
        let message = BurnMessage::parse(&attestation.message)?;
        self.ensure_message_binds(leg, &message)?;
        let transmitter = leg
            .config
            .message_transmitter
            .ok_or(RailError::NoMessageTransmitter)?;
        Ok(RailTx {
            purpose: TxPurpose::Mint(leg.quote_hash),
            chain_id: leg.quote.dst_chain,
            to: transmitter,
            value: Wei::ZERO,
            data: cctp_receive_message(&attestation.message, &attestation.attestation),
            gas_limit: MINT_GAS_LIMIT,
        })
    }
}

impl CallRail for Cctp {
    fn rail(&self) -> Rail {
        if self.fast {
            Rail::CctpV2Fast
        } else {
            Rail::CctpV2Standard
        }
    }

    fn step(&self, leg: &Leg) -> Result<RailStep, RailError> {
        // the burn spends the source USDC and the payout pays the destination USDC, so
        // both of the quote's tokens have to be those; refused before the burn rather
        // than after the funds have crossed
        ensure_rail_tokens(leg)?;
        Ok(match leg.swap.last_leg {
            None => RailStep::Send(self.burn(leg)?),
            Some(SwapLeg::Burn) => match leg.attestation {
                Some(attestation) => RailStep::Send(self.mint(leg, attestation)?),
                None => RailStep::Wait(WaitingFor::Attestation),
            },
            // the mint confirmed: what it delivered is on the chain, in the mint's own
            // receipt, and that is what `PaidInStable` records
            Some(SwapLeg::Mint) => RailStep::ReadMint {
                chain_id: leg.quote.dst_chain,
                tx_hash: leg.swap.last_tx_hash.ok_or(RailError::NoMintHash)?,
            },
            Some(SwapLeg::Payout | SwapLeg::Refund | SwapLeg::Reclaim) => {
                RailStep::Stuck("a payout, refund or reclaim leg is not one this rail steps from")
            }
        })
    }

    fn reclaim(&self, _leg: &Leg) -> Result<RailStep, RailError> {
        // a burn is final: the USDC is gone from the source chain, and only the mint on
        // the destination brings it back into a vault
        Ok(RailStep::Stuck(
            "the burn confirmed, so the funds cannot come back to the source vault",
        ))
    }
}
