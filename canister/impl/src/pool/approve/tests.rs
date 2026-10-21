use super::*;

fn swapic() -> EvmAddress {
    "0xF0Ba68cBc87c31eCaA9654EB14daF3632eaCa406"
        .parse()
        .unwrap()
}

#[test]
fn the_approval_is_of_swapic_for_everything() {
    let data = approve_calldata(swapic());
    // approve(address,uint256)
    assert_eq!(hex::encode(&data[..4]), "095ea7b3");
    assert_eq!(
        hex::encode(&data[4..36]),
        "000000000000000000000000f0ba68cbc87c31ecaa9654eb14daf3632eaca406"
    );
    assert_eq!(data[36..], [0xff; 32]);
    assert_eq!(data.len(), 68);
}

#[test]
fn the_read_asks_the_canisters_allowance_to_swapic() {
    let canister: EvmAddress = "0x36D34038F04990a58c068e08eD9754e2E3D9bA53"
        .parse()
        .unwrap();
    let data = allowance_calldata(canister, swapic());
    // allowance(address,address)
    assert_eq!(hex::encode(&data[..4]), "dd62ed3e");
    assert_eq!(
        hex::encode(&data[4..36]),
        "00000000000000000000000036d34038f04990a58c068e08ed9754e2e3d9ba53"
    );
    assert_eq!(
        hex::encode(&data[36..]),
        "000000000000000000000000f0ba68cbc87c31ecaa9654eb14daf3632eaca406"
    );
}

#[test]
fn an_allowance_from_half_of_all_up_is_in_place() {
    assert!(in_place(U256::MAX));
    assert!(in_place(U256::from(1) << 255));
    assert!(!in_place((U256::from(1) << 255) - U256::from(1)));
    assert!(!in_place(U256::ZERO));
    assert_eq!(
        allowance_of(&Simulated::Returned(U256::MAX.to_be_bytes::<32>().to_vec())),
        Some(U256::MAX)
    );
    assert_eq!(allowance_of(&Simulated::Returned(vec![0; 31])), None);
    assert_eq!(allowance_of(&Simulated::Reverted("no".to_string())), None);
}

#[test]
fn a_stranger_and_a_halt_are_refused() {
    assert_eq!(checked(true, true), Err(PoolError::Halted));
    assert!(matches!(
        checked(false, false),
        Err(PoolError::NotAllowed(_))
    ));
    assert_eq!(checked(false, true), Ok(()));
}
