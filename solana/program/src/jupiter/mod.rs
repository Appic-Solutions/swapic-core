//! The Jupiter pass-through (rule 5 of design 4.5): the only Jupiter instructions that reach the
//! router are its two route forms, no route may name an account of Swapic's own, and only the
//! intended authority signs, so the fee payer's signature never reaches an AMM.

use crate::accounts::{key_of, JUPITER_ADDRESS};
use crate::consts;
use crate::fail;
use pinocchio::cpi::{invoke_signed_with_slice, Signer};
use pinocchio::instruction::{InstructionAccount, InstructionView};
use pinocchio::{AccountView, ProgramResult};
use swapic_solana_wire::consts::{MT, TMM};
use swapic_solana_wire::errors::SwapicError;
use swapic_solana_wire::jupiter::is_allowed_route;
use swapic_solana_wire::Pubkey;

/// Which way the route runs, and so which of Swapic's accounts it may name.
#[derive(Copy, Clone)]
pub enum Way<'a> {
    /// On the way out the route may name only `vault[shard]`, its output, and never an `auth`.
    Out { shard: usize, event: &'a Pubkey },
    /// On an arrival `auth[shard]` and `vault[shard]` are the route's authority and source.
    Arrival { shard: usize },
}

const COMMON: [&Pubkey; 7] = [
    &consts::CONFIG,
    &consts::RENT,
    &consts::CANISTER,
    &consts::ADMIN,
    &consts::PROGRAM_ID,
    &MT,
    &TMM,
];

#[inline(always)]
fn first_word(key: &Pubkey) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&key[..8]);
    u64::from_ne_bytes(word)
}

/// Refuses a route naming an account it must not. Comparing the first word first keeps this to a
/// few CU per account.
pub fn check_route(route: &[AccountView], way: Way<'_>) -> ProgramResult {
    let (shard, event, auth_allowed) = match way {
        Way::Out { shard, event } => (shard, Some(event), false),
        Way::Arrival { shard } => (shard, None, true),
    };
    for view in route {
        let key = key_of(view);
        let word = first_word(key);
        let hit = |other: &Pubkey| word == first_word(other) && key == other;
        let forbidden = COMMON.iter().any(|other| hit(other))
            || event.is_some_and(|e| hit(e))
            || (0..4).any(|j| {
                (hit(&consts::AUTH[j]) && !(auth_allowed && j == shard))
                    || (hit(&consts::VAULT[j]) && j != shard)
            });
        if forbidden {
            return Err(fail(SwapicError::ForbiddenRouteAccount));
        }
    }
    Ok(())
}

/// Calls the router with the route's accounts. The signer flag is set for the authority alone: the
/// outer flags are never copied.
pub fn call(
    route: &[AccountView],
    data: &[u8],
    authority: &Pubkey,
    signers: &[Signer<'_, '_>],
) -> ProgramResult {
    if !is_allowed_route(data) {
        return Err(fail(SwapicError::JupiterInstructionNotAllowed));
    }
    let metas: alloc::vec::Vec<InstructionAccount<'_>> = route
        .iter()
        .map(|view| {
            InstructionAccount::new(
                view.address(),
                view.is_writable(),
                key_of(view) == authority,
            )
        })
        .collect();
    let instruction = InstructionView {
        program_id: &JUPITER_ADDRESS,
        data,
        accounts: &metas,
    };
    invoke_signed_with_slice(&instruction, route, signers)
}
