//! Eco: the intent published and funded on the source chain through the vault, a solver's
//! fill arriving in the destination vault on its own, and the permissionless refund of an
//! intent nobody filled.
//!
//! THE RAIL IS OFF (`eco_enabled`, default off), and a quote naming it is refused at the
//! claim. Three things must be true before a deploy turns it on:
//!
//! 1. The route must end in a deposit into OUR destination vault under the quote hash.
//!    The fill is detected by reading that vault's `Deposited` log for the quote, and the
//!    route is what a filler runs on the destination: a route that delivers anywhere else
//!    is filled, the reward is claimed, and this canister sees nothing and refunds after
//!    the deadline. The research's quoted route (`eco_intent_1_quote.json`) calls
//!    `approve` and `depositForBurn`, which is not that, so the quoter must build the
//!    route, and this canister must check it rather than publish what the watcher pushed.
//! 2. The reward must not be claimable without a delivery. The prover decides who may say
//!    an intent was filled, and today it is whatever the watcher pushed: it must be
//!    pinned to a configured per-chain value, and the destination to the quote's own
//!    chain mapping, before the vault locks `amount_in` against an intent.
//! 3. The allowlist law gets an explicit ruling on the Portal. The publish is a vault
//!    `execute` whose target is the Portal, so the Portal has to be on the vault's router
//!    allowlist, and the law (handoff section 8) admits only stateless routers, never
//!    anything holding vault-claimable balances. Either the law gains an exception, with
//!    the vault's public `depositAndExecute` path reviewed against it, or the publish
//!    leaves the vault another way.

#[cfg(test)]
mod tests;

use super::{
    ensure_rail_tokens, through_the_vault, usdc_on, CallRail, Position, RailError, RailStep,
    RailTx, ReclaimStep, WaitingFor,
};
use crate::deposits::vault_of;
use types::abi::{eco_publish_and_fund, eco_refund, eco_route_hash, EcoReward};
use types::events::TxPurpose;
use types::{EcoIntent, GasAmount, Leg as SwapLeg, Rail, Wei};

/// The gas a publish through the vault needs: the vault's approve, the Portal's publish
/// and fund, and the approve back to zero, with room over the measured cost.
pub const PUBLISH_GAS_LIMIT: GasAmount = GasAmount::new(450_000);

/// The gas a Portal refund needs: the vault's refund transfer and the intent's bookkeeping.
pub const RECLAIM_GAS_LIMIT: GasAmount = GasAmount::new(250_000);

/// The Eco rail.
pub struct Eco;

/// The rail moves nothing while the deploy has it off.
fn ensure_enabled(at: &Position) -> Result<(), RailError> {
    if !at.config.eco_enabled.is_on() {
        return Err(RailError::RailDisabled { rail: Rail::Eco });
    }
    Ok(())
}

impl Eco {
    /// The reward the vault locks for the filler: the swap's whole amount of the source
    /// USDC, refundable to the vault itself after the deadline. The creator is the vault
    /// and never anything the watcher pushed, because the creator is who a refund pays.
    pub fn reward(&self, at: &Position, intent: &EcoIntent) -> Result<EcoReward, RailError> {
        let source_usdc = usdc_on(at.config, at.quote.src_chain)?;
        Ok(EcoReward {
            deadline: intent.deadline(),
            creator: vault_of(at.config, at.quote.src_chain)?,
            prover: intent.prover(),
            native_amount: Wei::ZERO,
            tokens: vec![(source_usdc, at.swap.amount_in)],
        })
    }

    /// The publish: the vault approves the Portal for the amount and calls
    /// `publishAndFund` for the destination ECO named, and its USDC balance may fall by
    /// exactly the amount.
    pub fn publish(&self, at: &Position, intent: &EcoIntent) -> Result<RailTx, RailError> {
        let config = at.config;
        let amount = at.swap.amount_in;
        let portal = config.eco_portal.ok_or(RailError::NoEcoPortal)?;
        let source_usdc = usdc_on(config, at.quote.src_chain)?;
        let reward = self.reward(at, intent)?;
        through_the_vault(
            at,
            TxPurpose::Burn(at.quote_hash),
            portal,
            eco_publish_and_fund(intent.destination(), intent.route(), &reward),
            source_usdc,
            amount,
            PUBLISH_GAS_LIMIT,
        )
    }

    /// The reclaim: the Portal's permissionless `refund`, from this canister's own address,
    /// paying the reward back to the vault that created it. Named for the leg it is and not
    /// for the Portal's function, because the vault has a `refund` of its own and that one
    /// is what pays the user back.
    pub fn reclaim_tx(&self, at: &Position, intent: &EcoIntent) -> Result<RailTx, RailError> {
        let portal = at.config.eco_portal.ok_or(RailError::NoEcoPortal)?;
        let reward = self.reward(at, intent)?;
        Ok(RailTx {
            purpose: TxPurpose::Reclaim(at.quote_hash),
            chain_id: at.quote.src_chain,
            to: portal,
            value: Wei::ZERO,
            data: eco_refund(
                intent.destination(),
                eco_route_hash(intent.route()),
                &reward,
            ),
            gas_limit: RECLAIM_GAS_LIMIT,
        })
    }
}

impl CallRail for Eco {
    fn rail(&self) -> Rail {
        Rail::Eco
    }

    fn step(&self, at: &Position) -> Result<RailStep, RailError> {
        // the rail is off until its route is designed: a swap claimed while it was on,
        // or through a door that did not check, moves nothing
        ensure_enabled(at)?;
        // the publish locks the source USDC as the reward and the fill is read in the
        // destination USDC, so both of the quote's tokens have to be those
        ensure_rail_tokens(at)?;
        let Some(intent) = at.intent else {
            return Ok(RailStep::Wait(WaitingFor::Intent));
        };
        Ok(match at.swap.last_leg {
            None => RailStep::Send(self.publish(at, intent)?),
            // the filler delivers into the destination vault on its own; the read that
            // finds it is the engine's, and past the deadline a read that finds nothing
            // ends the swap in a refund
            Some(SwapLeg::Burn) => RailStep::CheckArrival {
                chain_id: at.quote.dst_chain,
                expired: intent.is_past_deadline(at.now),
            },
            Some(SwapLeg::Mint | SwapLeg::Payout | SwapLeg::Refund | SwapLeg::Reclaim) => {
                RailStep::Stuck(
                    "a mint, payout, refund or reclaim at is not one this rail steps from",
                )
            }
        })
    }

    fn reclaim(&self, at: &Position) -> Result<ReclaimStep, RailError> {
        // the engine asks only once the publish has confirmed, so the reward is locked in
        // the Portal: with the rail off nothing is sent for it, and the swap stops for a
        // human instead of being refused on every tick with nothing to show for it
        if !at.config.eco_enabled.is_on() {
            return Ok(ReclaimStep::Stuck(
                "the Eco rail is off with this swap's reward locked in the Portal",
            ));
        }
        let Some(intent) = at.intent else {
            return Ok(ReclaimStep::Stuck(
                "the intent this swap published is no longer in the inbox",
            ));
        };
        if !intent.is_past_deadline(at.now) {
            return Ok(ReclaimStep::Wait(WaitingFor::Deadline));
        }
        Ok(ReclaimStep::Send(self.reclaim_tx(at, intent)?))
    }
}
