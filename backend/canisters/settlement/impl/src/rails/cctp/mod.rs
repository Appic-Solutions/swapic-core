//! CCTP v2: the burn on the source chain through the vault, Circle's attestation, and the
//! mint on the destination chain from this canister's own address.

#[cfg(test)]
mod tests;

use super::{quote_address, usdc_on, CallRail, Leg, RailError, RailStep, RailTx, WaitingFor};
use crate::deposits::vault_of;
use types::abi::{
    cctp_deposit_for_burn, cctp_receive_message, vault_execute, Burn, VaultCall, VaultDelta,
};
use types::events::TxPurpose;
use types::{BasisPoints, GasAmount, Leg as SwapLeg, Rail, TokenAmount, Wei};

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

    /// The mint: `receiveMessage` on the destination's message transmitter, from this
    /// canister's own address, carrying what the watcher handed in.
    pub fn mint(&self, leg: &Leg, attestation: &types::Attestation) -> Result<RailTx, RailError> {
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
        // the payout's token has to be the USDC the mint delivers; refused before the burn
        // rather than after the funds have crossed
        quote_address(leg.quote.dst_token.as_str(), "dst_token")?;
        Ok(match leg.swap.last_leg {
            None => RailStep::Send(self.burn(leg)?),
            Some(SwapLeg::Burn) => match leg.attestation {
                Some(attestation) => RailStep::Send(self.mint(leg, attestation)?),
                None => RailStep::Wait(WaitingFor::Attestation),
            },
            Some(SwapLeg::Mint) => RailStep::Arrived {
                chain_id: leg.quote.dst_chain,
                amount: self.least_minted(leg.swap.amount_in)?,
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
