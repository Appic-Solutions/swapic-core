//! Burns into Solana (design 5.2.6, 5.2.7, 8c): what the canister accepts as an arrival, and the
//! transaction it builds to finish one. The program is the wall: the recipient's delta, the vault's
//! baseline and the route's refusal list are its checks. The rules here bound what the canister spends
//! and what it signs its name to.

#[cfg(test)]
mod tests;

use settlement_api::types::solana::SolanaRoute;
use swapic_solana_wire::args::{ArrivalArgs, TAG_COMPLETE, TAG_RELEASE};
use swapic_solana_wire::base58;
use swapic_solana_wire::cctp::Message;
use swapic_solana_wire::consts::*;
use swapic_solana_wire::envelope::{ForwarderHook, SolanaEnvelope, SOLANA_ACTION_SWAP};
use swapic_solana_wire::instructions::{self, ArrivalSource};
use swapic_solana_wire::jupiter;
use swapic_solana_wire::message::{AccountMeta, Instruction, MAX_TX_ACCOUNTS};
use swapic_solana_wire::pda::{self, is_on_curve, SwapicAddresses};
use types::solana::SolanaConfig;

/// A burn into Solana, read from its fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolanaBurn {
    /// Into the program: the hook's terms, on a vault and auth of one shard.
    Swap(SolanaEnvelope),
    /// USDC to its owner through Circle's Forwarder.
    Forwarder { owner: [u8; 32] },
}

/// The fields of a burn that decide its arrival.
pub struct BurnFields<'a> {
    pub source_domain: u32,
    pub mint_recipient: &'a [u8; 32],
    pub destination_caller: &'a [u8; 32],
    pub hook: &'a [u8],
    pub max_fee: u128,
}

/// The arrival of a burn into Solana, as the program will read it (design 5.2.6). `read_owner` is the
/// owner the 32-byte Forwarder form's token account holds, read from Solana by the door. Pure.
pub fn read_burn(
    burn: &BurnFields<'_>,
    a: &SwapicAddresses,
    config: &SolanaConfig,
    read_owner: Option<[u8; 32]>,
    now_s: u64,
) -> Result<SolanaBurn, &'static str> {
    if let Some(shard) = a.vault_index(burn.mint_recipient) {
        if burn.destination_caller != &a.auth[shard].0 {
            return Err("a burn into the program is called by its vault's own auth");
        }
        let hook =
            SolanaEnvelope::parse(burn.hook).map_err(|_| "the hook is not a Solana envelope")?;
        if hook.action != SOLANA_ACTION_SWAP {
            return Err("the hook's action is not a swap");
        }
        check_recipient(&hook.recipient, a)?;
        if hook.mint_out == USDC_MINT {
            return Err("a swap to USDC goes through the Forwarder");
        }
        if hook.min_out == 0 {
            return Err("the hook names no minimum out");
        }
        if (hook.deadline as u64) < now_s + config.complete_margin_s {
            return Err("the hook's deadline leaves no time to complete");
        }
        return Ok(SolanaBurn::Swap(hook));
    }
    if burn.destination_caller != &[0u8; 32] {
        return Err("a Forwarder burn has no destination caller");
    }
    if burn.source_domain == PLASMA_DOMAIN {
        return Err("Plasma has no Forwarder");
    }
    let hook = ForwarderHook::parse(burn.hook).ok_or("the hook is not a Forwarder hook")?;
    let owner = match hook {
        ForwarderHook::CreateFor(owner) => owner,
        ForwarderHook::Existing => read_owner.ok_or("the token account's owner was not read")?,
    };
    check_recipient(&owner, a)?;
    // never another token account, on either form: only its keypair holder could recreate it (attack L3)
    if *burn.mint_recipient != pda::ata(&owner, &USDC_MINT, &TOKEN_PROGRAM) {
        return Err("a Forwarder burn mints to its owner's USDC account");
    }
    if burn.max_fee > config.max_forward_fee as u128 {
        return Err("the Forwarder burn's max fee is above the margin");
    }
    Ok(SolanaBurn::Forwarder { owner })
}

/// A wallet on the curve that is none of Swapic's accounts.
pub fn check_recipient(recipient: &[u8; 32], a: &SwapicAddresses) -> Result<(), &'static str> {
    if !is_on_curve(recipient) {
        return Err("the recipient is not a wallet");
    }
    if a.is_swapic_account(recipient) {
        return Err("the recipient is one of Swapic's accounts");
    }
    Ok(())
}

/// A route the watcher handed over, read and held to rule 7 of design 5.2.7. It never carries a
/// signer flag: the program sets the one signer.
pub fn read_route(
    route: &SolanaRoute,
    a: &SwapicAddresses,
    create_accounts: usize,
) -> Result<Vec<AccountMeta>, String> {
    if !jupiter::is_allowed_route(&route.data) {
        return Err("the route is not one of Jupiter's two route instructions".to_string());
    }
    if route.data.len() > 1024 {
        return Err("the route's data is above 1,024 bytes".to_string());
    }
    let most = MAX_TX_ACCOUNTS.saturating_sub(23 + 3 * create_accounts);
    if route.accounts.len() > most {
        return Err(format!(
            "the route names {} accounts, above {most}",
            route.accounts.len()
        ));
    }
    let mut metas = Vec::with_capacity(route.accounts.len());
    for account in &route.accounts {
        let key = base58::decode_pubkey(&account.address).ok_or("a route account is not base58")?;
        if key == a.canister
            || key == a.admin
            || key == a.config.0
            || key == a.rent.0
            || key == a.program
            || key == MT
            || key == TMM
        {
            return Err("the route names an account of Swapic's or CCTP's".to_string());
        }
        metas.push(AccountMeta {
            pubkey: key,
            signer: false,
            writable: account.writable,
        });
    }
    Ok(metas)
}

/// The `complete` transaction's instructions: the accounts it may need, then the call.
#[allow(clippy::too_many_arguments)]
pub fn build_complete(
    a: &SwapicAddresses,
    hook: &SolanaEnvelope,
    shard: usize,
    nonce: &[u8; 32],
    source_domain: u32,
    source_token: &[u8; 32],
    message: &[u8],
    attestation: &[u8],
    route: &SolanaRoute,
    route_metas: &[AccountMeta],
    create_account: bool,
    token_2022: bool,
) -> Vec<Instruction> {
    let native = hook.mint_out == WSOL_MINT;
    let token_program = if token_2022 {
        TOKEN_2022_PROGRAM
    } else {
        TOKEN_PROGRAM
    };
    let output = if native {
        hook.recipient
    } else {
        pda::ata(&hook.recipient, &hook.mint_out, &token_program)
    };
    let mut all = Vec::with_capacity(3);
    // a route may have closed it, and creating it again costs nothing when it is there (attack M3)
    if native {
        all.push(instructions::create_ata_idempotent(
            &a.canister,
            &a.auth[shard].0,
            &WSOL_MINT,
            &TOKEN_PROGRAM,
        ));
    } else if create_account {
        all.push(instructions::create_ata_idempotent(
            &a.canister,
            &hook.recipient,
            &hook.mint_out,
            &token_program,
        ));
    }
    let data = ArrivalArgs {
        tag: TAG_COMPLETE,
        message,
        attestation,
        jupiter: &route.data,
    }
    .encode();
    let source = ArrivalSource {
        nonce,
        source_domain,
        source_token,
    };
    all.push(instructions::complete(
        a,
        &a.canister,
        shard,
        &output,
        source,
        route_metas,
        data,
    ));
    all
}

/// The `release` transaction's instructions: the USDC account when it may be missing, then the call.
#[allow(clippy::too_many_arguments)]
pub fn build_release(
    a: &SwapicAddresses,
    caller: usize,
    vault: usize,
    recipient: Option<&[u8; 32]>,
    nonce: &[u8; 32],
    source_domain: u32,
    source_token: &[u8; 32],
    message: &[u8],
    attestation: &[u8],
    create_account: bool,
) -> Vec<Instruction> {
    let data = ArrivalArgs {
        tag: TAG_RELEASE,
        message,
        attestation,
        jupiter: &[],
    }
    .encode();
    let source = ArrivalSource {
        nonce,
        source_domain,
        source_token,
    };
    let mut all = Vec::with_capacity(2);
    let output = match recipient {
        Some(recipient) => {
            if create_account {
                all.push(instructions::create_ata_idempotent(
                    &a.canister,
                    recipient,
                    &USDC_MINT,
                    &TOKEN_PROGRAM,
                ));
            }
            pda::ata(recipient, &USDC_MINT, &TOKEN_PROGRAM)
        }
        None => a.fee_account,
    };
    all.push(instructions::release(
        a,
        &a.canister,
        caller,
        vault,
        &output,
        source,
        data,
    ));
    all
}

/// The two shards a message names, and whether it reads as the program's: the caller's and the vault's.
pub fn shards_of(message: &Message<'_>, a: &SwapicAddresses) -> Option<(usize, usize)> {
    Some((
        a.auth_index(message.destination_caller())?,
        a.vault_index(message.mint_recipient())?,
    ))
}

/// The owner a 32-byte Forwarder hook's token account holds, read from Solana, when the order has such
/// a burn: the door reads it before the pure checks, which then hold the mint recipient to its ATA.
pub async fn forward_owner_of(
    order: &types::swapic::Order,
    d: &super::Deployment,
    source: &types::config::EvmChain,
) -> Result<Option<[u8; 32]>, String> {
    for step in &order.steps {
        let Some(Ok(burn)) = types::swapic::CctpBurn::of_step(step, source.token_messenger) else {
            continue;
        };
        if burn.destination_domain != SOLANA_DOMAIN
            || ForwarderHook::parse(&burn.hook_data) != Some(ForwarderHook::Existing)
        {
            continue;
        }
        let account = super::rpc::account(d.config.sol_rpc, &burn.mint_recipient, Some(64), 1024)
            .await?
            .ok_or("the Forwarder's token account does not exist")?;
        if account.data.get(..32) != Some(&USDC_MINT[..]) {
            return Err("the Forwarder's token account is not USDC".to_string());
        }
        return Ok(account
            .data
            .get(32..64)
            .and_then(|owner| owner.try_into().ok()));
    }
    Ok(None)
}
