use super::*;

#[test]
fn a_step_round_trips_and_clears_its_bytes() {
    let mut step = SolanaStep::Signed {
        purpose: SolanaPurpose::Complete { arrival: [1; 32] },
        version: SolanaTxVersion::V1,
        signature: [2; 64],
        raw: vec![3; 10],
        last_valid_block_height: 7,
    };
    assert_eq!(SolanaStep::from_bytes(&step.to_bytes()), Some(step.clone()));
    assert_eq!(step.signed().unwrap().1, &[3; 10]);
    step.clear_raw();
    assert!(step.signed().unwrap().1.is_empty());
}

#[test]
fn a_swap_arrival_always_pays_the_dearer_end() {
    let fees = SolanaFees {
        gasless_burn: TokenAmount::new(1),
        complete_new_account: TokenAmount::new(5),
        release_new_account: TokenAmount::new(9),
        forward_relay: TokenAmount::new(2),
        reclaim: TokenAmount::new(3),
    };
    assert_eq!(fees.arrival(), TokenAmount::new(9));
    assert_eq!(fees.get(SolanaAction::ForwardRelay), TokenAmount::new(2));
}

#[test]
fn every_key_fits_its_bound_and_keys_of_two_kinds_differ() {
    let longest = SolanaKey::cosign_by_signature(&[0xff; 64]);
    assert_eq!(longest.0.len(), 65);
    assert_ne!(
        SolanaKey::complete_attempts(&[1; 32]),
        SolanaKey::release_attempts(&[1; 32])
    );
}
