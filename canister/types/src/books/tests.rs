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
    let books = Books::default().apply(&BookStep::FeeEarned {
        tx_hash: TxHash::new([1; 32]),
        amount: usdc(100),
    });
    let (expected, shortfall) = assess(
        books.total(TokenAmount::ZERO),
        TokenAmount::ZERO,
        usdc(150),
        usdc(1),
    );
    assert_eq!(expected, usdc(100));
    assert!(!shortfall);
    let books = books.apply(&checked(usdc(150), expected));
    assert_eq!(books.last_check.unwrap().surplus(), 50_000_000);
    assert_eq!(books.fee_income, usdc(100), "the check books nothing");
    assert_eq!(books.unattributed, TokenAmount::ZERO);
}

/// Review Focus 5: a payout of 30 signed and not yet confirmed is no shortfall, whether
/// the block the balance is read at has it mined or not.
#[test]
fn pending_payout_is_not_a_shortfall() {
    let lines = Books::default().total(usdc(100));
    // mined at the block: 70 left, as expected
    assert_eq!(
        assess(lines, usdc(30), usdc(70), usdc(1)),
        (usdc(70), false)
    );
    // not mined yet: 100 still there, a passing surplus
    assert_eq!(
        assess(lines, usdc(30), usdc(100), usdc(1)),
        (usdc(70), false)
    );
    // gone further than the payout and the tolerance: a shortfall
    assert_eq!(assess(lines, usdc(30), usdc(68), usdc(1)), (usdc(70), true));
    // within the tolerance: not one
    assert_eq!(
        assess(lines, usdc(30), usdc(69), usdc(1)),
        (usdc(70), false)
    );
}

#[test]
fn a_surplus_is_booked_only_up_to_what_the_check_found() {
    let books = Books::default().apply(&checked(usdc(150), usdc(100)));
    assert_eq!(
        books.admits(&BookStep::SurplusBooked { amount: usdc(51) }),
        Err(BooksError::AboveSurplus {
            amount: usdc(51),
            surplus: 50_000_000
        })
    );
    let step = BookStep::SurplusBooked { amount: usdc(50) };
    assert_eq!(books.admits(&step), Ok(()));
    let books = books.apply(&step);
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
