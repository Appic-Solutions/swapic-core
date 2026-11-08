//! `set_config` (design 4.10, attack L7).

mod common;

use swapic_solana_tests::wire::args::{encode_set_config, Config};
use swapic_solana_tests::wire::errors::SwapicError as E;
use swapic_solana_tests::wire::instructions;
use swapic_solana_tests::wire::message::Instruction;
use swapic_solana_tests::*;

#[test]
fn the_first_set_config_works_on_an_address_that_already_holds_a_donation() {
    let mut env = Env::new();
    // a stranger donated before the deploy session reached it: start over from a bare address
    let key = solana_address::Address::new_from_array(env.a.config.0);
    env.svm
        .set_account(key, solana_account::Account { lamports: 1, data: vec![], owner: solana_address::Address::new_from_array([0; 32]), executable: false, rent_epoch: u64::MAX })
        .unwrap();
    let admin = env.admin.insecure_clone();
    let config = default_config();
    let ix = instructions::set_config(&env.a, &config);
    env.send_ok(&[ix], &admin, &[]);
    let stored = Config::parse(&env.data(&env.a.config.0)).unwrap();
    assert_eq!(stored.swap_domains, config.swap_domains);
    assert_eq!(stored.bump, env.a.config.1, "the program sets the bump");
    assert_eq!(env.lamports(&env.a.config.0), env.svm.minimum_balance_for_rent_exemption(40));
}

#[test]
fn a_later_set_config_updates_in_place_and_refuses_a_non_admin() {
    let mut env = Env::new();
    env.set_config(|c| c.min_burn = 42);
    assert_eq!(Config::parse(&env.data(&env.a.config.0)).unwrap().min_burn, 42);

    // the payer key is not the admin key: a co-signed set_config can never pass
    let canister = env.canister.insecure_clone();
    let mut ix = instructions::set_config(&env.a, &default_config());
    ix.accounts[0].pubkey = pubkey_of(&canister);
    assert_eq!(custom_code(env.send(&[ix], &canister, &[])), E::WrongKey.code());
}

#[test]
fn the_admin_must_sign() {
    let mut env = Env::new();
    let canister = env.canister.insecure_clone();
    let mut ix = instructions::set_config(&env.a, &default_config());
    ix.accounts[0].signer = false;
    ix.accounts[0].writable = false;
    assert_eq!(custom_code(env.send(&[ix], &canister, &[])), E::WrongKey.code());
}

#[test]
fn set_config_refuses_a_bad_window_a_ck_domain_off_ethereum_and_a_short_body() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    let send = |env: &mut Env, config: &Config| {
        let ix = instructions::set_config(&env.a, config);
        custom_code(env.send(&[ix], &admin, &[]))
    };
    let mut bad = default_config();
    bad.min_deadline_s = bad.max_deadline_s;
    assert_eq!(send(&mut env, &bad), E::DeadlineOutOfWindow.code());
    let mut bad = default_config();
    bad.ck_domains = 1 << 6;
    assert_eq!(send(&mut env, &bad), E::DomainNotAllowed.code());

    let mut ix = instructions::set_config(&env.a, &default_config());
    ix.data = encode_set_config(&default_config())[..39].to_vec();
    assert_eq!(custom_code(env.send(&[ix], &admin, &[])), E::ShortData.code());
}

#[test]
fn an_unknown_tag_is_refused() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    for data in [vec![], vec![7u8], vec![0u8]] {
        let ix = Instruction { program: env.a.program, accounts: vec![], data };
        assert_eq!(custom_code(env.send(&[ix], &admin, &[])), E::BadAction.code());
    }
}
