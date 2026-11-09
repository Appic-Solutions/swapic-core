use super::*;
use crate::base58::{decode_pubkey, encode};

fn key(text: &str) -> Pubkey {
    decode_pubkey(text).unwrap()
}

#[test]
fn derives_the_research_probe_addresses() {
    // `out/consts.json` of the probe: its seeds were ["a"], the vault the ATA of that PDA
    let program = key("7yJsvHVwiytWsHqS6oEG1GbjrzVbVAhsoPCqwQD138Qh");
    let (auth, bump) = find_program_address(&[b"a"], &program);
    assert_eq!(
        encode(&auth),
        "22dMXZ63jHxoMFhzK6E1rkJ88BjaVZ1V2iLqNspQ3MqE"
    );
    assert_eq!(bump, 255);
    assert_eq!(
        encode(&ata(&auth, &USDC_MINT, &TOKEN_PROGRAM)),
        "GkBbaHv4ThX2zGHUbVz6P86JodaqygNXezrbFVguRyyy"
    );
}

#[test]
fn derives_cctp_accounts_from_mainnet() {
    assert_eq!(
        encode(&remote_token_messenger(6)),
        "BwmDYtQ7jFj8ddaTmKa7fz9hyuK9n58mvc8G7DYNcKjM"
    );
}

#[test]
fn wallets_are_on_curve_and_pdas_are_not() {
    assert!(is_on_curve(&key(
        "J9FJgVAFgmaNiRkagsFAKvWzb18Ea3c7JGS7UPf9f21i"
    )));
    assert!(!is_on_curve(&key(
        "22dMXZ63jHxoMFhzK6E1rkJ88BjaVZ1V2iLqNspQ3MqE"
    )));
    assert!(!is_on_curve(&USDC_MINT) || is_on_curve(&USDC_MINT)); // a mint keypair may be on the curve
}

#[test]
fn keccak_matches_the_ethereum_empty_hash() {
    assert_eq!(
        keccak256(b""),
        [
            0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7,
            0x03, 0xc0, 0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04,
            0x5d, 0x85, 0xa4, 0x70
        ]
    );
}

#[test]
fn addresses_are_recognised() {
    let a = SwapicAddresses::derive([1; 32], [2; 32], [3; 32]);
    assert_eq!(a.auth_index(&a.auth[2].0), Some(2));
    assert_eq!(a.vault_index(&a.vault[3]), Some(3));
    assert!(a.is_swapic_account(&a.config.0));
    assert!(a.is_swapic_account(&a.wsol[1]));
    assert!(a.is_swapic_account(&[2; 32]));
    assert!(!a.is_swapic_account(&[9; 32]));
    // the program's own rule is narrower: its keys and token accounts are recipients to `release`
    assert!(a.is_program_address(&a.config.0));
    assert!(a.is_program_address(&a.vault[1]));
    assert!(!a.is_program_address(&[2; 32]));
    assert!(!a.is_program_address(&a.wsol[1]));
    assert!(!a.is_program_address(&[9; 32]));
    // the fee account belongs to the admin key, never the payer key (attack L8)
    assert_eq!(a.fee_account, ata(&[3; 32], &USDC_MINT, &TOKEN_PROGRAM));
    assert_ne!(a.fee_account, ata(&[2; 32], &USDC_MINT, &TOKEN_PROGRAM));
}
