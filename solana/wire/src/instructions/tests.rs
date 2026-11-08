use super::*;
use crate::message::{compile_v0, LookupTable};

fn addresses() -> SwapicAddresses {
    SwapicAddresses::derive([1; 32], [2; 32], [3; 32])
}

#[test]
fn burn_head_has_the_design_s_21_fixed_accounts_in_order() {
    let a = addresses();
    let head = burn_head(&a, &[9; 32], 2, 6, true, &[8; 32], false);
    assert_eq!(head.len(), 21);
    assert_eq!(head[0].pubkey, [9; 32]);
    assert!(head[0].signer && !head[0].writable, "the float pays, so the user is not written");
    assert_eq!(head[1].pubkey, a.config.0);
    assert_eq!(head[2].pubkey, a.canister);
    assert!(head[2].signer);
    assert_eq!(head[3].pubkey, a.auth[2].0);
    assert_eq!(head[4].pubkey, a.vault[2]);
    assert_eq!(head[5].pubkey, a.fee_account);
    assert_eq!(head[6].pubkey, a.rent.0);
    assert_eq!(head[7].pubkey, [8; 32]);
    assert_eq!(head[9].pubkey, a.denylist[2]);
    assert_eq!(head[12].pubkey, pda::remote_token_messenger(6));
    assert_eq!(head[15].pubkey, USDC_MINT);
    assert_eq!(head[16].pubkey, MT);
    assert_eq!(head[17].pubkey, TMM);
    assert_eq!(head[20].pubkey, TMM_EVENT_AUTHORITY);

    let user_paid = burn_head(&a, &[9; 32], 0, 6, false, &[8; 32], false);
    assert!(user_paid[0].writable, "the user pays the rent");
    assert!(!user_paid[2].signer, "the canister signs only a float burn");
}

#[test]
fn complete_and_release_share_their_head() {
    let a = addresses();
    let source = ArrivalSource { nonce: &[4; 32], source_domain: 6, source_token: &[5; 32] };
    let complete = complete(&a, &a.canister, 1, &[7; 32], source, &[AccountMeta::writable([6; 32])], vec![3]);
    assert_eq!(complete.accounts.len(), 22 + 1 + 1);
    assert_eq!(complete.accounts[3].pubkey, a.vault[1]);
    assert_eq!(complete.accounts[5].pubkey, a.wsol[1]);
    assert_eq!(complete.accounts[6].pubkey, MT_AUTHORITY_TMM);
    assert_eq!(complete.accounts[8].pubkey, pda::used_nonce(&[4; 32]));
    assert_eq!(complete.accounts[21].pubkey, TMM_EVENT_AUTHORITY);
    assert_eq!(complete.accounts[22].pubkey, JUPITER);

    let release = release(&a, &[9; 32], 1, 2, &[7; 32], source, vec![4]);
    assert_eq!(release.accounts.len(), 22);
    assert_eq!(release.accounts[2].pubkey, a.auth[1].0);
    assert_eq!(release.accounts[3].pubkey, a.vault[2]);
    assert_eq!(release.accounts[5].pubkey, a.auth[2].0);
}

#[test]
fn the_relay_receive_has_circle_s_account_order() {
    let source = ArrivalSource { nonce: &[4; 32], source_domain: 6, source_token: &[5; 32] };
    let ix = receive_message(&[2; 32], &[7; 32], source, &[1; 486], &[2; 130]);
    assert_eq!(ix.program, MT);
    let keys: Vec<Pubkey> = ix.accounts.iter().map(|m| m.pubkey).collect();
    assert_eq!(keys.len(), 20);
    assert_eq!(keys[0], [2; 32]);
    assert_eq!(keys[1], [2; 32]);
    assert_eq!(keys[2], MT_AUTHORITY_TMM);
    assert_eq!(keys[3], MESSAGE_TRANSMITTER);
    assert_eq!(keys[5], TMM);
    assert_eq!(keys[8], MT);
    assert_eq!(keys[9], TOKEN_MESSENGER);
    assert_eq!(keys[14], FEE_RECIPIENT_TOKEN_ACCOUNT);
    assert_eq!(keys[15], [7; 32]);
    assert_eq!(keys[16], CUSTODY);
    assert_eq!(keys[17], TOKEN_PROGRAM);
    assert_eq!(keys[18], TMM_EVENT_AUTHORITY);
    assert_eq!(keys[19], TMM);
}

#[test]
fn a_gasless_burn_compiles_with_the_payer_first_and_the_user_second() {
    let a = addresses();
    let event = a.event(&[5; 32]);
    let head = burn_head(&a, &[9; 32], 0, 6, true, &event, false);
    let ix = burn(&a, head, &[10; 32], vec![2]);
    let table = LookupTable {
        address: [11; 32],
        entries: vec![a.config.0, a.auth[0].0, a.vault[0], a.fee_account, a.rent.0, SENDER_AUTHORITY, MT, TMM, USDC_MINT],
    };
    let message = compile_v0(&a.canister, &[compute_unit_limit(200_000), compute_unit_price(1000), ix], &[table], &[7; 32]).unwrap();
    assert_eq!(message.header.required_signatures, 2);
    assert_eq!(message.static_keys[0], a.canister);
    assert_eq!(message.static_keys[1], [9; 32]);
    // the program ids stay static, a table cannot name them
    assert!(message.static_keys.contains(&a.program));
    assert!(message.static_keys.contains(&COMPUTE_BUDGET_PROGRAM));
}

#[test]
fn the_ata_create_is_idempotent_and_derives_the_account() {
    let ix = create_ata_idempotent(&[1; 32], &[2; 32], &USDC_MINT, &TOKEN_PROGRAM);
    assert_eq!(ix.data, vec![1]);
    assert_eq!(ix.accounts[1].pubkey, pda::ata(&[2; 32], &USDC_MINT, &TOKEN_PROGRAM));
}

#[test]
fn the_loader_upgrade_names_program_data_and_is_tag_3() {
    let ix = loader_upgrade(&[1; 32], &[2; 32], &[3; 32], &[4; 32]);
    assert_eq!(ix.data, vec![3, 0, 0, 0]);
    assert_eq!(ix.accounts[0].pubkey, pda::find_program_address(&[&[1u8; 32][..]], &UPGRADEABLE_LOADER).0);
    assert!(ix.accounts[6].signer);
}

#[test]
fn compute_budget_instructions_have_their_tags() {
    assert_eq!(compute_unit_limit(5).data, vec![2, 5, 0, 0, 0]);
    assert_eq!(compute_unit_price(5).data, vec![3, 5, 0, 0, 0, 0, 0, 0, 0]);
}
