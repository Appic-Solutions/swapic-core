use super::*;
use crate::solana::config_for_tests as solana_config;

fn addresses() -> SwapicAddresses {
    SwapicAddresses::derive([1; 32], [2; 32], [3; 32])
}

const NOW: u64 = 1_800_000_000;

/// A real wallet from the research: on the curve.
fn wallet() -> [u8; 32] {
    base58::decode_pubkey("J9FJgVAFgmaNiRkagsFAKvWzb18Ea3c7JGS7UPf9f21i").unwrap()
}

fn hook() -> SolanaEnvelope {
    SolanaEnvelope {
        ref_bytes: [5; 32],
        action: SOLANA_ACTION_SWAP,
        deadline: (NOW + 3600) as u32,
        recipient: wallet(),
        mint_out: [0x77; 32],
        min_out: 100,
    }
}

fn read_swap(a: &SwapicAddresses, hook: &SolanaEnvelope, caller: usize, vault: usize) -> Result<SolanaBurn, &'static str> {
    let bytes = hook.encode();
    read_burn(
        &BurnFields { source_domain: 6, mint_recipient: &a.vault[vault], destination_caller: &a.auth[caller].0, hook: &bytes, max_fee: 1 },
        a,
        &solana_config(),
        None,
        NOW,
    )
}

#[test]
fn a_good_swap_arrival_reads() {
    let a = addresses();
    assert_eq!(read_swap(&a, &hook(), 2, 2), Ok(SolanaBurn::Swap(hook())));
}

#[test]
fn a_swap_arrival_is_held_to_the_programs_rules() {
    let a = addresses();
    assert!(read_swap(&a, &hook(), 1, 2).unwrap_err().contains("its vault's own auth"));
    let mut h = hook();
    h.action = 0;
    assert!(read_swap(&a, &h, 0, 0).unwrap_err().contains("not a swap"));
    let mut h = hook();
    h.recipient = a.vault[1];
    assert!(read_swap(&a, &h, 0, 0).unwrap_err().contains("not a wallet"));
    let mut h = hook();
    h.recipient = a.canister;
    assert!(read_swap(&a, &h, 0, 0).is_err());
    let mut h = hook();
    h.mint_out = USDC_MINT;
    assert!(read_swap(&a, &h, 0, 0).unwrap_err().contains("Forwarder"));
    let mut h = hook();
    h.min_out = 0;
    assert!(read_swap(&a, &h, 0, 0).unwrap_err().contains("minimum"));
    let mut h = hook();
    h.deadline = (NOW + 59) as u32;
    assert!(read_swap(&a, &h, 0, 0).unwrap_err().contains("deadline"));
}

fn forward(a: &SwapicAddresses, source_domain: u32, mint_recipient: &[u8; 32], hook: ForwarderHook, read: Option<[u8; 32]>, max_fee: u128) -> Result<SolanaBurn, &'static str> {
    let mut out = [0u8; 65];
    let n = hook.write(&mut out);
    read_burn(
        &BurnFields { source_domain, mint_recipient, destination_caller: &[0; 32], hook: &out[..n], max_fee },
        a,
        &solana_config(),
        read,
        NOW,
    )
}

#[test]
fn a_forwarder_burn_mints_only_to_its_owners_usdc_account() {
    // attack L3: both forms
    let a = addresses();
    let owner = wallet();
    let ata = pda::ata(&owner, &USDC_MINT, &TOKEN_PROGRAM);
    assert_eq!(forward(&a, 6, &ata, ForwarderHook::CreateFor(owner), None, 1), Ok(SolanaBurn::Forwarder { owner }));
    assert_eq!(forward(&a, 6, &ata, ForwarderHook::Existing, Some(owner), 1), Ok(SolanaBurn::Forwarder { owner }));
    assert!(forward(&a, 6, &[0x12; 32], ForwarderHook::CreateFor(owner), None, 1).unwrap_err().contains("USDC account"));
    assert!(forward(&a, 6, &[0x12; 32], ForwarderHook::Existing, Some(owner), 1).unwrap_err().contains("USDC account"));
    // a 32-byte form whose owner was not read cannot pass
    assert!(forward(&a, 6, &ata, ForwarderHook::Existing, None, 1).unwrap_err().contains("not read"));
}

#[test]
fn a_forwarder_burn_is_refused_from_plasma_and_above_the_margin() {
    let a = addresses();
    let owner = wallet();
    let ata = pda::ata(&owner, &USDC_MINT, &TOKEN_PROGRAM);
    assert!(forward(&a, PLASMA_DOMAIN, &ata, ForwarderHook::CreateFor(owner), None, 1).unwrap_err().contains("Plasma"));
    assert!(forward(&a, 6, &ata, ForwarderHook::CreateFor(owner), None, 500_001).unwrap_err().contains("margin"));
}

fn route(data: Vec<u8>, keys: &[[u8; 32]]) -> SolanaRoute {
    SolanaRoute {
        data,
        accounts: keys
            .iter()
            .map(|k| settlement_api::types::solana::SolanaRouteAccount { address: base58::encode(k), writable: true })
            .collect(),
    }
}

fn route_data() -> Vec<u8> {
    let mut d = jupiter::ROUTE_V2.to_vec();
    d.extend_from_slice(&[0; 30]);
    d
}

#[test]
fn a_route_is_held_to_rule_seven() {
    let a = addresses();
    let metas = read_route(&route(route_data(), &[[8; 32]]), &a, 0).unwrap();
    assert!(!metas[0].signer, "the program sets the one signer");

    assert!(read_route(&route(vec![1, 2, 3], &[]), &a, 0).unwrap_err().contains("two route"));
    let mut long = route_data();
    long.resize(1025, 0);
    assert!(read_route(&route(long, &[]), &a, 0).unwrap_err().contains("1,024"));
    for key in [a.canister, a.admin, a.config.0, a.rent.0, a.program, MT, TMM] {
        assert!(read_route(&route(route_data(), &[key]), &a, 0).is_err());
    }
    let many: Vec<[u8; 32]> = (0..42).map(|i| [i as u8 + 100; 32]).collect();
    assert!(read_route(&route(route_data(), &many), &a, 0).unwrap_err().contains("above 41"));
    assert!(read_route(&route(route_data(), &many[..38]), &a, 1).is_ok());
    assert!(read_route(&route(route_data(), &many[..39]), &a, 1).is_err());
}

fn build(a: &SwapicAddresses, h: &SolanaEnvelope, create: bool, token_2022: bool) -> Vec<Instruction> {
    let r = route(route_data(), &[[8; 32]]);
    let metas = read_route(&r, a, 1).unwrap();
    build_complete(a, h, 1, &[4; 32], 6, &[3; 32], &[7; 486], &[9; 130], &r, &metas, create, token_2022)
}

#[test]
fn a_complete_pays_the_recipients_own_account_and_creates_it_when_asked() {
    let a = addresses();
    let h = hook();
    let all = build(&a, &h, true, false);
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].program, ATA_PROGRAM);
    let output = pda::ata(&h.recipient, &h.mint_out, &TOKEN_PROGRAM);
    assert_eq!(all[1].accounts[4].pubkey, output);
    assert_eq!(all[1].accounts[0].pubkey, a.canister);
    assert_eq!(all[1].accounts[2].pubkey, a.auth[1].0);
    assert_eq!(all[1].accounts[3].pubkey, a.vault[1]);
    assert_eq!(build(&a, &h, false, false).len(), 1);
    // a Token-2022 token derives under its own program
    let all = build(&a, &h, false, true);
    assert_eq!(all[0].accounts[4].pubkey, pda::ata(&h.recipient, &h.mint_out, &TOKEN_2022_PROGRAM));
}

#[test]
fn a_native_arrival_recreates_wsol_and_pays_the_wallet() {
    let a = addresses();
    let mut h = hook();
    h.mint_out = WSOL_MINT;
    let all = build(&a, &h, false, false);
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].accounts[1].pubkey, a.wsol[1], "CreateIdempotent(wsol[i])");
    assert_eq!(all[1].accounts[4].pubkey, h.recipient);
}

#[test]
fn a_release_pays_the_recipients_usdc_account_or_the_fee_account() {
    let a = addresses();
    let owner = wallet();
    let all = build_release(&a, 1, 2, Some(&owner), &[4; 32], 6, &[3; 32], &[7; 486], &[9; 130], true);
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].accounts[4].pubkey, pda::ata(&owner, &USDC_MINT, &TOKEN_PROGRAM));
    assert_eq!(all[1].accounts[2].pubkey, a.auth[1].0);
    assert_eq!(all[1].accounts[3].pubkey, a.vault[2]);
    assert_eq!(all[1].accounts[5].pubkey, a.auth[2].0);
    let all = build_release(&a, 1, 1, None, &[4; 32], 6, &[3; 32], &[7; 486], &[9; 130], false);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].accounts[4].pubkey, a.fee_account);
}
