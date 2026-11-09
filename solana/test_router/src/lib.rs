//! A stand-in for the Jupiter router in the program's tests, placed at Jupiter's address. It reads
//! its behaviour from the bytes after the steps' length. Hostile modes do what the attack notes (design
//! 8a) fear a route could do. Never deployed to mainnet.
//!
//! Data: a route discriminator, in_amount u64, quoted_out u64, slippage u16, two fee u16, an empty
//! steps vector, then mode u8 and arg u64.
//!
//! Accounts: 0 authority, 1 source, 2 destination, 3 and 4 mints, 5 and 6 token programs, 7 the
//! optional destination, 8 event authority, 9 the router, 10 pool_in, 11 pool_out, 12 the pool's PDA,
//! 13.. whatever the mode needs.
#![allow(unexpected_cfgs)]

use pinocchio::cpi::{invoke_signed, invoke_signed_with_slice, Seed, Signer};
use pinocchio::error::ProgramError;
use pinocchio::instruction::{InstructionAccount, InstructionView};
use pinocchio::{AccountView, Address, ProgramResult};
use swapic_solana_wire::consts::{SYSTEM_PROGRAM, TOKEN_2022_PROGRAM, TOKEN_PROGRAM};

#[cfg(target_os = "solana")]
pinocchio::entrypoint!(process);

pub const POOL_SEED: &[u8] = b"pool";
const TOKEN: Address = Address::new_from_array(TOKEN_PROGRAM);
const SYSTEM: Address = Address::new_from_array(SYSTEM_PROGRAM);
const TOKEN_2022: Address = Address::new_from_array(TOKEN_2022_PROGRAM);

pub const HONEST: u8 = 0;
pub const PAY_SHORT: u8 = 1;
pub const SPEND_PART: u8 = 2;
pub const APPROVE_DELEGATE: u8 = 3;
pub const SET_OWNER: u8 = 4;
pub const SET_CLOSE_AUTHORITY: u8 = 5;
pub const ASSIGN_AUTH: u8 = 6;
pub const REENTER: u8 = 7;
pub const NATIVE_CLOSING: u8 = 8;
pub const NATIVE_DIRECT: u8 = 9;
pub const NOTHING: u8 = 10;
pub const ALLOCATE_AUTH: u8 = 11;
pub const FUND_AUTH: u8 = 12;
pub const CLOSE_WSOL: u8 = 13;
pub const PAY_2022: u8 = 14;

fn token_call(
    tag_and_data: &[u8],
    metas: &[InstructionAccount<'_>],
    views: &[&AccountView],
    signers: &[Signer<'_, '_>],
) -> ProgramResult {
    let ix = InstructionView {
        program_id: &TOKEN,
        data: tag_and_data,
        accounts: metas,
    };
    invoke_signed_with_slice(&ix, views, signers)
}

fn transfer(
    from: &AccountView,
    to: &AccountView,
    authority: &AccountView,
    amount: u64,
    signers: &[Signer<'_, '_>],
) -> ProgramResult {
    let mut data = [0u8; 9];
    data[0] = 3;
    data[1..].copy_from_slice(&amount.to_le_bytes());
    let metas = [
        InstructionAccount::writable(from.address()),
        InstructionAccount::writable(to.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ];
    token_call(&data, &metas, &[from, to, authority], signers)
}

pub fn process(program: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let a: &[AccountView] = accounts;
    if data.len() < 8 + 8 + 8 + 2 + 4 + 4 + 1 + 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let in_amount = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let quoted = u64::from_le_bytes(data[16..24].try_into().unwrap());
    let mode = data[34];
    let arg = u64::from_le_bytes(data[35..43].try_into().unwrap());

    let (pda, bump) = Address::find_program_address(&[POOL_SEED], program);
    if a[12].address() != &pda {
        return Err(ProgramError::InvalidSeeds);
    }
    let bump_seed = [bump];
    let seeds = [Seed::from(POOL_SEED), Seed::from(&bump_seed)];
    let pool_signer = [Signer::from(&seeds)];

    match mode {
        HONEST | PAY_SHORT | SPEND_PART | NATIVE_CLOSING | NATIVE_DIRECT | CLOSE_WSOL => {
            let spend = if mode == SPEND_PART { arg } else { in_amount };
            transfer(&a[1], &a[10], &a[0], spend, &[])?;
            let pay = if mode == PAY_SHORT { arg } else { quoted };
            match mode {
                NATIVE_CLOSING => {
                    // to `wsol[i]`, then unwrap by closing it to the wallet, signed by `auth[i]`
                    transfer(&a[11], &a[2], &a[12], pay, &pool_signer)?;
                    close_account(&a[2], &a[7], &a[0])?;
                }
                NATIVE_DIRECT => {
                    // plain lamports to the wallet: the pool's PDA is the payer
                    let mut d = [0u8; 12];
                    d[0] = 2;
                    d[4..12].copy_from_slice(&pay.to_le_bytes());
                    let metas = [
                        InstructionAccount::writable_signer(a[12].address()),
                        InstructionAccount::writable(a[7].address()),
                    ];
                    let ix = InstructionView {
                        program_id: &SYSTEM,
                        data: &d,
                        accounts: &metas,
                    };
                    invoke_signed(&ix, &[&a[12], &a[7]], &pool_signer)?;
                }
                CLOSE_WSOL => {
                    // the arrival is not native, yet the route closes `wsol[i]` (account 13)
                    transfer(&a[11], &a[2], &a[12], pay, &pool_signer)?;
                    close_account(&a[13], &a[7], &a[0])?;
                }
                _ => transfer(&a[11], &a[2], &a[12], pay, &pool_signer)?,
            }
            Ok(())
        }
        NOTHING => Ok(()),
        PAY_2022 => {
            transfer(&a[1], &a[10], &a[0], in_amount, &[])?;
            // the same transfer, through Token-2022
            let mut d = [0u8; 9];
            d[0] = 3;
            d[1..].copy_from_slice(&quoted.to_le_bytes());
            let metas = [
                InstructionAccount::writable(a[11].address()),
                InstructionAccount::writable(a[2].address()),
                InstructionAccount::readonly_signer(a[12].address()),
            ];
            let ix = InstructionView {
                program_id: &TOKEN_2022,
                data: &d,
                accounts: &metas,
            };
            invoke_signed_with_slice(&ix, &[&a[11], &a[2], &a[12]], &pool_signer)
        }
        APPROVE_DELEGATE => {
            transfer(&a[1], &a[10], &a[0], in_amount, &[])?;
            transfer(&a[11], &a[2], &a[12], quoted, &pool_signer)?;
            let mut d = [0u8; 9];
            d[0] = 4;
            d[1..].copy_from_slice(&1u64.to_le_bytes());
            let metas = [
                InstructionAccount::writable(a[1].address()),
                InstructionAccount::readonly(a[13].address()),
                InstructionAccount::readonly_signer(a[0].address()),
            ];
            token_call(&d, &metas, &[&a[1], &a[13], &a[0]], &[])
        }
        SET_OWNER | SET_CLOSE_AUTHORITY => {
            transfer(&a[1], &a[10], &a[0], in_amount, &[])?;
            transfer(&a[11], &a[2], &a[12], quoted, &pool_signer)?;
            let mut d = [0u8; 35];
            d[0] = 6;
            d[1] = if mode == SET_OWNER { 2 } else { 3 };
            d[2] = 1;
            d[3..35].copy_from_slice(a[13].address().as_ref());
            let metas = [
                InstructionAccount::writable(a[1].address()),
                InstructionAccount::readonly_signer(a[0].address()),
            ];
            token_call(&d, &metas, &[&a[1], &a[0]], &[])
        }
        ASSIGN_AUTH | ALLOCATE_AUTH => {
            transfer(&a[1], &a[10], &a[0], in_amount, &[])?;
            transfer(&a[11], &a[2], &a[12], quoted, &pool_signer)?;
            let mut d = [0u8; 36];
            let n = if mode == ASSIGN_AUTH {
                d[0] = 1;
                d[4..36].copy_from_slice(program.as_ref());
                36
            } else {
                d[0] = 8;
                d[4..12].copy_from_slice(&8u64.to_le_bytes());
                12
            };
            let metas = [InstructionAccount::writable_signer(a[0].address())];
            let ix = InstructionView {
                program_id: &SYSTEM,
                data: &d[..n],
                accounts: &metas,
            };
            invoke_signed(&ix, &[&a[0]], &[])
        }
        FUND_AUTH => {
            transfer(&a[1], &a[10], &a[0], in_amount, &[])?;
            transfer(&a[11], &a[2], &a[12], quoted, &pool_signer)?;
            let mut d = [0u8; 12];
            d[0] = 2;
            d[4..12].copy_from_slice(&arg.to_le_bytes());
            let metas = [
                InstructionAccount::writable_signer(a[12].address()),
                InstructionAccount::writable(a[0].address()),
            ];
            let ix = InstructionView {
                program_id: &SYSTEM,
                data: &d,
                accounts: &metas,
            };
            invoke_signed(&ix, &[&a[12], &a[0]], &pool_signer)
        }
        REENTER => {
            // call the Swapic program (account 13) with its signer privilege, carrying `data` after the mode
            let target = a[13].address();
            let inner = &data[43..];
            let rest = &a[14..];
            let metas: Vec<InstructionAccount<'_>> = rest
                .iter()
                .map(|v| InstructionAccount::new(v.address(), v.is_writable(), v.is_signer()))
                .collect();
            let ix = InstructionView {
                program_id: target,
                data: inner,
                accounts: &metas,
            };
            invoke_signed_with_slice(&ix, rest, &[])
        }
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn close_account(account: &AccountView, to: &AccountView, owner: &AccountView) -> ProgramResult {
    let metas = [
        InstructionAccount::writable(account.address()),
        InstructionAccount::writable(to.address()),
        InstructionAccount::readonly_signer(owner.address()),
    ];
    token_call(&[9], &metas, &[account, to, owner], &[])
}
