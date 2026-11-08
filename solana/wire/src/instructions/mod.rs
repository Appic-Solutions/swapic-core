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

#[cfg(test)]
mod tests;
