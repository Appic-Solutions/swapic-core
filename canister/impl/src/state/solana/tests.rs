use super::*;
use crate::state::MemoryStore;
use types::solana::{SolanaProposal, SolanaTxVersion};

fn state() -> State<MemoryStore> {
    State::new(MemoryStore::default())
}

fn at(seconds: u64) -> Timestamp {
    Timestamp::from_nanos(seconds * 1_000_000_000)
}

fn signed(purpose: SolanaPurpose) -> SolanaStep {
    SolanaStep::Signed {
        purpose,
        version: SolanaTxVersion::V1,
        signature: [7; 64],
        raw: vec![1],
        last_valid_block_height: 9,
    }
}

fn apply(state: &mut State<MemoryStore>, step: &SolanaStep, seconds: u64) {
    state.admits_solana(step).unwrap();
    state.record_solana(step, EventIndex::new(1), at(seconds));
}

fn proposal(id: u64) -> SolanaProposal {
    SolanaProposal {
        id,
        buffer: [1; 32],
        build_hash: [2; 32],
        source_commit: "abc".to_string(),
        proposed_at_s: 0,
        not_before_s: 100,
        status: SolanaProposalStatus::Waiting,
    }
}

#[test]
fn attempts_count_per_arrival_and_kind() {
    let mut s = state();
    apply(
        &mut s,
        &signed(SolanaPurpose::Complete { arrival: [1; 32] }),
        10,
    );
    apply(
        &mut s,
        &signed(SolanaPurpose::Complete { arrival: [1; 32] }),
        10,
    );
    apply(
        &mut s,
        &signed(SolanaPurpose::Release { arrival: [1; 32] }),
        10,
    );
    assert_eq!(s.solana_complete_attempts(&[1; 32]), 2);
    assert_eq!(s.solana_release_attempts(&[1; 32]), 1);
    assert_eq!(s.solana_complete_attempts(&[2; 32]), 0);
}

#[test]
fn an_arrival_finished_stays_finished() {
    let mut s = state();
    assert!(!s.solana_arrival_done(&[1; 32]));
    apply(
        &mut s,
        &SolanaStep::ArrivalFinished { arrival: [1; 32] },
        10,
    );
    assert!(s.solana_arrival_done(&[1; 32]));
}

#[test]
fn one_finish_per_ref() {
    let mut s = state();
    let first = SolanaStep::RefFinished {
        r#ref: [1; 32],
        arrival: [2; 32],
        burned_at_s: 5,
    };
    apply(&mut s, &first, 10);
    assert_eq!(s.solana_finished_ref(&[1; 32]), Some([2; 32]));
    assert_eq!(
        s.solana_burn(&[1; 32]),
        Some((5, 0)),
        "the keeper may reclaim it"
    );
    s.admits_solana(&first).unwrap();
    let other = SolanaStep::RefFinished {
        r#ref: [1; 32],
        arrival: [3; 32],
        burned_at_s: 5,
    };
    assert!(s.admits_solana(&other).is_err());
}

#[test]
fn a_co_sign_counts_for_its_signer_and_everyone_this_hour() {
    let mut s = state();
    let step = signed(SolanaPurpose::Cosign {
        r#ref: [1; 32],
        signer: [9; 32],
        message_hash: [4; 32],
    });
    apply(&mut s, &step, 7_200);
    assert_eq!(s.solana_signer(&[9; 32], 2), (false, false, 1));
    assert_eq!(s.solana_global(2), (1, 0));
    assert_eq!(s.solana_cosign_signer(&[7; 64]), Some([9; 32]));
    assert_eq!(s.solana_cosign_of_ref(&[1; 32]).map(|c| c.0), Some([4; 32]));
    assert_eq!(s.solana_burn(&[1; 32]), Some((7_200, 0)));
    // the next hour counts from zero
    assert_eq!(s.solana_signer(&[9; 32], 3), (false, false, 0));
    assert_eq!(s.solana_global(3), (0, 0));
    // the same message is never recorded twice
    assert!(s.admits_solana(&step).is_err());
}

#[test]
fn a_landed_failure_blocks_its_signer_and_charges_the_budget_a_landed_swap_counts_for_it() {
    let mut s = state();
    apply(
        &mut s,
        &signed(SolanaPurpose::Cosign {
            r#ref: [1; 32],
            signer: [9; 32],
            message_hash: [4; 32],
        }),
        7_200,
    );
    apply(
        &mut s,
        &SolanaStep::RefFinished {
            r#ref: [1; 32],
            arrival: [2; 32],
            burned_at_s: 7_100,
        },
        7_210,
    );
    assert_eq!(
        s.solana_signer(&[9; 32], 2),
        (false, true, 1),
        "a co-signed burn that finished landed"
    );
    apply(
        &mut s,
        &SolanaStep::CosignFailed {
            signer: [9; 32],
            signature: [7; 64],
        },
        7_220,
    );
    assert_eq!(s.solana_signer(&[9; 32], 2), (true, true, 1));
    assert_eq!(s.solana_global(2), (1, 1));
}

#[test]
fn a_reclaim_counts_its_tries_for_every_ref() {
    let mut s = state();
    apply(
        &mut s,
        &SolanaStep::RefFinished {
            r#ref: [1; 32],
            arrival: [2; 32],
            burned_at_s: 5,
        },
        10,
    );
    apply(
        &mut s,
        &signed(SolanaPurpose::Reclaim {
            refs: vec![[1; 32], [8; 32]],
        }),
        20,
    );
    assert_eq!(s.solana_burn(&[1; 32]), Some((5, 1)));
    assert_eq!(
        s.solana_burn(&[8; 32]),
        None,
        "a ref nothing recorded stays unrecorded"
    );
}

#[test]
fn a_top_up_records_its_time() {
    let mut s = state();
    assert_eq!(s.solana_last_top_up(), None);
    apply(&mut s, &signed(SolanaPurpose::TopUp), 77);
    assert_eq!(s.solana_last_top_up(), Some(77));
}

#[test]
fn one_proposal_is_live_at_a_time_and_only_the_live_one_can_end() {
    let mut s = state();
    assert!(s.solana_live_proposal().is_none());
    assert!(
        s.admits_solana(&SolanaStep::UpgradeProposed {
            proposal: proposal(1)
        })
        .is_err(),
        "the next id is 0"
    );
    apply(
        &mut s,
        &SolanaStep::UpgradeProposed {
            proposal: proposal(0),
        },
        10,
    );
    assert_eq!(s.solana_live_proposal().map(|p| p.id), Some(0));
    assert!(s
        .admits_solana(&SolanaStep::UpgradeProposed {
            proposal: proposal(1)
        })
        .is_err());
    assert!(s
        .admits_solana(&SolanaStep::UpgradeExecuted { id: 9 })
        .is_err());
    apply(&mut s, &SolanaStep::UpgradeCancelled { id: 0 }, 11);
    assert!(s.solana_live_proposal().is_none());
    assert_eq!(
        s.solana_proposal(0).map(|p| p.status),
        Some(SolanaProposalStatus::Cancelled)
    );
    assert!(s
        .admits_solana(&SolanaStep::UpgradeExecuted { id: 0 })
        .is_err());
    apply(
        &mut s,
        &SolanaStep::UpgradeProposed {
            proposal: proposal(1),
        },
        12,
    );
    apply(&mut s, &SolanaStep::UpgradeExecuted { id: 1 }, 13);
    assert_eq!(
        s.solana_proposal(1).map(|p| p.status),
        Some(SolanaProposalStatus::Executed)
    );
}

#[test]
fn a_landed_failure_is_charged_once_per_signature() {
    let mut s = state();
    let signature = [7; 64];
    let cosign = signed(SolanaPurpose::Cosign {
        r#ref: [1; 32],
        signer: [2; 32],
        message_hash: [3; 32],
    });
    apply(&mut s, &cosign, 3_600);
    assert_eq!(s.solana_cosign_signer(&signature), Some([2; 32]));
    assert!(!s.solana_cosign_reported(&signature));

    let failed = SolanaStep::CosignFailed {
        signer: [2; 32],
        signature,
    };
    apply(&mut s, &failed, 3_600);
    assert!(s.solana_cosign_reported(&signature));
    assert_eq!(s.solana_global(1).1, 1);
    assert!(s.admits_solana(&failed).is_err());
    assert_eq!(s.solana_global(1).1, 1, "the budget is charged once");
}

#[test]
fn a_source_counts_each_arrival_it_stood_for_once() {
    let mut s = state();
    let source = [9; 32];
    let of = |arrival| SolanaStep::ArrivalOfSource {
        source_tx: source,
        arrival,
    };
    assert_eq!(s.solana_other_arrivals(&source, &[1; 32]), 0);
    apply(&mut s, &of([1; 32]), 10);
    // a release after a complete is the same arrival, and stands for no other
    apply(&mut s, &of([1; 32]), 20);
    assert_eq!(s.solana_other_arrivals(&source, &[1; 32]), 0);
    assert_eq!(s.solana_other_arrivals(&source, &[2; 32]), 1);
    apply(&mut s, &of([2; 32]), 30);
    assert_eq!(s.solana_other_arrivals(&source, &[1; 32]), 1);
    assert_eq!(s.solana_other_arrivals(&source, &[3; 32]), 2);
    assert_eq!(s.solana_other_arrivals(&[8; 32], &[3; 32]), 0, "per source");
}
