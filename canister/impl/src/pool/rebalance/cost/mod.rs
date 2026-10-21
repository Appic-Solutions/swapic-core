//! What the canister's own route for a move costs: Circle's fee at the most (nothing at
//! standard finality but the messenger's least fee; fast, at least the proposals' fee cap of
//! the amount, which lets CCTP fast through), and the network fees the config names for the
//! move's own transaction and its finish (`min_network_fee`'s `pool_move` on the source chain,
//! `pool_move_finish` on the destination). What a proposal must undercut.

#[cfg(test)]
mod tests;

use types::config::{Config, GasAction};
use types::pool::{MoveDirection, RebalanceSettings};
use types::{ChainId, TokenAmount};

/// The chains a move runs between: its source pocket's chain and the other one. Ethereum is
/// where the ck minter lands and takes USDC.
pub fn chains(direction: MoveDirection, pool_chain: ChainId) -> (ChainId, ChainId) {
    match direction {
        MoveDirection::BaseToIcp => (pool_chain, ChainId::ETHEREUM),
        MoveDirection::IcpToBase => (ChainId::ETHEREUM, pool_chain),
    }
}

/// Whether the own route burns fast: when a payout waits, or always once standard finality
/// is turned off.
pub fn fast(urgent: bool, settings: &RebalanceSettings) -> bool {
    urgent || !settings.standard_when_idle
}

/// The most the own route offers Circle for a burn of `amount` from `src`.
pub fn max_fee(
    config: &Config,
    src: ChainId,
    amount: TokenAmount,
    fast: bool,
    settings: &RebalanceSettings,
) -> TokenAmount {
    let least = config
        .cctp_min_fees
        .get(src)
        .and_then(|min_fee| min_fee.amount_for(amount))
        .unwrap_or(TokenAmount::ZERO);
    if !fast {
        return least;
    }
    let cap = amount
        .checked_mul(settings.max_proposal_fee_bps)
        .and_then(|scaled| scaled.checked_div_ceil(10_000_u32))
        .unwrap_or(TokenAmount::ZERO);
    least.max(cap)
}

/// A chain's least network fee for `action`, none where the row is not set.
pub fn network_fee(config: &Config, chain: ChainId, action: GasAction) -> TokenAmount {
    config
        .evm_chains
        .get(chain)
        .and_then(|evm| evm.min_network_fee.get(action))
        .unwrap_or(TokenAmount::ZERO)
}

/// The own route's estimated cost for a move of `amount`, micro USDC.
pub fn own_cost(
    config: &Config,
    pool_chain: ChainId,
    direction: MoveDirection,
    amount: TokenAmount,
    urgent: bool,
    settings: &RebalanceSettings,
) -> TokenAmount {
    let (src, dst) = chains(direction, pool_chain);
    let circle = max_fee(config, src, amount, fast(urgent, settings), settings);
    [
        network_fee(config, src, GasAction::PoolMove),
        network_fee(config, dst, GasAction::PoolMoveFinish),
    ]
    .into_iter()
    .try_fold(circle, TokenAmount::checked_add)
    .unwrap_or(TokenAmount::MAX)
}
