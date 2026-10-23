use super::*;
use candid::Nat;

fn wire() -> WireSettings {
    WireSettings::from(&RebalanceSettings::default())
}

#[test]
fn settings_are_set_by_a_controller_and_hold_together() {
    assert_eq!(checked(true, true, wire()), Err(PoolError::Halted));
    assert!(matches!(
        checked(false, false, wire()),
        Err(PoolError::NotAllowed(_))
    ));
    assert_eq!(
        checked(false, true, wire()),
        Ok(RebalanceSettings::default())
    );
    let bad = WireSettings {
        max_move: Nat::from(1_u8),
        ..wire()
    };
    assert!(matches!(
        checked(false, true, bad),
        Err(PoolError::BadSettings(_))
    ));
}

/// Review Focus 3: a move that fails half way never loses track of the money.
mod stuck_moves {
    use super::super::drive::{next, Next, MAX_REVERTS};
    use super::*;
    use crate::storage::on_fresh_memory;
    use crate::storage::pool::tests::{base_stake, created, funded_pool, usdc};
    use crate::storage::pool::{ledger, record_at};
    use types::events::TxPurpose;
    use types::pool::payout::PinnedOrder;
    use types::pool::{MoveDirection, MoveRoute, MoveStatus};
    use types::{BlockNumber, ChainId, Nonce, TokenAmount, TxHash};

    fn at(secs: u64) -> Timestamp {
        Timestamp::from_secs(1_800_000_000 + secs).unwrap()
    }

    fn pinned() -> PinnedOrder {
        PinnedOrder {
            order_id: [0x91; 32],
            order: vec![0x92; 8],
            gas_limit: 300_000,
            deadline_s: at(3_600).as_secs().get(),
        }
    }

    /// $4,000 on ICP, $6,000 on Base, and a $1,000 move to ICP routed on its own order.
    fn routed_move() -> u64 {
        crate::storage::init();
        funded_pool(usdc(4_000));
        base_stake(
            "0x1111111111111111111111111111111111111111"
                .parse()
                .unwrap(),
            usdc(6_000),
            0x52,
            8,
        );
        record_at(
            PoolStep::MoveSettingsSet {
                settings: RebalanceSettings::default(),
            },
            at(1),
        )
        .unwrap();
        record_at(
            PoolStep::MoveOpened {
                id: 0,
                direction: MoveDirection::BaseToIcp,
                amount: usdc(1_000),
                urgent: false,
                opened_at: at(2),
                window_ends: None,
                own_cost: TokenAmount::ZERO,
            },
            at(2),
        )
        .unwrap();
        record_at(
            PoolStep::MoveRouted {
                id: 0,
                route: MoveRoute::Own {
                    max_fee: TokenAmount::ZERO,
                    fast: false,
                },
                order: pinned(),
                scan_from: None,
            },
            at(3),
        )
        .unwrap();
        0
    }

    fn signed(id: u64, nonce: u64, secs: u64) {
        crate::storage::events::append_event_at(created(TxPurpose::PoolMove(id), nonce), at(secs))
            .unwrap();
        record_at(
            PoolStep::MoveSigned {
                id,
                chain_id: ChainId::BASE,
                nonce: Nonce::new(nonce),
                tx_hash: TxHash::new([nonce as u8 + 1; 32]),
                raw_tx: vec![0x02],
            },
            at(secs),
        )
        .unwrap();
    }

    #[test]
    fn a_move_stuck_after_its_burn_stays_in_flight_listed_and_pauses_nothing() {
        on_fresh_memory(|| {
            let id = routed_move();
            signed(id, 0, 4);
            record_at(
                PoolStep::MoveBurned {
                    id,
                    tx_hash: TxHash::new([1; 32]),
                    block: BlockNumber::new(40),
                    intent: None,
                },
                at(5),
            )
            .unwrap();
            // three hours on, no ckUSDC: it stops for a controller
            let item = item(id).unwrap();
            let step = next(
                &item,
                at(5 + 3 * 3_600),
                false,
                std::time::Duration::from_secs(300),
            );
            let Next::Stuck(why) = step else {
                panic!("not stopped: {step:?}");
            };
            record_at(PoolStep::MoveStuck { id, why }, at(5 + 3 * 3_600)).unwrap();
            // held in flight, in the assets, listed
            let pool = ledger();
            assert_eq!(pool.pockets.base.in_flight, usdc(1_000));
            assert_eq!(pool.total_assets(), usdc(10_000));
            let listed = read_state(stuck);
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].status, MoveStatus::Burned);
            assert_eq!(
                MoveView::from(&listed[0]).status,
                settlement_api::types::pool::MoveStatus::Stuck(
                    "no ckUSDC arrived in the rebalance subaccount".to_string()
                )
            );
            // the balance checks expect it nowhere: Base holds its $5,000, ICP its $4,000,
            // and neither reads short
            let (expected, short) = read_state(|state| {
                crate::books::expected_at(
                    state,
                    ChainId::BASE,
                    Some(ChainId::BASE),
                    BlockNumber::new(41),
                    usdc(5_000),
                    TokenAmount::ZERO,
                )
            });
            assert_eq!((expected, short), (usdc(5_000), false));
            let (_, icp_short) = read_state(|state| {
                crate::pool::icp::icp_expected(state, usdc(4_000), TokenAmount::ZERO)
            });
            assert!(!icp_short);
        });
    }

    #[test]
    fn a_controllers_retry_signs_the_same_order_again() {
        on_fresh_memory(|| {
            let id = routed_move();
            for nonce in 0..u64::from(MAX_REVERTS) {
                signed(id, nonce, 4 + nonce);
                record_at(
                    PoolStep::MoveReverted {
                        id,
                        block: Some(types::BlockNumber::new(30 + nonce)),
                    },
                    at(4 + nonce),
                )
                .unwrap();
            }
            let item = item(id).unwrap();
            let Next::Stuck(why) = next(&item, at(10), false, std::time::Duration::from_secs(300))
            else {
                panic!("not stopped after its reverts");
            };
            record_at(PoolStep::MoveStuck { id, why }, at(10)).unwrap();
            // reverted, it is reserved again on Base, not in flight
            let base = ledger().pockets.base;
            assert_eq!(
                (base.reserved, base.in_flight),
                (usdc(1_000), TokenAmount::ZERO)
            );
            assert_eq!(
                resolve(true, true, id, MoveResolution::Retry, at(11)),
                Err(PoolError::Halted)
            );
            assert!(matches!(
                resolve(false, false, id, MoveResolution::Retry, at(11)),
                Err(PoolError::NotAllowed(_))
            ));
            assert_eq!(
                resolve(false, true, 9, MoveResolution::Retry, at(11)),
                Err(PoolError::Unknown)
            );
            resolve(false, true, id, MoveResolution::Retry, at(11)).unwrap();
            let item = super::item(id).unwrap();
            assert_eq!(item.stuck, None);
            // the order it pinned, signed again once Swapic says it did not run
            assert_eq!(item.order, Some(pinned()));
            assert_eq!(
                next(&item, at(12), false, std::time::Duration::from_secs(300)),
                Next::Sign { again: true }
            );
            // written off: out of the assets
            resolve(
                false,
                true,
                id,
                MoveResolution::Lost("the order cannot run".to_string()),
                at(13),
            )
            .unwrap();
            assert_eq!(ledger().pockets.base.reserved, TokenAmount::ZERO);
            assert_eq!(ledger().total_assets(), usdc(9_000));
            assert!(read_state(open_moves).is_empty());
        });
    }
}
