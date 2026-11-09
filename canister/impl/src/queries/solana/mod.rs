//! What ops, the watcher and the kit read of the Solana line.

use crate::solana::now_s;
use crate::state::solana::hour_of;
use crate::storage::events::read_state;
use crate::storage::solana_keys;
use ic_cdk::query;
pub use settlement_api::types::solana::{SolanaAddresses, SolanaBudget, SolanaUpgradeView};
use swapic_solana_wire::base58;
use types::solana::SolanaProposal;

/// The canister's two Solana addresses, or nothing before they are derived. World-readable.
#[query]
pub fn solana_addresses() -> Option<SolanaAddresses> {
    solana_keys::get().map(|(payer, admin)| SolanaAddresses {
        payer: base58::encode(&payer),
        admin: base58::encode(&admin),
    })
}

fn view(proposal: SolanaProposal) -> SolanaUpgradeView {
    SolanaUpgradeView {
        id: proposal.id,
        buffer: base58::encode(&proposal.buffer),
        build_hash: proposal.build_hash.to_vec(),
        source_commit: proposal.source_commit,
        proposed_at_s: proposal.proposed_at_s,
        not_before_s: proposal.not_before_s,
        status: format!("{:?}", proposal.status),
    }
}

/// The program upgrade waiting out its delay. World-readable: anyone may rebuild it and compare.
#[query]
pub fn solana_live_upgrade() -> Option<SolanaUpgradeView> {
    read_state(|state| state.solana_live_proposal()).map(view)
}

/// What the co-sign limits have counted this hour.
#[query]
pub fn solana_budget() -> SolanaBudget {
    let (cosigns_this_hour, failures_this_hour) =
        read_state(|state| state.solana_global(hour_of(now_s())));
    SolanaBudget {
        cosigns_this_hour,
        failures_this_hour,
    }
}
