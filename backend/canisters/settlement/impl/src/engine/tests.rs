use super::*;
use types::{Attempt, BasisPoints, Leg, Outcome, SwapStatus};

fn at(status: SwapStatus, last_leg: Option<Leg>, last_outcome: Option<Outcome>) -> Swap {
    Swap {
        quote_bytes: vec![],
        status,
        last_attempt: last_leg.map(|_| Attempt::FIRST),
        open_attempt: None,
        src_chain: ChainId::BASE,
        src_token: "usdc".parse().unwrap(),
        amount_in: TokenAmount::from(100_u8),
        amount_paid: None,
        waiting_since: None,
        last_leg,
        last_outcome,
        last_tx_hash: None,
    }
}

/// The engine's whole table, one row per reachable combination of status, latest leg and
/// its outcome. It is the law of which line follows which, so every row is spelled out.
#[test]
fn next_action_is_a_table_over_status_leg_and_outcome() {
    use Action::*;
    use Leg::{Burn, Mint, Payout, Reclaim as ReclaimLeg, Refund};
    use Outcome::*;
    use SwapStatus::*;
    let confirmed = Some(Confirmed);
    let failed = Some(Failed);
    let table: Vec<(SwapStatus, Option<Leg>, Option<Outcome>, Action)> = vec![
        // the funds are in the source vault and nothing has been signed: the rail's first leg
        (FundsReceived, None, None, Rail),
        // executing: the rail's own legs, until the stable is on the destination side
        (Executing, None, None, Rail),
        (Executing, Some(Burn), confirmed, Rail),
        (Executing, Some(Mint), confirmed, Rail),
        // a burn that reverted moved nothing, so the user is paid back
        (
            Executing,
            Some(Burn),
            failed,
            StartRefund("the burn reverted on the chain"),
        ),
        // a mint that reverted has the funds burned and not minted: a human
        (
            Executing,
            Some(Mint),
            failed,
            Freeze("the mint reverted on the chain"),
        ),
        // legs that are not the rail's, in a status that is the rail's
        (
            Executing,
            Some(Payout),
            confirmed,
            Freeze("a payout confirmed while executing"),
        ),
        (
            Executing,
            Some(Refund),
            confirmed,
            Freeze("a refund confirmed while executing"),
        ),
        (
            Executing,
            Some(ReclaimLeg),
            confirmed,
            Freeze("a reclaim confirmed while executing"),
        ),
        (
            Executing,
            Some(Payout),
            failed,
            Freeze("a payout failed while executing"),
        ),
        (
            Executing,
            Some(Refund),
            failed,
            Freeze("a refund failed while executing"),
        ),
        (
            Executing,
            Some(ReclaimLeg),
            failed,
            Freeze("a reclaim failed while executing"),
        ),
        // paid in stable: the user is paid out, whatever leg brought the stable in
        (PaidInStable, Some(Mint), confirmed, SendPayout),
        (PaidInStable, Some(Burn), confirmed, SendPayout),
        (PaidInStable, None, None, SendPayout),
        // delivering: the payout landed, or it did not
        (Delivering, Some(Payout), confirmed, RecordDone),
        (
            Delivering,
            Some(Payout),
            failed,
            Freeze("the payout reverted on the chain"),
        ),
        (
            Delivering,
            Some(Mint),
            confirmed,
            Freeze("delivering with no payout signed"),
        ),
        (
            Delivering,
            None,
            None,
            Freeze("delivering with no payout signed"),
        ),
        // refunding: paid back from the vault while the funds are in it, reclaimed first
        // when they are on the rail, and never when they have left for good
        (Refunding, None, None, SendRefund),
        (Refunding, Some(Burn), failed, SendRefund),
        (Refunding, Some(ReclaimLeg), confirmed, SendRefund),
        (Refunding, Some(Refund), confirmed, RecordRefunded),
        (Refunding, Some(Burn), confirmed, RailReclaim),
        (
            Refunding,
            Some(Refund),
            failed,
            Freeze("the refund reverted on the chain"),
        ),
        (
            Refunding,
            Some(ReclaimLeg),
            failed,
            Freeze("the reclaim reverted on the chain"),
        ),
        (
            Refunding,
            Some(Mint),
            confirmed,
            Freeze("the stable is on the destination side"),
        ),
        (
            Refunding,
            Some(Mint),
            failed,
            Freeze("the funds left for the rail and never arrived"),
        ),
        (
            Refunding,
            Some(Payout),
            confirmed,
            Freeze("the user was paid out"),
        ),
        (
            Refunding,
            Some(Payout),
            failed,
            Freeze("the user was paid out"),
        ),
        // nothing to do: the user is being asked, or the swap is closed
        (WaitingForUser, None, None, Wait),
        (WaitingForUser, Some(Burn), confirmed, Wait),
        (Done, Some(Payout), confirmed, Wait),
        (Refunded, Some(Refund), confirmed, Wait),
        (Frozen, Some(Mint), failed, Wait),
        (Frozen, None, None, Wait),
    ];
    for (status, leg, outcome, expected) in table {
        assert_eq!(
            next_action(&at(status, leg, outcome)),
            expected,
            "{status:?} after {leg:?} {outcome:?}"
        );
    }
}

/// An attempt that is still open is waited on, whatever else the swap says: the outbox
/// is deciding it against the chain.
#[test]
fn an_open_attempt_is_always_waited_on() {
    use SwapStatus::*;
    for status in [
        FundsReceived,
        Executing,
        PaidInStable,
        Delivering,
        Refunding,
    ] {
        for leg in [None, Some(Leg::Burn), Some(Leg::Payout), Some(Leg::Refund)] {
            let mut swap = at(status, leg, None);
            swap.open_attempt = Some(Attempt::FIRST);
            assert_eq!(next_action(&swap), Action::Wait, "{status:?} {leg:?}");
        }
    }
}

/// A closed leg whose outcome the fold never recorded is a state no line produces: it is
/// stopped for a human rather than stepped from.
#[test]
fn a_closed_leg_with_no_outcome_is_stopped() {
    use SwapStatus::*;
    for status in [Executing, Delivering, Refunding] {
        assert_eq!(
            next_action(&at(status, Some(Leg::Burn), None)),
            Action::Freeze("a leg closed with no outcome recorded"),
            "{status:?}"
        );
    }
}

/// The payout is what the stable brought in less the platform's fee, and never below the
/// least the user was quoted.
#[test]
fn the_payout_is_the_stable_less_the_fee_and_never_below_min_out() {
    let paid = TokenAmount::from(24_995_000_u32);
    let fee = |bps: u16| BasisPoints::new(bps);
    assert_eq!(
        payout_of(paid, fee(0), TokenAmount::from(24_900_000_u32)),
        Ok(Payout {
            amount: paid,
            fee: TokenAmount::ZERO,
        })
    );
    assert_eq!(
        payout_of(paid, fee(30), TokenAmount::from(24_900_000_u32)),
        Ok(Payout {
            amount: TokenAmount::from(24_920_015_u32),
            fee: TokenAmount::from(74_985_u32),
        }),
        "thirty basis points, rounded down in the platform's disfavour"
    );
    assert_eq!(
        payout_of(paid, fee(30), TokenAmount::from(24_990_000_u32)),
        Err(EngineError::BelowMinOut {
            payout: TokenAmount::from(24_920_015_u32),
            min_out: TokenAmount::from(24_990_000_u32),
        })
    );
    assert_eq!(
        payout_of(paid, fee(0), paid),
        Ok(Payout {
            amount: paid,
            fee: TokenAmount::ZERO,
        }),
        "exactly the minimum is enough"
    );
}

/// Rule A7: one tick at a time, and the guard comes back when the tick ends, including
/// when it traps and unwinds through it.
#[test]
fn one_tick_runs_at_a_time_and_the_guard_comes_back_on_a_trap() {
    let first = TickGuard::take();
    assert!(first.is_some(), "the first tick takes the guard");
    assert!(
        TickGuard::take().is_none(),
        "a second tick finds the guard taken"
    );
    drop(first);
    assert!(
        TickGuard::take().is_some(),
        "the guard comes back when a tick ends"
    );

    let unwound = std::panic::catch_unwind(|| {
        let _guard = TickGuard::take().expect("the guard is free");
        panic!("the tick traps");
    });
    assert!(unwound.is_err());
    assert!(
        TickGuard::take().is_some(),
        "a trap releases the guard on the way out"
    );
}
