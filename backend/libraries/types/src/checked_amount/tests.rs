use crate::checked_amount::{CheckedAmountOf, NatOverflow};
use candid::Nat;
use num_bigint::BigUint;

enum Unit {}
type Amount = CheckedAmountOf<Unit>;

mod checked_div_ceil {
    use super::Amount;
    use proptest::prelude::any;
    use proptest::proptest;

    proptest! {
        #[test]
        fn should_be_zero_when_dividend_is_zero(divisor in 1_u128..=u128::MAX) {
            assert_eq!(Amount::ZERO, Amount::ZERO.checked_div_ceil(divisor).unwrap());
        }
    }

    proptest! {
        #[test]
        fn should_be_none_when_divisor_is_zero(amount in any::<u128>()) {
            assert_eq!(None, Amount::from(amount).checked_div_ceil(0_u8));
        }
    }

    proptest! {
        #[test]
        fn should_be_like_floor_division_for_multiple_of_divisors(quotient in any::<u128>(), divisor in 1_u128..=u128::MAX) {
            let expected_quotient = Amount::from(quotient);
            let amount = expected_quotient.checked_mul(divisor).expect("multiplication of two u128 fits in a u256");

            let actual_quotient = amount.checked_div_ceil(divisor).unwrap();

            assert_eq!(expected_quotient, actual_quotient);
        }
    }

    proptest! {
        #[test]
        fn should_increment_quotient_of_floor_division_when_not_multiple_of_divisor(divisor in 1_u128..=u128::MAX) {
            let large_prime_number = Amount::from_str_hex(
                "0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F",
            )
            .expect("valid u256 since this is the p parameter of ECDSA Secp256k1 curve");

            let actual_quotient = large_prime_number.checked_div_ceil(divisor).unwrap();

            let expected_quotient = large_prime_number.0 / divisor + 1;
            assert_eq!(expected_quotient, actual_quotient.0);
        }
    }
}

#[test]
fn try_into_u128_stops_at_u128_max() {
    assert_eq!(Amount::from(u128::MAX).try_into_u128(), Some(u128::MAX));
    assert_eq!(Amount::ZERO.try_into_u128(), Some(0));
    let above = Amount::from(u128::MAX).checked_add(Amount::ONE).unwrap();
    assert_eq!(above.try_into_u128(), None);
}

#[test]
fn nat_round_trips_up_to_u256_max() {
    for amount in [Amount::ZERO, Amount::from(u128::MAX), Amount::MAX] {
        assert_eq!(Amount::try_from(Nat::from(amount)), Ok(amount));
    }
    let too_large = Nat::from(BigUint::from_bytes_be(&[1; 33]));
    assert_eq!(
        Amount::try_from(too_large.clone()),
        Err(NatOverflow(too_large))
    );
}

#[test]
fn display_prints_the_plain_number() {
    assert_eq!(Amount::from(25_000_000_u32).to_string(), "25000000");
    assert_eq!(format!("{:?}", Amount::from(7_u8)), "7");
}

#[test]
fn cbor_round_trips_small_and_bignum_amounts() {
    for amount in [
        Amount::ZERO,
        Amount::from(u64::MAX),
        Amount::from(u128::MAX),
        Amount::MAX,
    ] {
        let bytes = minicbor::to_vec(amount).unwrap();
        assert_eq!(minicbor::decode::<Amount>(&bytes).unwrap(), amount);
    }
    // up to u64::MAX a native integer, one byte for a small value
    assert_eq!(minicbor::to_vec(Amount::from(7_u8)).unwrap(), vec![0x07]);
}
