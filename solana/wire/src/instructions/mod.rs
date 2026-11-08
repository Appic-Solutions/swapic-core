//! Builds every instruction the off-chain parts of Swapic put in a Solana transaction, so the
//! quoter, the canister and the tests name each account in the same order the program reads it.

use crate::args::{encode_set_config, Config, TAG_RECLAIM};
use crate::consts::*;
use crate::message::{AccountMeta, Instruction};
use crate::pda::{self, SwapicAddresses};
use crate::Pubkey;
use alloc::vec;
use alloc::vec::Vec;

/// The fixed accounts of `swap_burn` and `burn` (design 4.6), without account 21 and the route.
pub fn burn_head(
    a: &SwapicAddresses,
    user: &Pubkey,
    shard: usize,
    domain: u32,
    float: bool,
    event: &Pubkey,
    event_signs: bool,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta { pubkey: *user, signer: true, writable: !float },
        AccountMeta::readonly(a.config.0),
        AccountMeta { pubkey: a.canister, signer: float, writable: false },
        AccountMeta::readonly(a.auth[shard].0),
        AccountMeta::writable(a.vault[shard]),
        AccountMeta::writable(a.fee_account),
        AccountMeta::writable(a.rent.0),
        AccountMeta { pubkey: *event, signer: event_signs, writable: true },
        AccountMeta::readonly(SENDER_AUTHORITY),
        AccountMeta::readonly(a.denylist[shard]),
        AccountMeta::writable(MESSAGE_TRANSMITTER),
        AccountMeta::readonly(TOKEN_MESSENGER),
        AccountMeta::readonly(pda::remote_token_messenger(domain)),
        AccountMeta::readonly(TOKEN_MINTER),
        AccountMeta::writable(LOCAL_TOKEN),
        AccountMeta::writable(USDC_MINT),
        AccountMeta::readonly(MT),
        AccountMeta::readonly(TMM),
        AccountMeta::readonly(TOKEN_PROGRAM),
        AccountMeta::readonly(SYSTEM_PROGRAM),
        AccountMeta::readonly(TMM_EVENT_AUTHORITY),
    ]
}

/// `swap_burn`: the head, Jupiter, then the route's accounts with their own write flags. The
/// outer signer flags of the route do not matter: the program sets the signer in its CPI.
pub fn swap_burn(a: &SwapicAddresses, head: Vec<AccountMeta>, route: &[AccountMeta], data: Vec<u8>) -> Instruction {
    let mut accounts = head;
    accounts.push(AccountMeta::readonly(JUPITER));
    accounts.extend_from_slice(route);
    Instruction { program: a.program, accounts, data }
}

/// `burn`: the head, then the user's USDC account.
pub fn burn(a: &SwapicAddresses, head: Vec<AccountMeta>, user_usdc: &Pubkey, data: Vec<u8>) -> Instruction {
    let mut accounts = head;
    accounts.push(AccountMeta::writable(*user_usdc));
    Instruction { program: a.program, accounts, data }
}

/// The CCTP accounts of a receive from `source_domain`, whose USDC is `source_token`.
fn receive_tail(nonce: &[u8; 32], source_domain: u32, source_token: &[u8; 32]) -> [AccountMeta; 16] {
    [
        AccountMeta::readonly(MT_AUTHORITY_TMM),
        AccountMeta::readonly(MESSAGE_TRANSMITTER),
        AccountMeta::writable(pda::used_nonce(nonce)),
        AccountMeta::readonly(TMM),
        AccountMeta::readonly(SYSTEM_PROGRAM),
        AccountMeta::readonly(MT_EVENT_AUTHORITY),
        AccountMeta::readonly(MT),
        AccountMeta::readonly(TOKEN_MESSENGER),
        AccountMeta::readonly(pda::remote_token_messenger(source_domain)),
        AccountMeta::writable(TOKEN_MINTER),
        AccountMeta::writable(LOCAL_TOKEN),
        AccountMeta::readonly(pda::token_pair(source_domain, source_token)),
        AccountMeta::writable(FEE_RECIPIENT_TOKEN_ACCOUNT),
        AccountMeta::writable(CUSTODY),
        AccountMeta::readonly(TOKEN_PROGRAM),
        AccountMeta::readonly(TMM_EVENT_AUTHORITY),
    ]
}

/// What identifies the CCTP side of an arrival.
#[derive(Copy, Clone, Debug)]
pub struct ArrivalSource<'a> {
    pub nonce: &'a [u8; 32],
    pub source_domain: u32,
    pub source_token: &'a [u8; 32],
}

/// `complete` (design 4.7): route accounts come after Jupiter with their own write flags.
pub fn complete(
    a: &SwapicAddresses,
    submitter: &Pubkey,
    shard: usize,
    output: &Pubkey,
    source: ArrivalSource<'_>,
    route: &[AccountMeta],
    data: Vec<u8>,
) -> Instruction {
    let mut accounts = vec![
        AccountMeta::writable_signer(*submitter),
        AccountMeta::readonly(a.config.0),
        AccountMeta::readonly(a.auth[shard].0),
        AccountMeta::writable(a.vault[shard]),
        AccountMeta::writable(*output),
        AccountMeta::writable(a.wsol[shard]),
    ];
    accounts.extend_from_slice(&receive_tail(source.nonce, source.source_domain, source.source_token));
    accounts.push(AccountMeta::readonly(JUPITER));
    accounts.extend_from_slice(route);
    Instruction { program: a.program, accounts, data }
}

/// `release` (design 4.8): `caller` signs the receive, `vault` is where the message mints, and
/// `output` is the recipient's USDC account or Swapic's fee account.
pub fn release(
    a: &SwapicAddresses,
    submitter: &Pubkey,
    caller: usize,
    vault: usize,
    output: &Pubkey,
    source: ArrivalSource<'_>,
    data: Vec<u8>,
) -> Instruction {
    let mut accounts = vec![
        AccountMeta::writable_signer(*submitter),
        AccountMeta::readonly(a.config.0),
        AccountMeta::readonly(a.auth[caller].0),
        AccountMeta::writable(a.vault[vault]),
        AccountMeta::writable(*output),
        AccountMeta::readonly(a.auth[vault].0),
    ];
    accounts.extend_from_slice(&receive_tail(source.nonce, source.source_domain, source.source_token));
    Instruction { program: a.program, accounts, data }
}

/// `reclaim` (design 4.9). Anyone may pay; the lamports go to the canister's payer key.
pub fn reclaim(a: &SwapicAddresses, payer: &Pubkey, event: &Pubkey, attestation: &[u8], message: &[u8]) -> Instruction {
    let data = crate::args::ReclaimArgs { attestation, destination_message: message }.encode();
    debug_assert_eq!(data[0], TAG_RECLAIM);
    Instruction {
        program: a.program,
        accounts: vec![
            AccountMeta::signer(*payer),
            AccountMeta::writable(a.rent.0),
            AccountMeta::writable(MESSAGE_TRANSMITTER),
            AccountMeta::writable(*event),
            AccountMeta::readonly(MT),
            AccountMeta::writable(a.canister),
            AccountMeta::readonly(SYSTEM_PROGRAM),
        ],
        data,
    }
}

/// `set_config` (design 4.10), signed by the admin key.
pub fn set_config(a: &SwapicAddresses, config: &Config) -> Instruction {
    Instruction {
        program: a.program,
        accounts: vec![
            AccountMeta::writable_signer(a.admin),
            AccountMeta::writable(a.config.0),
            AccountMeta::readonly(SYSTEM_PROGRAM),
        ],
        data: encode_set_config(config).to_vec(),
    }
}

/// The ATA program's `CreateIdempotent`.
pub fn create_ata_idempotent(payer: &Pubkey, owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Instruction {
    Instruction {
        program: ATA_PROGRAM,
        accounts: vec![
            AccountMeta::writable_signer(*payer),
            AccountMeta::writable(pda::ata(owner, mint, token_program)),
            AccountMeta::readonly(*owner),
            AccountMeta::readonly(*mint),
            AccountMeta::readonly(SYSTEM_PROGRAM),
            AccountMeta::readonly(*token_program),
        ],
        data: vec![1],
    }
}

/// CCTP's `receive_message`, called directly: the canister's relay of a stuck Forwarder message
/// (design 5.2.8), the payer key being both the payer and the caller.
pub fn receive_message(
    payer: &Pubkey,
    mint_recipient: &Pubkey,
    source: ArrivalSource<'_>,
    message: &[u8],
    attestation: &[u8],
) -> Instruction {
    let mut data = vec![0u8; crate::cctp::receive_data_len(message.len(), attestation.len())];
    crate::cctp::write_receive_data(&mut data, message, attestation);
    let tail = receive_tail(source.nonce, source.source_domain, source.source_token);
    let mut accounts = vec![AccountMeta::writable_signer(*payer), AccountMeta::signer(*payer)];
    accounts.extend_from_slice(&tail[..14]);
    // the handler's remaining accounts, in Circle's order: the recipient's token account sits
    // after the fee recipient, then custody, the token program and the event authority
    accounts.insert(15, AccountMeta::writable(*mint_recipient));
    accounts.extend_from_slice(&tail[14..]);
    accounts.push(AccountMeta::readonly(TMM));
    Instruction { program: MT, accounts, data }
}

/// System `transfer`.
pub fn system_transfer(from: &Pubkey, to: &Pubkey, lamports: u64) -> Instruction {
    let mut data = Vec::with_capacity(12);
    data.extend_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program: SYSTEM_PROGRAM,
        accounts: vec![AccountMeta::writable_signer(*from), AccountMeta::writable(*to)],
        data,
    }
}

/// Token `transfer`, signed by the account's owner.
pub fn token_transfer(from: &Pubkey, to: &Pubkey, owner: &Pubkey, amount: u64) -> Instruction {
    let mut data = Vec::with_capacity(9);
    data.push(3);
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program: TOKEN_PROGRAM,
        accounts: vec![AccountMeta::writable(*from), AccountMeta::writable(*to), AccountMeta::signer(*owner)],
        data,
    }
}

/// The upgradeable loader's `Upgrade`, the only loader instruction the canister ever signs.
pub fn loader_upgrade(
    program: &Pubkey,
    buffer: &Pubkey,
    spill: &Pubkey,
    authority: &Pubkey,
) -> Instruction {
    let program_data = pda::find_program_address(&[program], &UPGRADEABLE_LOADER).0;
    Instruction {
        program: UPGRADEABLE_LOADER,
        accounts: vec![
            AccountMeta::writable(program_data),
            AccountMeta::writable(*program),
            AccountMeta::writable(*buffer),
            AccountMeta::writable(*spill),
            AccountMeta::readonly(RENT_SYSVAR),
            AccountMeta::readonly(CLOCK_SYSVAR),
            AccountMeta::signer(*authority),
        ],
        data: 3u32.to_le_bytes().to_vec(),
    }
}

/// ComputeBudget `SetComputeUnitLimit`.
pub fn compute_unit_limit(units: u32) -> Instruction {
    let mut data = Vec::with_capacity(5);
    data.push(2);
    data.extend_from_slice(&units.to_le_bytes());
    Instruction { program: COMPUTE_BUDGET_PROGRAM, accounts: Vec::new(), data }
}

/// ComputeBudget `SetComputeUnitPrice`.
pub fn compute_unit_price(micro_lamports: u64) -> Instruction {
    let mut data = Vec::with_capacity(9);
    data.push(3);
    data.extend_from_slice(&micro_lamports.to_le_bytes());
    Instruction { program: COMPUTE_BUDGET_PROGRAM, accounts: Vec::new(), data }
}

/// Points a `route_v2` straight at where its output must end, in place of the taker's own account
/// for the token (Jeff's rule, 2026-11-09: Swapic's accounts hold USDC and nothing else). Jupiter's
/// answer swaps into the taker's account and forwards at the end, which needs that account to exist;
/// with this the last swap pays the final account and the forward step is gone. Not an option Jupiter
/// offers: the builder simulates the result, and a route that refuses it is not used. Answers how many
/// places it changed. `shared_accounts_route_v2` has no such account and needs nothing.
pub fn route_straight_to(route: &mut [AccountMeta], taker_account: &Pubkey, output: &Pubkey) -> usize {
    let mut changed = 0;
    for meta in route.iter_mut() {
        if meta.pubkey == *taker_account {
            meta.pubkey = *output;
            meta.writable = true;
            changed += 1;
        }
    }
    if let Some(slot) = route.get_mut(crate::jupiter::ROUTE_V2_DESTINATION_OPTIONAL) {
        if slot.pubkey == *output {
            *slot = AccountMeta::readonly(JUPITER);
        }
    }
    changed
}

/// The entries of the frozen Swapic lookup table (design 4.14). A signer never comes from a table:
/// the compiler keeps a signing key static, so the canister's entry serves only a user-paid burn.
pub fn swapic_table_entries(a: &SwapicAddresses) -> Vec<Pubkey> {
    let mut entries: Vec<Pubkey> = Vec::with_capacity(64);
    entries.push(a.config.0);
    entries.extend(a.auth.iter().map(|x| x.0));
    entries.extend_from_slice(&a.vault);
    entries.extend_from_slice(&a.wsol);
    entries.extend_from_slice(&a.denylist);
    entries.extend_from_slice(&[a.rent.0, a.fee_account, a.canister]);
    entries.extend_from_slice(&[
        SENDER_AUTHORITY,
        MESSAGE_TRANSMITTER,
        TOKEN_MESSENGER,
        TOKEN_MINTER,
        LOCAL_TOKEN,
        MT_EVENT_AUTHORITY,
        TMM_EVENT_AUTHORITY,
        MT_AUTHORITY_TMM,
        FEE_RECIPIENT_TOKEN_ACCOUNT,
        CUSTODY,
        MT,
        TMM,
    ]);
    for chain in &SWAPIC_DOMAINS {
        let mut token = [0u8; 32];
        token[12..].copy_from_slice(&chain.usdc);
        entries.push(pda::remote_token_messenger(chain.domain));
        entries.push(pda::token_pair(chain.domain, &token));
    }
    entries.extend_from_slice(&[
        USDC_MINT,
        WSOL_MINT,
        TOKEN_PROGRAM,
        TOKEN_2022_PROGRAM,
        SYSTEM_PROGRAM,
        ATA_PROGRAM,
        JUPITER,
        JUPITER_EVENT_AUTHORITY,
    ]);
    entries
}

/// The lookup table program's `create`. Answers the instruction and the table's address.
pub fn create_lookup_table(authority: &Pubkey, payer: &Pubkey, recent_slot: u64) -> (Instruction, Pubkey) {
    let (table, bump) = pda::find_program_address(&[authority, &recent_slot.to_le_bytes()], &ADDRESS_LOOKUP_TABLE_PROGRAM);
    let mut data = Vec::with_capacity(13);
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&recent_slot.to_le_bytes());
    data.push(bump);
    let ix = Instruction {
        program: ADDRESS_LOOKUP_TABLE_PROGRAM,
        accounts: vec![
            AccountMeta::writable(table),
            AccountMeta::readonly(*authority),
            AccountMeta::writable_signer(*payer),
            AccountMeta::readonly(SYSTEM_PROGRAM),
        ],
        data,
    };
    (ix, table)
}

/// The lookup table program's `extend`.
pub fn extend_lookup_table(table: &Pubkey, authority: &Pubkey, payer: &Pubkey, entries: &[Pubkey]) -> Instruction {
    let mut data = Vec::with_capacity(12 + entries.len() * 32);
    data.extend_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for entry in entries {
        data.extend_from_slice(entry);
    }
    Instruction {
        program: ADDRESS_LOOKUP_TABLE_PROGRAM,
        accounts: vec![
            AccountMeta::writable(*table),
            AccountMeta::signer(*authority),
            AccountMeta::writable_signer(*payer),
            AccountMeta::readonly(SYSTEM_PROGRAM),
        ],
        data,
    }
}

/// The lookup table program's `freeze`: the table's entries never change again.
pub fn freeze_lookup_table(table: &Pubkey, authority: &Pubkey) -> Instruction {
    Instruction {
        program: ADDRESS_LOOKUP_TABLE_PROGRAM,
        accounts: vec![AccountMeta::writable(*table), AccountMeta::signer(*authority)],
        data: 1u32.to_le_bytes().to_vec(),
    }
}

#[cfg(test)]
mod tests;
