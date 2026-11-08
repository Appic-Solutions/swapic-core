//! The calls into the other programs: CCTP's burn, receive and reclaim, System and Token.

use crate::accounts::{MT_ADDRESS, SYSTEM_ADDRESS, TMM_ADDRESS, TOKEN_ADDRESS};
use alloc::vec;
use pinocchio::cpi::{invoke_signed, invoke_signed_with_bounds, Signer};
use pinocchio::instruction::{InstructionAccount, InstructionView};
use pinocchio::{AccountView, ProgramResult};
use swapic_solana_wire::cctp::{reclaim_data_len, receive_data_len, write_reclaim_data, write_receive_data};

/// System `transfer`.
#[inline(never)]
pub fn system_transfer(from: &AccountView, to: &AccountView, lamports: u64, signers: &[Signer<'_, '_>]) -> ProgramResult {
    let mut data = [0u8; 12];
    data[0] = 2;
    data[4..12].copy_from_slice(&lamports.to_le_bytes());
    let metas = [InstructionAccount::writable_signer(from.address()), InstructionAccount::writable(to.address())];
    let instruction = InstructionView { program_id: &SYSTEM_ADDRESS, data: &data, accounts: &metas };
    invoke_signed(&instruction, &[from, to], signers)
}

/// System `allocate`.
pub fn system_allocate(account: &AccountView, space: u64, signers: &[Signer<'_, '_>]) -> ProgramResult {
    let mut data = [0u8; 12];
    data[0] = 8;
    data[4..12].copy_from_slice(&space.to_le_bytes());
    let metas = [InstructionAccount::writable_signer(account.address())];
    let instruction = InstructionView { program_id: &SYSTEM_ADDRESS, data: &data, accounts: &metas };
    invoke_signed(&instruction, &[account], signers)
}

/// System `assign`.
pub fn system_assign(account: &AccountView, owner: &[u8; 32], signers: &[Signer<'_, '_>]) -> ProgramResult {
    let mut data = [0u8; 36];
    data[0] = 1;
    data[4..36].copy_from_slice(owner);
    let metas = [InstructionAccount::writable_signer(account.address())];
    let instruction = InstructionView { program_id: &SYSTEM_ADDRESS, data: &data, accounts: &metas };
    invoke_signed(&instruction, &[account], signers)
}

/// Token `transfer`, signed by `authority`.
#[inline(never)]
pub fn token_transfer(
    from: &AccountView,
    to: &AccountView,
    authority: &AccountView,
    amount: u64,
    signers: &[Signer<'_, '_>],
) -> ProgramResult {
    let mut data = [0u8; 9];
    data[0] = 3;
    data[1..9].copy_from_slice(&amount.to_le_bytes());
    let metas = [
        InstructionAccount::writable(from.address()),
        InstructionAccount::writable(to.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ];
    let instruction = InstructionView { program_id: &TOKEN_ADDRESS, data: &data, accounts: &metas };
    invoke_signed(&instruction, &[from, to, authority], signers)
}

/// The accounts `deposit_for_burn_with_hook` takes, in CCTP's order.
pub struct BurnAccounts<'a> {
    pub owner: &'a AccountView,
    pub rent_payer: &'a AccountView,
    pub sender_authority: &'a AccountView,
    pub burn_token_account: &'a AccountView,
    pub denylist: &'a AccountView,
    pub message_transmitter: &'a AccountView,
    pub token_messenger: &'a AccountView,
    pub remote_token_messenger: &'a AccountView,
    pub token_minter: &'a AccountView,
    pub local_token: &'a AccountView,
    pub mint: &'a AccountView,
    pub event: &'a AccountView,
    pub mt_program: &'a AccountView,
    pub tmm_program: &'a AccountView,
    pub token_program: &'a AccountView,
    pub system_program: &'a AccountView,
    pub event_authority: &'a AccountView,
}

/// CCTP's burn. `data` is the 420 bytes of `wire::cctp::BurnData`.
#[inline(never)]
pub fn deposit_for_burn(a: &BurnAccounts<'_>, data: &[u8], signers: &[Signer<'_, '_>]) -> ProgramResult {
    let metas = [
        InstructionAccount::readonly_signer(a.owner.address()),
        InstructionAccount::writable_signer(a.rent_payer.address()),
        InstructionAccount::readonly(a.sender_authority.address()),
        InstructionAccount::writable(a.burn_token_account.address()),
        InstructionAccount::readonly(a.denylist.address()),
        InstructionAccount::writable(a.message_transmitter.address()),
        InstructionAccount::readonly(a.token_messenger.address()),
        InstructionAccount::readonly(a.remote_token_messenger.address()),
        InstructionAccount::readonly(a.token_minter.address()),
        InstructionAccount::writable(a.local_token.address()),
        InstructionAccount::writable(a.mint.address()),
        InstructionAccount::writable_signer(a.event.address()),
        InstructionAccount::readonly(a.mt_program.address()),
        InstructionAccount::readonly(a.tmm_program.address()),
        InstructionAccount::readonly(a.token_program.address()),
        InstructionAccount::readonly(a.system_program.address()),
        InstructionAccount::readonly(a.event_authority.address()),
        InstructionAccount::readonly(a.tmm_program.address()),
    ];
    let views = [
        a.owner,
        a.rent_payer,
        a.sender_authority,
        a.burn_token_account,
        a.denylist,
        a.message_transmitter,
        a.token_messenger,
        a.remote_token_messenger,
        a.token_minter,
        a.local_token,
        a.mint,
        a.event,
        a.mt_program,
        a.tmm_program,
        a.token_program,
        a.system_program,
        a.event_authority,
        a.tmm_program,
    ];
    let instruction = InstructionView { program_id: &TMM_ADDRESS, data, accounts: &metas };
    invoke_signed_with_bounds::<18, _>(&instruction, &views, signers)
}

/// `receive_message` for a mint into `vault`, `accounts` being the arrival head of design 4.7.
/// `caller` signs through `signers`.
#[inline(never)]
pub fn receive_message(
    accounts: &[AccountView],
    caller: &AccountView,
    vault: &AccountView,
    message: &[u8],
    attestation: &[u8],
    signers: &[Signer<'_, '_>],
) -> ProgramResult {
    let mut data = vec![0u8; receive_data_len(message.len(), attestation.len())];
    write_receive_data(&mut data, message, attestation);
    let a = accounts;
    let metas = [
        InstructionAccount::writable_signer(a[0].address()),
        InstructionAccount::readonly_signer(caller.address()),
        InstructionAccount::readonly(a[6].address()),
        InstructionAccount::readonly(a[7].address()),
        InstructionAccount::writable(a[8].address()),
        InstructionAccount::readonly(a[9].address()),
        InstructionAccount::readonly(a[10].address()),
        InstructionAccount::readonly(a[11].address()),
        InstructionAccount::readonly(a[12].address()),
        InstructionAccount::readonly(a[13].address()),
        InstructionAccount::readonly(a[14].address()),
        InstructionAccount::writable(a[15].address()),
        InstructionAccount::writable(a[16].address()),
        InstructionAccount::readonly(a[17].address()),
        InstructionAccount::writable(a[18].address()),
        InstructionAccount::writable(vault.address()),
        InstructionAccount::writable(a[19].address()),
        InstructionAccount::readonly(a[20].address()),
        InstructionAccount::readonly(a[21].address()),
        InstructionAccount::readonly(a[9].address()),
    ];
    let views = [
        &a[0], caller, &a[6], &a[7], &a[8], &a[9], &a[10], &a[11], &a[12], &a[13], &a[14], &a[15], &a[16], &a[17],
        &a[18], vault, &a[19], &a[20], &a[21], &a[9],
    ];
    let instruction = InstructionView { program_id: &MT_ADDRESS, data: &data, accounts: &metas };
    invoke_signed_with_bounds::<20, _>(&instruction, &views, signers)
}

/// `reclaim_event_account`, `payee` signing.
#[inline(never)]
pub fn reclaim_event_account(
    payee: &AccountView,
    message_transmitter: &AccountView,
    event: &AccountView,
    attestation: &[u8],
    destination_message: &[u8],
    signers: &[Signer<'_, '_>],
) -> ProgramResult {
    let mut data = vec![0u8; reclaim_data_len(attestation.len(), destination_message.len())];
    write_reclaim_data(&mut data, attestation, destination_message);
    let metas = [
        InstructionAccount::writable_signer(payee.address()),
        InstructionAccount::writable(message_transmitter.address()),
        InstructionAccount::writable(event.address()),
    ];
    let instruction = InstructionView { program_id: &MT_ADDRESS, data: &data, accounts: &metas };
    invoke_signed(&instruction, &[payee, message_transmitter, event], signers)
}
