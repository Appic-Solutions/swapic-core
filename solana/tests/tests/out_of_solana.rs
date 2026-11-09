//! `swap_burn` and `burn` against the real CCTP programs (design 4.6, 8a).

mod common;

use common::*;
use swapic_solana_tests::route::*;
use swapic_solana_tests::wire::args::{BurnArgs, Payload, RefSource, TAG_BURN, TAG_SWAP_BURN};
use swapic_solana_tests::wire::cctp::Message;
use swapic_solana_tests::wire::consts::*;
use swapic_solana_tests::wire::envelope::{Action, Envelope};
use swapic_solana_tests::wire::errors::SwapicError as E;
use swapic_solana_tests::wire::instructions;
use swapic_solana_tests::wire::message::{AccountMeta, Instruction};
use swapic_solana_tests::wire::pda::keccak256;
use swapic_solana_tests::*;

const PRODUCED: u64 = 1_000 * ONE_USDC;
const IN: u64 = 5_000 * ONE_USDC;
const NET_FEE: u32 = 50_000;

/// One burn a test can change before it is sent.
struct Burn<'a> {
    float: bool,
    fee: u32,
    min_usdc: u64,
    deadline: u32,
    domain: u8,
    fast: bool,
    event_keypair: bool,
    max_fee_deci_bps: u16,
    recipient: [u8; 20],
    r: [u8; 32],
    payload: Payload<'a>,
    min_gas: u32,
    ref_source: Option<RefSource<'a>>,
}

impl<'a> Burn<'a> {
    fn new(env: &Env, label: &str, shard: usize) -> Self {
        Self {
            float: false,
            fee: NET_FEE,
            min_usdc: PRODUCED,
            deadline: (env.now() + 1800) as u32,
            domain: BASE_DOMAIN,
            fast: true,
            event_keypair: false,
            max_fee_deci_bps: 13,
            recipient: env.evm_recipient,
            r: ref_on(shard, label),
            payload: Payload::Swap {
                token_out: &[0x22; 20],
                min_out: &[0x0f, 0x42, 0x40],
            },
            min_gas: 709_154,
            ref_source: None,
        }
    }

    fn args(&'a self, tag: u8, amount: u64, jupiter: &'a [u8]) -> BurnArgs<'a> {
        BurnArgs {
            tag,
            fast: self.fast,
            event_keypair: self.event_keypair,
            float: self.float,
            domain: self.domain,
            ref_source: self.ref_source.unwrap_or(RefSource::Direct(&self.r)),
            deadline: self.deadline,
            recipient: &self.recipient,
            payload: self.payload,
            min_gas: self.min_gas,
            max_fee_deci_bps: self.max_fee_deci_bps,
            amount,
            fee: self.fee,
            jupiter,
        }
    }

    fn head(&self, env: &Env, event: &[u8; 32]) -> Vec<AccountMeta> {
        let shard = shard_of(&self.r);
        instructions::burn_head(
            &env.a,
            &pubkey_of(&env.user),
            shard,
            self.domain as u32,
            self.float,
            event,
            self.event_keypair,
        )
    }

    fn route(&self, env: &Env, extra: &[AccountMeta]) -> Vec<AccountMeta> {
        let shard = shard_of(&self.r);
        route_accounts(
            &pubkey_of(&env.user),
            &env.user_in,
            &env.a.vault[shard],
            &env.a.vault[shard],
            &env.pool_in,
            &env.pool_usdc,
            &env.pool,
            extra,
        )
    }

    fn swap_burn(&self, env: &Env, mode: u8, produced: u64, extra: &[AccountMeta]) -> Instruction {
        let event = env.a.event(&self.real_ref());
        let jupiter = route::data(IN, produced, mode, produced);
        let data = self.args(TAG_SWAP_BURN, self.min_usdc, &jupiter).encode();
        instructions::swap_burn(
            &env.a,
            self.head(env, &event),
            &self.route(env, extra),
            data,
        )
    }

    fn burn(&self, env: &Env, amount: u64) -> Instruction {
        let event = env.a.event(&self.real_ref());
        let data = self.args(TAG_BURN, amount, &[]).encode();
        instructions::burn(&env.a, self.head(env, &event), &env.user_usdc, data)
    }

    fn real_ref(&self) -> [u8; 32] {
        match self.ref_source {
            Some(RefSource::Memo(memo)) => keccak256(memo),
            _ => self.r,
        }
    }
}

fn message_of(env: &Env, event: &[u8; 32]) -> Vec<u8> {
    let data = env.data(event);
    let len = u32::from_le_bytes(data[48..52].try_into().unwrap()) as usize;
    data[52..52 + len].to_vec()
}

fn user_sends(env: &mut Env, ix: Instruction) -> u32 {
    let user = env.user.insecure_clone();
    custom_code(env.send(&[ix], &user, &[]))
}

#[test]
fn a_user_paid_swap_burn_sends_all_it_produced_and_writes_the_right_message() {
    let mut env = Env::new();
    for shard in 0..4 {
        let burn = Burn::new(&env, &format!("happy-{shard}"), shard);
        let user = env.user.insecure_clone();
        let lamports = env.lamports(&pubkey_of(&user));
        let rent = env.lamports(&env.a.rent.0);
        let fee_before = env.balance(&env.a.fee_account);
        let ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
        let meta = env.send_ok(&[ix], &user, &[]);

        let event = env.a.event(&burn.r);
        let bytes = env.data(&event);
        assert_eq!(bytes.len(), EVENT_ACCOUNT_LEN as usize);
        assert_eq!(
            &bytes[8..40],
            &env.a.rent.0,
            "the event's rent payer is the rent PDA"
        );
        let message = message_of(&env, &event);
        assert_eq!(message.len(), 696, "the length CctpReceiver requires");
        let m = Message::new(&message).unwrap();
        assert_eq!(m.destination_domain(), 6);
        assert_eq!(m.source_domain(), 5);
        assert_eq!(m.mint_recipient(), &word(&CCTP_RECEIVER));
        assert_eq!(m.destination_caller(), &word(&CCTP_RECEIVER));
        assert_eq!(m.message_sender(), &env.a.auth[shard].0);
        assert_eq!(m.burn_token(), &USDC_MINT);
        assert_eq!(
            swapic_solana_tests::wire::cctp::word_to_u64(m.amount_word()),
            Some(PRODUCED - NET_FEE as u64)
        );
        let envelope = Envelope::parse(m.hook()).unwrap();
        assert_eq!(envelope.ref_bytes, burn.r);
        assert_eq!(envelope.action, Action::Swap);
        assert_eq!(envelope.recipient, env.evm_recipient);
        assert_eq!(envelope.token_out, [0x22; 20]);
        assert_eq!(&envelope.min_out[29..], &[0x0f, 0x42, 0x40]);
        assert_eq!(envelope.min_gas, 709_154);

        assert_eq!(env.balance(&env.a.fee_account) - fee_before, NET_FEE as u64);
        assert_eq!(
            env.balance(&env.a.vault[shard]),
            0,
            "nothing rests in the vault"
        );
        // the user paid the event account, and the float did not move
        assert_eq!(env.lamports(&env.a.rent.0), rent);
        let event_rent = env.svm.minimum_balance_for_rent_exemption(748);
        assert_eq!(
            lamports - env.lamports(&pubkey_of(&user)),
            event_rent + 5000
        );
        let own = own_compute(&meta.logs, &env.a.program);
        println!("own compute {own}");
        assert!(own <= 20_000, "own compute {own}");
    }
}

#[test]
fn a_gasless_burn_spends_the_float_and_the_canister_pays_the_fee() {
    let mut env = Env::new();
    let mut burn = Burn::new(&env, "gasless", 1);
    burn.float = true;
    let (user, canister) = (env.user.insecure_clone(), env.canister.insecure_clone());
    let user_lamports = env.lamports(&pubkey_of(&user));
    let rent_before = env.lamports(&env.a.rent.0);
    let ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    env.send_ok(&[ix], &canister, &[&user]);
    assert_eq!(
        env.lamports(&pubkey_of(&user)),
        user_lamports,
        "the user pays no SOL"
    );
    assert_eq!(
        rent_before - env.lamports(&env.a.rent.0),
        env.svm.minimum_balance_for_rent_exemption(748)
    );
}

#[test]
fn the_float_is_spent_only_with_the_canisters_signature() {
    // a user sets the bit and signs alone: an account-level signer flag cannot be forged either
    let mut env = Env::new();
    let mut burn = Burn::new(&env, "float-alone", 0);
    burn.float = true;
    let mut ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    ix.accounts[2].signer = false;
    assert_eq!(user_sends(&mut env, ix), E::FloatWithoutCanister.code());

    // the canister signing without the bit is refused too
    let mut burn = Burn::new(&env, "signed-no-float", 0);
    burn.float = false;
    let mut ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    ix.accounts[2].signer = true;
    let (user, canister) = (env.user.insecure_clone(), env.canister.insecure_clone());
    assert_eq!(
        custom_code(env.send(&[ix], &user, &[&canister])),
        E::FloatWithoutCanister.code()
    );
}

#[test]
fn burn_takes_the_users_usdc_and_a_deliver_mints_to_the_recipient_with_no_caller() {
    let mut env = Env::new();
    let mut burn = Burn::new(&env, "deliver", 2);
    burn.payload = Payload::Deliver;
    burn.min_gas = 0;
    let user = env.user.insecure_clone();
    let before = env.balance(&env.user_usdc);
    let ix = burn.burn(&env, 100 * ONE_USDC);
    env.send_ok(&[ix], &user, &[]);
    assert_eq!(before - env.balance(&env.user_usdc), 100 * ONE_USDC);
    let message = message_of(&env, &env.a.event(&burn.r));
    let m = Message::new(&message).unwrap();
    assert_eq!(m.mint_recipient(), &word(&env.evm_recipient));
    assert_eq!(m.destination_caller(), &[0u8; 32]);
    assert_eq!(Envelope::parse(m.hook()).unwrap().action, Action::Deliver);
}

#[test]
fn a_memo_burn_derives_its_ref_by_keccak_with_the_terms_in_the_instruction() {
    let mut env = Env::new();
    let memo = [0x5au8; 180];
    let mut burn = Burn::new(&env, "memo", 0);
    burn.payload = Payload::Deliver;
    burn.min_gas = 0;
    burn.ref_source = Some(RefSource::Memo(&memo));
    let user = env.user.insecure_clone();
    let ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    env.send_ok(&[ix], &user, &[]);
    let expected = keccak256(&memo);
    let message = message_of(&env, &env.a.event(&expected));
    assert_eq!(
        Envelope::parse(Message::new(&message).unwrap().hook())
            .unwrap()
            .ref_bytes,
        expected
    );
}

#[test]
fn a_ck_deposit_goes_only_to_ethereum_and_carries_its_principal() {
    let mut env = Env::new();
    let principal = [0x77u8; 32];
    let mut burn = Burn::new(&env, "ck-base", 0);
    burn.payload = Payload::CkDeposit {
        principal: &principal,
        subaccount: None,
    };
    burn.min_gas = 1;
    let ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    assert_eq!(user_sends(&mut env, ix), E::DomainNotAllowed.code());

    let mut burn = Burn::new(&env, "ck-eth", 0);
    burn.domain = ETHEREUM_DOMAIN;
    burn.payload = Payload::CkDeposit {
        principal: &principal,
        subaccount: None,
    };
    burn.min_gas = 1;
    let user = env.user.insecure_clone();
    let ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    env.send_ok(&[ix], &user, &[]);
    let message = message_of(&env, &env.a.event(&burn.r));
    assert_eq!(
        Envelope::parse(Message::new(&message).unwrap().hook())
            .unwrap()
            .principal,
        principal
    );
}

#[test]
fn one_live_burn_per_ref() {
    let mut env = Env::new();
    let burn = Burn::new(&env, "twice", 0);
    let user = env.user.insecure_clone();
    let ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    env.send_ok(&[ix], &user, &[]);
    let ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    assert!(
        is_foreign_failure(env.send(&[ix], &user, &[])),
        "the event PDA already exists"
    );
}

#[test]
fn a_non_canonical_event_account_is_refused() {
    let mut env = Env::new();
    let burn = Burn::new(&env, "event", 0);
    let mut ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    ix.accounts[7].pubkey = [9; 32];
    assert_eq!(user_sends(&mut env, ix), E::WrongEvent.code());
}

#[test]
fn the_keypair_fallback_works_only_when_the_config_allows_it() {
    let mut env = Env::new();
    let keypair = test_key("event-keypair");
    let event = pubkey_of(&keypair);
    let mut burn = Burn::new(&env, "fallback", 0);
    burn.event_keypair = true;
    let jupiter = route::data(IN, PRODUCED, HONEST, PRODUCED);
    let data = burn.args(TAG_SWAP_BURN, burn.min_usdc, &jupiter).encode();
    let ix = instructions::swap_burn(
        &env.a,
        burn.head(&env, &event),
        &burn.route(&env, &[]),
        data,
    );
    let user = env.user.insecure_clone();
    assert_eq!(
        custom_code(env.send(&[ix.clone()], &user, &[&keypair])),
        E::EventKeypairNotAllowed.code()
    );
    env.set_config(|c| c.event_keypair_allowed = true);
    env.send_ok(&[ix], &user, &[&keypair]);
    assert_eq!(env.data(&event).len(), EVENT_ACCOUNT_LEN as usize);
}

#[test]
fn every_pinned_account_is_compared_before_any_call() {
    let cases: [(usize, E); 15] = [
        (1, E::WrongConfig),
        (2, E::WrongKey),
        (3, E::WrongAuth),
        (4, E::WrongVault),
        (5, E::WrongFeeAccount),
        (6, E::WrongRent),
        (8, E::WrongProgram),
        (9, E::WrongProgram),
        (10, E::WrongProgram),
        (11, E::WrongProgram),
        (13, E::WrongProgram),
        (14, E::WrongProgram),
        (15, E::WrongProgram),
        (16, E::WrongProgram),
        (17, E::WrongProgram),
    ];
    let mut env = Env::new();
    let burn = Burn::new(&env, "pins", 0);
    for (slot, want) in cases {
        let mut ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
        ix.accounts[slot].pubkey = [slot as u8 + 40; 32];
        assert_eq!(user_sends(&mut env, ix), want.code(), "slot {slot}");
    }
    // a vault of another shard
    let mut ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    ix.accounts[4].pubkey = env.a.vault[1];
    assert_eq!(user_sends(&mut env, ix), E::WrongVault.code());
    // not Jupiter
    let mut ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    ix.accounts[21].pubkey = [50; 32];
    assert_eq!(user_sends(&mut env, ix), E::WrongProgram.code());
}

#[test]
fn the_arguments_only_the_program_can_rule_on_are_refused() {
    let mut env = Env::new();
    let mut go = |change: &dyn Fn(&mut Burn<'_>), want: E, env: &mut Env| {
        let mut burn = Burn::new(env, "args", 0);
        change(&mut burn);
        let ix = burn.swap_burn(env, HONEST, PRODUCED, &[]);
        assert_eq!(user_sends(env, ix), want.code());
    };
    go(&|b| b.domain = 7, E::DomainNotAllowed, &mut env);
    go(&|b| b.deadline -= 1400, E::DeadlineOutOfWindow, &mut env);
    go(&|b| b.deadline += 7000, E::DeadlineOutOfWindow, &mut env);
    go(&|b| b.recipient = CCTP_RECEIVER, E::BadRecipient, &mut env);
    go(
        &|b| {
            b.recipient = [
                0xB3, 0xf6, 0x65, 0x17, 0xbE, 0xC4, 0xcC, 0x5A, 0x8E, 0x79, 0x9d, 0xB2, 0xdD, 0xA4,
                0xC8, 0x10, 0xBe, 0xfC, 0xc9, 0x17,
            ]
        },
        E::BadRecipient,
        &mut env,
    );
    go(
        &|b| {
            b.recipient = [
                0x34, 0xD8, 0xA2, 0xde, 0xbE, 0x49, 0xDC, 0xCa, 0x0f, 0x7c, 0xC7, 0x7d, 0x74, 0x16,
                0x3c, 0xFB, 0x94, 0xCc, 0x19, 0xF5,
            ]
        },
        E::BadRecipient,
        &mut env,
    );
    go(&|b| b.min_usdc = PRODUCED + 1, E::BelowMinUsdc, &mut env);
    go(
        &|b| b.fee = PRODUCED as u32 + 1,
        E::FeeAboveProduced,
        &mut env,
    );
    // what is left after the fee is under the least burn
    go(
        &|b| b.fee = (PRODUCED - ONE_USDC + 1) as u32,
        E::BelowMinBurn,
        &mut env,
    );
    // CCTP needs amount > maxFee: a one-unit burn rounds its max fee up to all of it
    env.set_config(|c| c.min_burn = 1);
    go(
        &|b| {
            b.fee = (PRODUCED - 1) as u32;
            b.max_fee_deci_bps = 1;
        },
        E::MaxFeeTooHigh,
        &mut env,
    );
}

#[test]
fn a_route_that_produces_nothing_or_too_little_moves_nothing() {
    let mut env = Env::new();
    let burn = Burn::new(&env, "little", 0);
    let ix = burn.swap_burn(&env, NOTHING, 0, &[]);
    assert_eq!(user_sends(&mut env, ix), E::NothingProduced.code());
    let ix = burn.swap_burn(&env, HONEST, PRODUCED - 1, &[]);
    assert_eq!(user_sends(&mut env, ix), E::BelowMinUsdc.code());
    assert_eq!(env.balance(&env.user_in), 1_000_000 * ONE_USDC);
}

#[test]
fn a_route_may_not_name_swapics_accounts_on_the_way_out_and_never_an_auth() {
    let mut env = Env::new();
    let burn = Burn::new(&env, "names", 0);
    let event = env.a.event(&burn.r);
    let a = env.a.clone();
    let forbidden = [
        a.config.0,
        a.rent.0,
        a.canister,
        a.admin,
        a.program,
        MT,
        TMM,
        event,
        a.auth[0].0,
        a.auth[1].0,
        a.vault[1],
    ];
    for key in forbidden {
        let ix = burn.swap_burn(&env, HONEST, PRODUCED, &[AccountMeta::readonly(key)]);
        assert_eq!(user_sends(&mut env, ix), E::ForbiddenRouteAccount.code());
    }
}

#[test]
fn the_fee_payers_signature_never_reaches_a_route() {
    // the stand-in asks for the canister's signature through a hostile mode: it never has it, because the
    // program sets the signer flag for the user alone and refuses the canister in the route list
    let mut env = Env::new();
    let mut burn = Burn::new(&env, "payer", 0);
    burn.float = true;
    let ix = burn.swap_burn(
        &env,
        HONEST,
        PRODUCED,
        &[AccountMeta::readonly(pubkey_of(&env.canister))],
    );
    let (user, canister) = (env.user.insecure_clone(), env.canister.insecure_clone());
    assert_eq!(
        custom_code(env.send(&[ix], &canister, &[&user])),
        E::ForbiddenRouteAccount.code()
    );
}

#[test]
fn only_jupiters_two_routes_reach_the_router_on_the_way_out() {
    let mut env = Env::new();
    let burn = Burn::new(&env, "disc", 0);
    let event = env.a.event(&burn.r);
    let mut jupiter = route::data(IN, PRODUCED, HONEST, 0);
    jupiter[..8].copy_from_slice(&[229, 23, 203, 151, 122, 227, 173, 42]);
    let data = burn.args(TAG_SWAP_BURN, burn.min_usdc, &jupiter).encode();
    let ix = instructions::swap_burn(
        &env.a,
        burn.head(&env, &event),
        &burn.route(&env, &[]),
        data,
    );
    assert_eq!(
        user_sends(&mut env, ix),
        E::JupiterInstructionNotAllowed.code()
    );
}

#[test]
fn a_nested_call_into_the_program_is_refused() {
    // a route AMM re-entering with the user's privilege: the stand-in calls Swapic from inside itself
    let mut env = Env::new();
    let burn = Burn::new(&env, "nested", 0);
    let inner = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    let mut data = route::data(0, 0, REENTER, 0);
    data.extend_from_slice(&inner.data);
    let mut accounts = vec![AccountMeta::readonly(env.a.program); 12];
    accounts.push(AccountMeta::readonly(env.pool));
    accounts.push(AccountMeta::readonly(env.a.program));
    accounts.extend_from_slice(&inner.accounts);
    let ix = Instruction {
        program: JUPITER,
        accounts,
        data,
    };
    assert_eq!(user_sends(&mut env, ix), E::NotTopLevel.code());
}

#[test]
fn a_pause_stops_burns() {
    let mut env = Env::new();
    env.set_config(|c| c.paused = true);
    let burn = Burn::new(&env, "paused", 0);
    let ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    assert_eq!(user_sends(&mut env, ix), E::Paused.code());
}

#[test]
fn a_donation_to_a_vault_is_never_burned() {
    let mut env = Env::new();
    let (vault, auth) = (env.a.vault[0], env.a.auth[0].0);
    env.put_token(&vault, &USDC_MINT, &auth, 12_345);
    let burn = Burn::new(&env, "donated", 0);
    let user = env.user.insecure_clone();
    let ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    env.send_ok(&[ix], &user, &[]);
    assert_eq!(env.balance(&vault), 12_345);
    let m = message_of(&env, &env.a.event(&burn.r));
    let amount =
        swapic_solana_tests::wire::cctp::word_to_u64(Message::new(&m).unwrap().amount_word());
    assert_eq!(amount, Some(PRODUCED - NET_FEE as u64));
}

#[test]
fn a_user_paid_burn_cannot_draw_on_the_float() {
    // attack L6: the program asks one byte less than CCTP takes
    let mut env = Env::with_program("swapic_solana_program_short_rent");
    let burn = Burn::new(&env, "short-rent", 0);
    let ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    assert_eq!(user_sends(&mut env, ix), E::FloatDrawn.code());
}

#[test]
fn reclaim_sends_the_rent_to_the_canister_after_five_days_only() {
    let mut env = Env::new();
    let burn = Burn::new(&env, "reclaim", 0);
    let user = env.user.insecure_clone();
    let ix = burn.swap_burn(&env, HONEST, PRODUCED, &[]);
    env.send_ok(&[ix], &user, &[]);
    let event = env.a.event(&burn.r);
    let rent_value = env.lamports(&event);

    // the message Iris attests: the source message with its nonce filled in
    let mut destination = message_of(&env, &event);
    destination[12..44].copy_from_slice(&hash("assigned-nonce"));
    let attestation = env.attest(&destination);
    let stranger = env.stranger.insecure_clone();
    let ix = instructions::reclaim(
        &env.a,
        &pubkey_of(&stranger),
        &event,
        &attestation,
        &destination,
    );

    assert!(
        is_foreign_failure(env.send(&[ix.clone()], &stranger, &[])),
        "CCTP's five-day window"
    );
    env.warp(5 * 24 * 3600 + 1);
    let canister_key = pubkey_of(&env.canister);
    let (canister_before, float_before) =
        (env.lamports(&canister_key), env.lamports(&env.a.rent.0));
    env.send_ok(&[ix], &stranger, &[]);
    assert_eq!(
        env.lamports(&canister_key) - canister_before,
        rent_value,
        "the reclaimed lamports reach the payer key"
    );
    assert_eq!(
        env.lamports(&env.a.rent.0),
        float_before,
        "the float is untouched"
    );
    assert!(!env.exists(&event));
}

#[test]
fn reclaim_cannot_be_pointed_elsewhere() {
    let mut env = Env::new();
    let stranger = env.stranger.insecure_clone();
    let ix = instructions::reclaim(
        &env.a,
        &pubkey_of(&stranger),
        &[9; 32],
        &[1; 130],
        &[2; 376],
    );
    for (slot, want) in [
        (1usize, E::WrongRent),
        (2, E::WrongProgram),
        (4, E::WrongProgram),
        (5, E::WrongKey),
        (6, E::WrongProgram),
    ] {
        let mut bad = ix.clone();
        bad.accounts[slot].pubkey = pubkey_of(&stranger);
        if slot == 1 || slot == 2 || slot == 5 {
            bad.accounts[slot].writable = true;
        }
        assert_eq!(
            custom_code(env.send(&[bad], &stranger, &[])),
            want.code(),
            "slot {slot}"
        );
    }
}
