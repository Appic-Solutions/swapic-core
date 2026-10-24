use super::*;

fn usdc(amount: u64) -> TokenAmount {
    TokenAmount::from(amount * 1_000_000)
}

fn checked(observed: TokenAmount, expected: TokenAmount) -> BookStep {
    BookStep::Checked {
        check: BalanceCheck {
            block: BlockNumber::new(100),
            observed,
            expected,
        },
        shortfall: false,
    }
}

/// Review Focus 5: 50 USDC sent to the canister's address unasked reads as surplus, halts
/// nothing, and stays out of the lines until a controller books it.
#[test]
fn donation_is_surplus_and_halts_nothing() {
    let books = Books::default().apply(
        &BookStep::FeeEarned {
            tx_hash: TxHash::new([1; 32]),
            amount: usdc(100),
            block: None,
        },
        Timestamp::from_nanos(1),
    );
    let (expected, shortfall) = assess(books.total(), usdc(150), usdc(1));
    assert_eq!(expected, usdc(100));
    assert!(!shortfall);
    let books = books.apply(&checked(usdc(150), expected), Timestamp::from_nanos(1));
    assert_eq!(books.last_check.unwrap().surplus(), 50_000_000);
    assert_eq!(books.fee_income, usdc(100), "the check books nothing");
    assert_eq!(books.unattributed, TokenAmount::ZERO);
}

/// A balance below the lines is a shortfall only beyond the tolerance.
#[test]
fn a_shortfall_is_only_beyond_the_tolerance() {
    let lines = usdc(70);
    assert_eq!(assess(lines, usdc(70), usdc(1)), (usdc(70), false));
    assert_eq!(assess(lines, usdc(100), usdc(1)), (usdc(70), false));
    assert_eq!(assess(lines, usdc(68), usdc(1)), (usdc(70), true));
    assert_eq!(assess(lines, usdc(69), usdc(1)), (usdc(70), false));
}

#[test]
fn a_surplus_is_booked_only_up_to_what_the_check_found() {
    let books = Books::default().apply(&checked(usdc(150), usdc(100)), Timestamp::from_nanos(1));
    assert_eq!(
        books.admits(&BookStep::SurplusBooked { amount: usdc(51) }),
        Err(BooksError::AboveSurplus {
            amount: usdc(51),
            surplus: 50_000_000
        })
    );
    let step = BookStep::SurplusBooked { amount: usdc(50) };
    assert_eq!(books.admits(&step), Ok(()));
    let books = books.apply(&step, Timestamp::from_nanos(1));
    assert_eq!(books.unattributed, usdc(50));
    assert_eq!(books.last_check.unwrap().surplus(), 0);
    assert!(books
        .admits(&BookStep::SurplusBooked { amount: usdc(1) })
        .is_err());
    assert!(Books::default()
        .admits(&BookStep::SurplusBooked { amount: usdc(1) })
        .is_err());
}

#[test]
fn a_step_round_trips_through_its_bytes() {
    let step = checked(usdc(3), usdc(2));
    assert_eq!(BookStep::from_bytes(&step.to_bytes()), Some(step));
}
