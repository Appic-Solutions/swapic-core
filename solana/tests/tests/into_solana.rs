//! `complete` and `release` against the real CCTP programs (design 4.7, 4.8, 8a).

mod common;

use common::*;
use swapic_solana_tests::route::*;
use swapic_solana_tests::wire::consts::*;
use swapic_solana_tests::wire::envelope::{SolanaEnvelope, SOLANA_ACTION_DELIVER};
use swapic_solana_tests::wire::errors::SwapicError as E;
use swapic_solana_tests::wire::instructions;
use swapic_solana_tests::wire::message::AccountMeta;
use swapic_solana_tests::wire::pda;
use swapic_solana_tests::*;

fn canister(env: &Env) -> [u8; 32] {
    pubkey_of(&env.canister)
}

/// Sends `complete` as the canister and answers its failure code.
fn refused(env: &mut Env, arrival: &Arrival, mode: u8, extra: &[AccountMeta]) -> u32 {
    let ix = arrival.complete(env, &canister(env), &env.recipient_out.clone(), mode, OUT, MINTED / 2, extra);
    let payer = env.canister.insecure_clone();
    custom_code(env.send(&[ix], &payer, &[]))
}

#[test]
fn the_canister_completes_a_swap_and_the_recipient_gets_all_of_it() {
    let mut env = Env::new();
    for shard in 0..4 {
        let arrival = Arrival::new(&env, &format!("happy-{shard}"), shard, |_| {});
        let before = env.balance(&env.recipient_out);
        let vault_before = env.balance(&env.a.vault[shard]);
        let payer = env.canister.insecure_clone();
        let ix = arrival.complete(&env, &canister(&env), &env.recipient_out, HONEST, OUT, 0, &[]);
        let meta = env.send_ok(&[ix], &payer, &[]);
        assert_eq!(env.balance(&env.recipient_out) - before, OUT);
        assert_eq!(env.balance(&env.a.vault[shard]), vault_before, "the vault is back at its baseline");
        assert!(env.exists(&pda::used_nonce(&arrival.nonce)));
        let own = own_compute(&meta.logs, &env.a.program);
        assert!(own <= 25_000, "own compute {own}");
    }
}

#[test]
fn the_recipient_may_submit_their_own_complete() {
    let mut env = Env::new();
    let arrival = Arrival::new(&env, "self", 1, |_| {});
    let recipient = env.recipient.insecure_clone();
    let ix = arrival.complete(&env, &pubkey_of(&recipient), &env.recipient_out, HONEST, OUT, 0, &[]);
    env.send_ok(&[ix], &recipient, &[]);
    assert_eq!(env.balance(&env.recipient_out), OUT);
}

#[test]
fn a_native_arrival_measures_output_after_the_receive_when_the_recipient_pays_the_nonce() {
    // section 0: a recipient that submits pays the used_nonce rent inside the receive
    let mut env = Env::new();
    let arrival = Arrival::new(&env, "native-self", 0, |h| {
        h.mint_out = WSOL_MINT;
        h.min_out = OUT;
    });
    let recipient = env.recipient.insecure_clone();
    let ix = arrival.complete_native(&env, &pubkey_of(&recipient), NATIVE_DIRECT, OUT);
    env.send_ok(&[ix], &recipient, &[]);
    assert_eq!(env.balance(&env.a.wsol[0]), 0);
}

#[test]
fn a_native_arrival_may_close_wsol_and_its_rent_is_not_counted_as_output() {
    let mut env = Env::new();
    let wsol = env.a.wsol[2];
    let rent = env.lamports(&wsol);
    // min_out sits between the output and the output plus the rent: counting the rent would pass it
    let arrival = Arrival::new(&env, "native-close", 2, |h| {
        h.mint_out = WSOL_MINT;
        h.min_out = OUT + rent;
    });
    let payer = env.canister.insecure_clone();
    let ix = arrival.complete_native(&env, &canister(&env), NATIVE_CLOSING, OUT);
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::BelowMinOut.code());

    let arrival = Arrival::new(&env, "native-close-2", 2, |h| {
        h.mint_out = WSOL_MINT;
        h.min_out = OUT;
    });
    let wallet = pubkey_of(&env.recipient);
    let before = env.lamports(&wallet);
    let ix = arrival.complete_native(&env, &canister(&env), NATIVE_CLOSING, OUT);
    env.send_ok(&[ix], &payer, &[]);
    assert_eq!(env.lamports(&wallet) - before, OUT + rent);
    assert!(!env.exists(&wsol), "the route closed it");

    // the shard keeps working while it is closed: the next token arrival does not need it open
    let arrival = Arrival::new(&env, "after-close", 2, |_| {});
    let ix = arrival.complete(&env, &canister(&env), &env.recipient_out, HONEST, OUT, 0, &[]);
    env.send_ok(&[ix], &payer, &[]);

    // and the idempotent create before the next native arrival brings it back
    let arrival = Arrival::new(&env, "native-again", 2, |h| {
        h.mint_out = WSOL_MINT;
        h.min_out = OUT;
    });
    let create = instructions::create_ata_idempotent(&canister(&env), &env.a.auth[2].0, &WSOL_MINT, &TOKEN_PROGRAM);
    let ix = arrival.complete_native(&env, &canister(&env), NATIVE_CLOSING, OUT);
    env.send_ok(&[create, ix], &payer, &[]);
}

#[test]
fn a_token_arrival_may_not_close_wsol() {
    let mut env = Env::new();
    let arrival = Arrival::new(&env, "close-wsol", 0, |_| {});
    let wsol = env.a.wsol[0];
    let code = refused(&mut env, &arrival, CLOSE_WSOL, &[AccountMeta::writable(wsol)]);
    assert_eq!(code, E::WsolState.code());
}

#[test]
fn a_route_cannot_leave_a_standing_right_on_the_vault_or_on_auth() {
    // attack H1: every amount ends at its baseline, yet the shard would be stuck for good
    let mut env = Env::new();
    let attacker = pubkey_of(&env.stranger);
    let mut n = 0;
    let mut go = |env: &mut Env, mode: u8, want: E| {
        n += 1;
        let arrival = Arrival::new(env, &format!("standing-{n}"), 0, |_| {});
        let code = refused(env, &arrival, mode, &[AccountMeta::readonly(attacker)]);
        assert_eq!(code, want.code(), "mode {mode}");
    };
    go(&mut env, APPROVE_DELEGATE, E::VaultStateChanged);
    go(&mut env, SET_OWNER, E::VaultStateChanged);
    go(&mut env, SET_CLOSE_AUTHORITY, E::VaultStateChanged);
    go(&mut env, ASSIGN_AUTH, E::AuthStateChanged);
    go(&mut env, ALLOCATE_AUTH, E::AuthStateChanged);
    go(&mut env, FUND_AUTH, E::AuthStateChanged);
}

#[test]
fn a_donation_to_auth_stops_nothing() {
    // a stranger can send SOL to any address: a shard must keep working
    let mut env = Env::new();
    let auth = env.a.auth[3].0;
    env.fund(&auth, 10_000_000);
    let payer = env.canister.insecure_clone();
    let arrival = Arrival::new(&env, "donated", 3, |_| {});
    let ix = arrival.complete(&env, &canister(&env), &env.recipient_out, HONEST, OUT, 0, &[]);
    env.send_ok(&[ix], &payer, &[]);
    let arrival = Arrival::new(&env, "donated-release", 3, |_| {});
    let ix = arrival.release(&env, &canister(&env), 3, 3, &env.recipient_usdc);
    env.send_ok(&[ix], &payer, &[]);
    assert_eq!(env.lamports(&auth), 10_000_000);
}

#[test]
fn a_donation_to_a_vault_is_never_spent_or_counted() {
    let mut env = Env::new();
    let vault = env.a.vault[1];
    let auth = env.a.auth[1].0;
    env.put_token(&vault, &USDC_MINT, &auth, 777);
    let payer = env.canister.insecure_clone();
    let arrival = Arrival::new(&env, "donated-vault", 1, |_| {});
    let ix = arrival.complete(&env, &canister(&env), &env.recipient_out, HONEST, OUT, 0, &[]);
    env.send_ok(&[ix], &payer, &[]);
    assert_eq!(env.balance(&vault), 777);
}

#[test]
fn all_or_nothing_holds_against_a_short_or_partial_route() {
    let mut env = Env::new();
    let arrival = Arrival::new(&env, "nothing", 0, |_| {});
    assert_eq!(refused(&mut env, &arrival, NOTHING, &[]), E::VaultNotAtBaseline.code());
    assert_eq!(refused(&mut env, &arrival, SPEND_PART, &[]), E::VaultNotAtBaseline.code());
    // pays less than min_out
    let ix = arrival.complete(&env, &canister(&env), &env.recipient_out, PAY_SHORT, OUT, MIN_OUT - 1, &[]);
    let payer = env.canister.insecure_clone();
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::BelowMinOut.code());
    // nothing moved
    assert_eq!(env.balance(&env.recipient_out), 0);
    assert!(!env.exists(&pda::used_nonce(&arrival.nonce)));
}

#[test]
fn only_the_two_jupiter_routes_get_through() {
    let mut env = Env::new();
    let arrival = Arrival::new(&env, "disc", 0, |_| {});
    let (_, token) = env.source();
    let route = route_accounts(&env.a.auth[0].0, &env.a.vault[0], &env.recipient_out, &pubkey_of(&env.recipient), &env.pool_usdc, &env.pool_out, &env.pool, &[]);
    let source = instructions::ArrivalSource { nonce: &arrival.nonce, source_domain: 6, source_token: &token };
    let payer = env.canister.insecure_clone();
    for discriminator in [[229u8, 23, 203, 151, 122, 227, 173, 42], [193, 32, 155, 51, 65, 214, 156, 129], [0; 8]] {
        let mut jupiter = route::data(MINTED, OUT, HONEST, 0);
        jupiter[..8].copy_from_slice(&discriminator);
        let ix = instructions::complete(&env.a, &canister(&env), 0, &env.recipient_out, source, &route, arrival.complete_data(&jupiter));
        assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::JupiterInstructionNotAllowed.code());
    }
    // the shared form passes the check (the stand-in reads the same offsets)
    let ix = instructions::complete(&env.a, &canister(&env), 0, &env.recipient_out, source, &route, arrival.complete_data(&route::shared(MINTED, OUT, HONEST)));
    env.send_ok(&[ix], &payer, &[]);
}

#[test]
fn a_route_may_not_name_swapics_accounts() {
    let mut env = Env::new();
    let arrival = Arrival::new(&env, "names", 0, |_| {});
    let a = env.a.clone();
    let forbidden = [a.config.0, a.rent.0, a.canister, a.admin, a.program, MT, TMM, a.auth[1].0, a.vault[1], a.auth[3].0, a.vault[3]];
    for key in forbidden {
        let code = refused(&mut env, &arrival, HONEST, &[AccountMeta::readonly(key)]);
        assert_eq!(code, E::ForbiddenRouteAccount.code());
    }
}

#[test]
fn the_output_must_be_the_recipients_account_for_the_mint() {
    let mut env = Env::new();
    let arrival = Arrival::new(&env, "output", 0, |_| {});
    let payer = env.canister.insecure_clone();
    let can = canister(&env);
    // someone else's account for the mint
    let other = pubkey_of(&test_key("other-account"));
    let stranger_key = pubkey_of(&env.stranger);
    env.put_token(&other, &env.mint_out.clone(), &stranger_key, 0);
    let ix = arrival.complete(&env, &can, &other, HONEST, OUT, 0, &[]);
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::BadOutputAccount.code());
    // the recipient's account for another mint
    let ix = arrival.complete(&env, &can, &env.recipient_usdc, HONEST, OUT, 0, &[]);
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::BadOutputAccount.code());
    // a native arrival names the wallet, and nothing else
    let native = Arrival::new(&env, "output-native", 0, |h| h.mint_out = WSOL_MINT);
    let ix = native.complete(&env, &can, &env.recipient_out, NATIVE_DIRECT, OUT, 0, &[]);
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::BadOutputAccount.code());
}

#[test]
fn only_the_canister_or_the_recipient_may_complete() {
    let mut env = Env::new();
    let arrival = Arrival::new(&env, "who", 0, |_| {});
    let stranger = env.stranger.insecure_clone();
    let ix = arrival.complete(&env, &pubkey_of(&stranger), &env.recipient_out, HONEST, OUT, 0, &[]);
    assert_eq!(custom_code(env.send(&[ix], &stranger, &[])), E::NotCanisterOrRecipient.code());
}

#[test]
fn complete_refuses_every_bad_hook_and_message() {
    let mut env = Env::new();
    let payer = env.canister.insecure_clone();
    let can = canister(&env);

    let mut try_hook = |env: &mut Env, label: &str, change: fn(&mut SolanaEnvelope), want: E| {
        let arrival = Arrival::new(env, label, 0, change);
        let ix = arrival.complete(env, &can, &env.recipient_out.clone(), HONEST, OUT, 0, &[]);
        assert_eq!(custom_code(env.send(&[ix], &payer, &[])), want.code(), "{label}");
    };
    try_hook(&mut env, "deliver", |h| h.action = SOLANA_ACTION_DELIVER, E::BadHook);
    try_hook(&mut env, "usdc-out", |h| h.mint_out = USDC_MINT, E::BadHook);
    try_hook(&mut env, "zero-min", |h| h.min_out = 0, E::BadHook);

    // a hook one byte short
    let arrival = Arrival::new(&env, "short", 0, |_| {});
    let hook = arrival.hook.encode();
    let short = Arrival::with_hook(&env, "short", 0, 0, &hook[..109], arrival.hook);
    let ix = short.complete(&env, &can, &env.recipient_out, HONEST, OUT, 0, &[]);
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::BadMessageLength.code());

    // another shard's caller or vault
    let crossed = Arrival::with_hook(&env, "crossed", 1, 0, &arrival.hook.encode(), arrival.hook);
    let ix = crossed.complete(&env, &can, &env.recipient_out, HONEST, OUT, 0, &[]);
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::NotForThisAuthOrVault.code());

    // a message for another chain
    let nonce = hash("other-chain");
    let (message, attestation) = env.craft(&MessageSpec {
        header_version: 1,
        destination: 6,
        nonce: &nonce,
        caller: &env.a.auth[0].0,
        mint_recipient: &env.a.vault[0],
        amount: AMOUNT,
        fee: FEE,
        hook: &arrival.hook.encode(),
    });
    let wrong = Arrival { shard: 0, nonce, hook: arrival.hook, message, attestation };
    let ix = wrong.complete(&env, &can, &env.recipient_out, HONEST, OUT, 0, &[]);
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::BadHeader.code());

    // a recipient that is one of the program's accounts
    let vault = env.a.vault[1];
    let arrival = Arrival::new(&env, "own-recipient", 0, |h| h.recipient = [0; 32]);
    let _ = arrival;
    let mut hook = Arrival::new(&env, "own-recipient", 0, |_| {}).hook;
    hook.recipient = vault;
    let own = Arrival::with_hook(&env, "own-recipient", 0, 0, &hook.encode(), hook);
    let ix = own.complete(&env, &can, &env.recipient_out, HONEST, OUT, 0, &[]);
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::BadRecipient.code());
}

#[test]
fn complete_stops_at_the_deadline_and_under_a_pause() {
    let mut env = Env::new();
    let payer = env.canister.insecure_clone();
    let arrival = Arrival::new(&env, "deadline", 0, |_| {});
    env.warp(3601);
    let ix = arrival.complete(&env, &canister(&env), &env.recipient_out, HONEST, OUT, 0, &[]);
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::DeadlinePassed.code());

    let arrival = Arrival::new(&env, "paused", 0, |_| {});
    env.set_config(|c| c.paused = true);
    let ix = arrival.complete(&env, &canister(&env), &env.recipient_out, HONEST, OUT, 0, &[]);
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::Paused.code());
}

#[test]
fn a_message_lands_once_and_complete_then_release_cannot_both_pay() {
    let mut env = Env::new();
    let payer = env.canister.insecure_clone();
    let arrival = Arrival::new(&env, "once", 0, |_| {});
    let ix = arrival.complete(&env, &canister(&env), &env.recipient_out, HONEST, OUT, 0, &[]);
    env.send_ok(&[ix], &payer, &[]);
    let ix = arrival.release(&env, &canister(&env), 0, 0, &env.recipient_usdc);
    assert!(env.send(&[ix], &payer, &[]).is_err(), "CCTP's used_nonce stops a second receive");
    assert_eq!(env.balance(&env.recipient_usdc), 0);
}

#[test]
fn a_token_2022_output_is_paid_and_measured() {
    let mut env = Env::new();
    let mint = pubkey_of(&test_key("mint-2022"));
    let output = pubkey_of(&test_key("output-2022"));
    let pool_2022 = pubkey_of(&test_key("pool-2022"));
    let recipient = pubkey_of(&env.recipient);
    let (lamports_mint, lamports_token) = (env.svm.minimum_balance_for_rent_exemption(82), env.svm.minimum_balance_for_rent_exemption(165));
    let owner = solana_address::Address::new_from_array(TOKEN_2022_PROGRAM);
    let put = |env: &mut Env, key: &[u8; 32], data: Vec<u8>, lamports: u64| {
        env.svm
            .set_account(solana_address::Address::new_from_array(*key), solana_account::Account { lamports, data, owner, executable: false, rent_epoch: u64::MAX })
            .unwrap();
    };
    let mut mint_data = vec![0u8; 82];
    mint_data[36..44].copy_from_slice(&u64::MAX.to_le_bytes());
    mint_data[44] = 9;
    mint_data[45] = 1;
    put(&mut env, &mint, mint_data, lamports_mint);
    put(&mut env, &output, token_account_bytes(&mint, &recipient, 0, None), lamports_token);
    let pool = env.pool;
    put(&mut env, &pool_2022, token_account_bytes(&mint, &pool, OUT * 2, None), lamports_token);

    let arrival = Arrival::new(&env, "t22", 0, |h| h.mint_out = mint);
    let (_, token) = env.source();
    let mut route = route_accounts(&env.a.auth[0].0, &env.a.vault[0], &output, &recipient, &env.pool_usdc, &pool_2022, &env.pool, &[]);
    route.push(AccountMeta::readonly(TOKEN_2022_PROGRAM));
    let source = instructions::ArrivalSource { nonce: &arrival.nonce, source_domain: 6, source_token: &token };
    let ix = instructions::complete(&env.a, &canister(&env), 0, &output, source, &route, arrival.complete_data(&route::data(MINTED, OUT, PAY_2022, 0)));
    let payer = env.canister.insecure_clone();
    env.send_ok(&[ix], &payer, &[]);
    assert_eq!(env.balance(&output), OUT);
}

#[test]
fn release_pays_all_the_usdc_to_the_recipient_before_the_deadline_only_for_them_or_the_canister() {
    let mut env = Env::new();
    let payer = env.canister.insecure_clone();
    let stranger = env.stranger.insecure_clone();
    let recipient = env.recipient.insecure_clone();

    let arrival = Arrival::new(&env, "rel-stranger", 0, |_| {});
    let ix = arrival.release(&env, &pubkey_of(&stranger), 0, 0, &env.recipient_usdc);
    assert_eq!(custom_code(env.send(&[ix], &stranger, &[])), E::NotCanisterOrRecipient.code());

    let meta = {
        let ix = arrival.release(&env, &canister(&env), 0, 0, &env.recipient_usdc);
        env.send_ok(&[ix], &payer, &[])
    };
    assert_eq!(env.balance(&env.recipient_usdc), MINTED);
    assert_eq!(env.balance(&env.a.vault[0]), 0);
    let own = own_compute(&meta.logs, &env.a.program);
    assert!(own <= 25_000, "own compute {own}");

    let arrival = Arrival::new(&env, "rel-self", 1, |_| {});
    let ix = arrival.release(&env, &pubkey_of(&recipient), 1, 1, &env.recipient_usdc);
    env.send_ok(&[ix], &recipient, &[]);
    assert_eq!(env.balance(&env.recipient_usdc), MINTED * 2);
}

#[test]
fn after_the_deadline_anyone_may_release_and_a_pause_never_stops_it() {
    let mut env = Env::new();
    let stranger = env.stranger.insecure_clone();
    let arrival = Arrival::new(&env, "rel-late", 0, |_| {});
    env.set_config(|c| c.paused = true);
    env.warp(3601);
    let ix = arrival.release(&env, &pubkey_of(&stranger), 0, 0, &env.recipient_usdc);
    env.send_ok(&[ix], &stranger, &[]);
    assert_eq!(env.balance(&env.recipient_usdc), MINTED);
}

#[test]
fn release_pays_only_the_recipients_own_usdc_account() {
    let mut env = Env::new();
    let payer = env.canister.insecure_clone();
    let arrival = Arrival::new(&env, "rel-where", 0, |_| {});
    let other = pubkey_of(&test_key("other-usdc"));
    let stranger_key = pubkey_of(&env.stranger);
    env.put_token(&other, &USDC_MINT, &stranger_key, 0);
    let ix = arrival.release(&env, &canister(&env), 0, 0, &other);
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::BadOutputAccount.code());
    // not created yet: a create may come first
    let fresh = pubkey_of(&test_key("fresh-recipient"));
    let mut hook = arrival.hook;
    hook.recipient = fresh;
    let arrival = Arrival::with_hook(&env, "rel-fresh", 0, 0, &hook.encode(), hook);
    let ata = pda::ata(&fresh, &USDC_MINT, &TOKEN_PROGRAM);
    let ix = arrival.release(&env, &canister(&env), 0, 0, &ata);
    assert_eq!(custom_code(env.send(&[ix.clone()], &payer, &[])), E::BadOutputAccount.code());
    let create = instructions::create_ata_idempotent(&canister(&env), &fresh, &USDC_MINT, &TOKEN_PROGRAM);
    env.send_ok(&[create, ix], &payer, &[]);
    assert_eq!(env.balance(&ata), MINTED);
}

#[test]
fn a_message_complete_refuses_can_still_be_released() {
    // attack M2: only `auth` can receive it and CCTP has no refund
    let mut env = Env::new();
    let payer = env.canister.insecure_clone();
    let can = canister(&env);
    let good = Arrival::new(&env, "m2", 0, |_| {}).hook;

    // caller and vault from different shards: a readable recipient is paid
    let crossed = Arrival::with_hook(&env, "m2-crossed", 1, 2, &good.encode(), good);
    let ix = crossed.release(&env, &can, 1, 2, &env.recipient_usdc);
    env.send_ok(&[ix], &payer, &[]);
    assert_eq!(env.balance(&env.recipient_usdc), MINTED);
    assert_eq!(env.balance(&env.a.vault[2]), 0);

    // a hook one byte short is still readable
    let short = Arrival::with_hook(&env, "m2-short", 0, 0, &good.encode()[..109], good);
    let ix = short.release(&env, &can, 0, 0, &env.recipient_usdc);
    env.send_ok(&[ix], &payer, &[]);
    assert_eq!(env.balance(&env.recipient_usdc), MINTED * 2);

    // a mint recipient of the wrong kind is not Swapic's: not for this program at all
    let fee = env.a.fee_account;
    // a hook of another version is unreadable: the canister alone, to the fee account
    let mut other = good.encode();
    other[0] = 2;
    let unreadable = Arrival::with_hook(&env, "m2-version", 0, 0, &other, good);
    let stranger = env.stranger.insecure_clone();
    let ix = unreadable.release(&env, &pubkey_of(&stranger), 0, 0, &fee);
    env.warp(10_000);
    assert_eq!(custom_code(env.send(&[ix], &stranger, &[])), E::NotCanisterOrRecipient.code(), "even after any deadline");
    let ix = unreadable.release(&env, &can, 0, 0, &env.recipient_usdc);
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::WrongFeeAccount.code());
    let ix = unreadable.release(&env, &can, 0, 0, &fee);
    let meta = env.send_ok(&[ix], &payer, &[]);
    assert_eq!(env.balance(&fee), MINTED);
    assert!(meta.logs.iter().any(|l| l.contains("Program data:")));

    // an empty hook, and a recipient that is a program address, go the same way
    let empty = Arrival::with_hook(&env, "m2-empty", 0, 0, &[], good);
    let ix = empty.release(&env, &can, 0, 0, &fee);
    env.send_ok(&[ix], &payer, &[]);
    assert_eq!(env.balance(&fee), MINTED * 2);

    let mut own = good;
    own.recipient = env.a.vault[1];
    let own_arrival = Arrival::with_hook(&env, "m2-own", 0, 0, &own.encode(), own);
    let ix = own_arrival.release(&env, &can, 0, 0, &fee);
    env.send_ok(&[ix], &payer, &[]);
    assert_eq!(env.balance(&fee), MINTED * 3);
}

#[test]
fn release_refuses_a_message_that_is_not_swapics() {
    let mut env = Env::new();
    let payer = env.canister.insecure_clone();
    let nonce = hash("not-swapic");
    let hook = Arrival::new(&env, "x", 0, |_| {}).hook.encode();
    let stranger_word = pubkey_of(&env.stranger);
    let (message, attestation) = env.craft(&MessageSpec {
        header_version: 1,
        destination: SOLANA_DOMAIN,
        nonce: &nonce,
        caller: &stranger_word,
        mint_recipient: &env.a.vault[0],
        amount: AMOUNT,
        fee: FEE,
        hook: &hook,
    });
    let mut arrival = Arrival { shard: 0, nonce, hook: Arrival::new(&env, "x", 0, |_| {}).hook, message, attestation };
    let ix = arrival.release(&env, &canister(&env), 0, 0, &env.recipient_usdc);
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::NotForThisAuthOrVault.code());
    // a header version other than 1
    let (message, attestation) = env.craft(&MessageSpec {
        header_version: 2,
        destination: SOLANA_DOMAIN,
        nonce: &nonce,
        caller: &env.a.auth[0].0,
        mint_recipient: &env.a.vault[0],
        amount: AMOUNT,
        fee: FEE,
        hook: &hook,
    });
    arrival.message = message;
    arrival.attestation = attestation;
    let ix = arrival.release(&env, &canister(&env), 0, 0, &env.recipient_usdc);
    assert_eq!(custom_code(env.send(&[ix], &payer, &[])), E::BadHeader.code());
}
