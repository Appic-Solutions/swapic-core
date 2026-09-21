use super::*;
use types::numeric::WeiPerGas;
use types::{BlockNumber, Timestamp};

fn wire() -> ChainData {
    ChainData {
        block: 19_000_000,
        base_fee_wei_per_gas: Nat::from(1_000_000_000_u64),
        priority_fee_wei_per_gas: Nat::from(100_000_000_u64),
    }
}

/// The wire reading crosses to the domain and back unchanged, so the query answers what the
/// watcher pushed.
#[test]
fn a_wire_reading_crosses_to_the_domain_and_back() {
    let domain = types::chain_data::ChainReading::try_from(wire()).unwrap();
    assert_eq!(domain.block, BlockNumber::new(19_000_000));
    assert_eq!(domain.base_fee, WeiPerGas::from(1_000_000_000_u64));
    assert_eq!(domain.priority_fee, WeiPerGas::from(100_000_000_u64));

    let stamped = domain.pushed_at(Timestamp::from_nanos(1_700_000_000_000_000_000));
    let entry = ChainDataEntry::from(stamped);
    assert_eq!(entry.data, wire());
    assert_eq!(entry.pushed_at_ns, 1_700_000_000_000_000_000);
}

/// A fee above 256 bits is no fee this canister prices gas with, and the refusal names the
/// field rather than wrapping it.
#[test]
fn a_fee_above_256_bits_names_its_field() {
    // 2^256, the first fee no 256-bit price holds
    let too_large = Nat::parse(
        b"115792089237316195423570985008687907853269984665640564039457584007913129639936",
    )
    .unwrap();
    for (field, wire) in [
        (
            "base_fee_wei_per_gas",
            ChainData {
                base_fee_wei_per_gas: too_large.clone(),
                ..self::wire()
            },
        ),
        (
            "priority_fee_wei_per_gas",
            ChainData {
                priority_fee_wei_per_gas: too_large.clone(),
                ..self::wire()
            },
        ),
    ] {
        assert_eq!(
            types::chain_data::ChainReading::try_from(wire),
            Err(types::chain_data::ChainDataError::FeeTooLarge { field })
        );
    }
}
