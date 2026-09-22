use super::*;

const USDC: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const USER: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";
const VAULT: &str = "0x1111111111111111111111111111111111111111";

fn permit2() -> Permit2Sig {
    Permit2Sig {
        quote_hash: [0x5a; 32],
        token: USDC.to_string(),
        owner: USER.to_string(),
        spender: VAULT.to_string(),
        amount: Nat::from(25_000_000_u32),
        nonce: Nat::from(7_u8),
        deadline_s: 1_800_000_000,
        signature: vec![0x22; 65],
    }
}

fn eip2612() -> PermitSig {
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

/// A Permit2 permit crosses to the vault's own types, and one that names something that is
/// not an address, or an amount no preimage holds, is refused by the field at fault.
#[test]
fn a_permit_crosses_to_the_domain_and_a_bad_one_names_its_field() {
    let pulled = PullPermit::try_from(PullRequest::Permit2(permit2())).unwrap();
    assert_eq!(pulled.witness, types::QuoteHash::new([0x5a; 32]));
    assert_eq!(pulled.owner, USER.parse().unwrap());
    assert_eq!(pulled.spender, VAULT.parse().unwrap());
    assert_eq!(pulled.permit.token, USDC.parse().unwrap());
    assert_eq!(pulled.permit.amount, TokenAmount::from(25_000_000_u32));
    assert_eq!(pulled.permit.nonce, Permit2Nonce::from(7_u8));
    assert_eq!(pulled.permit.deadline, UnixSeconds::new(1_800_000_000));
    assert_eq!(pulled.signature, vec![0x22; 65]);

    let no_prefix = Permit2Sig {
        owner: USER[2..].to_string(),
        ..permit2()
    };
    assert_eq!(
        PullPermit::try_from(PullRequest::Permit2(no_prefix)),
        Err(PermitError::NotAnAddress {
            field: "owner".to_string(),
            reason: EvmAddressError::NoPrefix
        })
    );
    let short = Permit2Sig {
        token: "0x1234".to_string(),
        ..permit2()
    };
    assert_eq!(
        PullPermit::try_from(PullRequest::Permit2(short)),
        Err(PermitError::NotAnAddress {
            field: "token".to_string(),
            reason: EvmAddressError::WrongLength { len: 4 }
        })
    );
    let not_a_spender = Permit2Sig {
        spender: "vault".to_string(),
        ..permit2()
    };
    assert_eq!(
        PullPermit::try_from(PullRequest::Permit2(not_a_spender)),
        Err(PermitError::NotAnAddress {
            field: "spender".to_string(),
            reason: EvmAddressError::NoPrefix
        })
    );
    let huge = Permit2Sig {
        amount: Nat::from(u128::MAX) + Nat::from(1_u8),
        ..permit2()
    };
    assert_eq!(
        PullPermit::try_from(PullRequest::Permit2(huge)),
        Err(PermitError::AmountTooLarge)
    );
    let huge_nonce = Permit2Sig {
        nonce: Nat::from(u128::MAX) * Nat::from(u128::MAX) * Nat::from(4_u8),
        ..permit2()
    };
    assert_eq!(
        PullPermit::try_from(PullRequest::Permit2(huge_nonce)),
        Err(PermitError::NonceTooLarge)
    );
}

/// The 2612 door is not one this canister pulls through: the vault transfers on a standing
/// allowance whether or not the permit verified, and a 2612 signature names no quote, so a
/// pull carrying one is refused by name and the signature it carries is never encoded. A
/// signature too long to be one is refused as well, before it is paid for in gas.
#[test]
fn a_2612_permit_and_an_oversized_signature_are_refused() {
    assert_eq!(
        PullPermit::try_from(PullRequest::Eip2612(eip2612())),
        Err(PermitError::NotAPermit2Permit)
    );
    let long = Permit2Sig {
        signature: vec![0x22; MAX_PERMIT2_SIGNATURE_BYTES + 1],
        ..permit2()
    };
    assert_eq!(
        PullPermit::try_from(PullRequest::Permit2(long)),
        Err(PermitError::SignatureTooLong {
            len: MAX_PERMIT2_SIGNATURE_BYTES as u64 + 1,
            cap: MAX_PERMIT2_SIGNATURE_BYTES as u64,
        })
    );
    let at_the_cap = Permit2Sig {
        signature: vec![0x22; MAX_PERMIT2_SIGNATURE_BYTES],
        ..permit2()
    };
    assert!(PullPermit::try_from(PullRequest::Permit2(at_the_cap)).is_ok());
}
