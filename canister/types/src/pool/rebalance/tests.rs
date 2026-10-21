use super::*;

fn usdc(whole: u64) -> TokenAmount {
    TokenAmount::from(whole * 1_000_000)
}

/// The move's ref and the rebalance subaccount, pinned against `cast keccak` (2026-10-21):
/// the watcher reads the ref from `rebalance_moves`, the quoter builds the order under it.
#[test]
fn the_move_ref_and_the_rebalance_subaccount_are_pinned() {
    assert_eq!(
        hex::encode(move_ref(1).into_bytes()),
        "95e0b438067a5fafb32a89d0e829ab1a67205f5d73b554aeb957741984ae1c8b"
    );
    assert_eq!(
        hex::encode(move_ref(0).into_bytes()),
        "02c6632a2b00aefeed6be0162a46c21343547034b163687b1cb49732d7aa34ef"
    );
    assert_eq!(
        hex::encode(rebalance_subaccount()),
        "03aa5542eb289443e09e6689d40994e2551467846fbe4e0ad2497102f22e20b0"
    );
    assert_ne!(move_ref(1), move_ref(2));
}

#[test]
fn the_defaults_are_the_decided_ones_and_hold_together() {
    let settings = RebalanceSettings::default();
    assert_eq!(settings.target_icp_bps, 4_000);
    assert_eq!(settings.band_bps, 1_500);
    // $300 for now (Jeff, 2026-10-21)
    assert_eq!(settings.min_move, TokenAmount::from(300_000_000_u64));
    assert_eq!(settings.proposal_window, Duration::from_secs(60));
    assert_eq!(settings.max_proposal_fee_bps, 2);
    assert!(settings.standard_when_idle);
    assert_eq!(settings.validate(), Ok(()));
}

#[test]
fn settings_that_do_not_hold_together_are_refused() {
    let good = RebalanceSettings::default();
    let refused = |settings: RebalanceSettings| settings.validate().unwrap_err();
    assert_eq!(
        refused(RebalanceSettings {
            band_bps: 4_001,
            ..good
        }),
        RebalanceSettingsError::Band
    );
    assert_eq!(
        refused(RebalanceSettings {
            target_icp_bps: 9_000,
            band_bps: 1_001,
            ..good
        }),
        RebalanceSettingsError::Band
    );
    assert_eq!(
        refused(RebalanceSettings {
            min_move: TokenAmount::ZERO,
            ..good
        }),
        RebalanceSettingsError::Zero("min_move")
    );
    assert_eq!(
        refused(RebalanceSettings {
            max_move: usdc(299),
            ..good
        }),
        RebalanceSettingsError::MaxBelowMin
    );
    assert_eq!(
        refused(RebalanceSettings {
            daily_cap: usdc(4_999),
            ..good
        }),
        RebalanceSettingsError::DailyBelowMax
    );
    assert_eq!(
        refused(RebalanceSettings {
            proposal_window: Duration::from_secs(601),
            ..good
        }),
        RebalanceSettingsError::Window
    );
    assert_eq!(
        refused(RebalanceSettings {
            max_proposal_fee_bps: 101,
            ..good
        }),
        RebalanceSettingsError::FeeCap
    );
    // a window of zero takes no proposals, and is a setting the pool runs on
    assert_eq!(
        RebalanceSettings {
            proposal_window: Duration::ZERO,
            ..good
        }
        .validate(),
        Ok(())
    );
}

#[test]
fn the_proposal_fee_cap_is_its_bps_of_the_move_rounded_down() {
    let settings = RebalanceSettings::default();
    // 2 bps of $1,000 is 20 cents
    assert_eq!(
        settings.proposal_fee_cap(usdc(1_000)),
        TokenAmount::from(200_000_u64)
    );
    assert_eq!(
        settings.proposal_fee_cap(TokenAmount::from(4_999_u64)),
        TokenAmount::ZERO
    );
}

#[test]
fn the_daily_cap_reads_only_the_last_day() {
    let at = |secs: u64| Timestamp::from_secs(secs).unwrap();
    let lines = RebalanceLines {
        recent: vec![(at(1_000), usdc(100)), (at(90_000), usdc(200))],
        ..RebalanceLines::default()
    };
    assert_eq!(lines.opened_within_day(at(90_000)), usdc(200));
    assert_eq!(lines.opened_within_day(at(80_000)), usdc(300));
    // a day after the first, only the second
    assert_eq!(lines.opened_within_day(at(1_000 + 86_400)), usdc(200));
}

#[test]
fn a_move_is_credited_at_most_its_amount_less_its_route() {
    let mut item = Move::opened(
        3,
        MoveDirection::BaseToIcp,
        usdc(1_000),
        false,
        Timestamp::from_nanos(7),
        None,
        TokenAmount::from(90_000_u64),
    );
    assert_eq!(item.creditable(), usdc(1_000));
    item.route = Some(MoveRoute::Own {
        max_fee: TokenAmount::from(200_000_u64),
        fast: true,
    });
    assert_eq!(item.creditable(), TokenAmount::from(999_800_000_u64));
    item.route = Some(MoveRoute::Proposed {
        fee: TokenAmount::from(150_000_u64),
    });
    assert_eq!(item.creditable(), TokenAmount::from(999_850_000_u64));
    assert_eq!(item.r#ref(), move_ref(3));
}

/// Base to ICP is proposable from its opening; ICP to Base only once its USDC landed on
/// Ethereum; an urgent move never.
#[test]
fn where_a_move_takes_proposals() {
    let open = |direction| {
        Move::opened(
            1,
            direction,
            usdc(500),
            false,
            Timestamp::from_nanos(7),
            None,
            TokenAmount::ZERO,
        )
    };
    assert!(open(MoveDirection::BaseToIcp).proposable());
    assert!(!open(MoveDirection::IcpToBase).proposable());
    let mut landed = open(MoveDirection::IcpToBase);
    landed.status = MoveStatus::Landed;
    assert!(landed.proposable());
    let mut urgent = open(MoveDirection::BaseToIcp);
    urgent.urgent = true;
    assert!(!urgent.proposable());
    assert!(urgent.routable());
}

#[test]
fn a_move_round_trips_through_its_stored_bytes() {
    use ic_stable_structures::Storable;
    let mut item = Move::opened(
        9,
        MoveDirection::IcpToBase,
        usdc(800),
        true,
        Timestamp::from_nanos(11),
        Some(Timestamp::from_nanos(12)),
        TokenAmount::from(13_u64),
    );
    item.intent = Some([0x14; 32]);
    item.stuck = Some("the minter's answer was lost".to_string());
    let bytes = item.to_bytes();
    assert_eq!(Move::from_bytes(bytes), item);
}

/// The ck helper's `depositErc20` (selector `0xdb9751af`, checked against the deployed helper
/// with cast for DeploySwapic), and the principal's word the helper and `ck_principal` read.
#[test]
fn the_ck_deposit_names_the_principal_and_the_subaccount_it_is_given() {
    let principal = candid::Principal::from_text("2ibo7-dia").unwrap();
    let word = ck_word(&principal);
    assert_eq!(crate::swapic::ck_principal(word), Some(principal));
    let usdc: crate::evm::EvmAddress = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        .parse()
        .unwrap();
    let data = ck_deposit_data(usdc, usdc_amount(), word, rebalance_subaccount());
    assert_eq!(hex::encode(&data[..4]), "db9751af");
    assert_eq!(data.len(), 4 + 4 * 32);
    assert_eq!(data[4 + 12..4 + 32], *usdc.as_bytes());
    assert_eq!(
        TokenAmount::from_be_bytes(data[36..68].try_into().unwrap()),
        usdc_amount()
    );
    assert_eq!(data[68..100], word);
    assert_eq!(data[100..132], rebalance_subaccount());
}

fn usdc_amount() -> TokenAmount {
    usdc(1_234)
}
