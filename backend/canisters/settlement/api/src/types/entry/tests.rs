use super::*;

const USDC: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const USER: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";

fn permit() -> PermitSig {
    PermitSig {
        token: USDC.to_string(),
        owner: USER.to_string(),
        amount: Nat::from(25_000_000_u32),
        deadline_s: 1_800_000_000,
        v: 28,
        r: [0x22; 32],
        s: [0x33; 32],
    }
}

/// A permit crosses to the vault's own types, and one that names something that is not an
/// address, or an amount no preimage holds, is refused by the field at fault.
#[test]
fn a_permit_crosses_to_the_domain_and_a_bad_one_names_its_field() {
    let pulled = PullPermit::try_from(permit()).unwrap();
    assert_eq!(pulled.token, USDC.parse().unwrap());
    assert_eq!(pulled.owner, USER.parse().unwrap());
    assert_eq!(pulled.amount, TokenAmount::from(25_000_000_u32));
    assert_eq!(pulled.deadline, UnixSeconds::new(1_800_000_000));
    assert_eq!(
        pulled.signature,
        Permit {
            v: 28,
            r: [0x22; 32],
            s: [0x33; 32]
        }
    );

    let no_prefix = PermitSig {
        owner: USER[2..].to_string(),
        ..permit()
    };
    assert_eq!(
        PullPermit::try_from(no_prefix),
        Err(PermitError::NotAnAddress {
            field: "owner".to_string(),
            reason: EvmAddressError::NoPrefix
        })
    );
    let short = PermitSig {
        token: "0x1234".to_string(),
        ..permit()
    };
    assert_eq!(
        PullPermit::try_from(short),
        Err(PermitError::NotAnAddress {
            field: "token".to_string(),
            reason: EvmAddressError::WrongLength { len: 4 }
        })
    );
    let huge = PermitSig {
        amount: Nat::from(u128::MAX) + Nat::from(1_u8),
        ..permit()
    };
    assert_eq!(PullPermit::try_from(huge), Err(PermitError::AmountTooLarge));
}
