//! Threshold ECDSA against pocket-ic's test keys.
//!
//! The suite's other instances have no threshold key at all, which is what makes the
//! failure path here testable: a canister on a subnet whose network holds no
//! `dfx_test_key` gets a rejected management call, and that is exactly the shape a
//! signing subnet under load has.

use crate::client::settlement::{
    derive_evm_address, evm_address, get_config_full, set_config, test_sign,
};
use crate::settlement_suite::init::{init_arg, install, setup, upgrade};
use candid::Principal;
use libsecp256k1::{recover, Message, RecoveryId, Signature};
use pocket_ic::{PocketIc, PocketIcBuilder};
use settlement_api::types::config::{Config, ConfigError};
use settlement_api::types::errors::{SetConfigError, SignError};
use settlement_api::types::evm::EcdsaError;
use sha2::Digest;

/// (pic, canister, admin) on a network that holds pocket-ic's test threshold keys.
fn setup_with_keys() -> (PocketIc, Principal, Principal) {
    let pic = PocketIcBuilder::new()
        .with_ii_subnet()
        .with_application_subnet()
        .build();
    let admin = Principal::from_slice(&[1; 29]);
    let subnet = pic.topology().get_app_subnets()[0];
    let canister = pic.create_canister_on_subnet(Some(admin), None, subnet);
    pic.add_cycles(canister, 100_000_000_000_000);
    install(&pic, canister, admin, &init_arg()).expect("the default arg installs");
    (pic, canister, admin)
}

/// A hash to sign that is not a round number.
fn digest(seed: &[u8]) -> [u8; 32] {
    sha2::Sha256::digest(seed).into()
}

/// The address the twenty low bytes of the keccak of an uncompressed public key make,
/// computed here rather than asked of the canister.
fn address_of(key: &libsecp256k1::PublicKey) -> String {
    let uncompressed = key.serialize();
    // keccak-256, which is NOT sha3-256: they differ in their padding, and an address
    // derived with the other one belongs to nobody
    let hash: [u8; 32] = alloy_primitives::keccak256(&uncompressed[1..]).into();
    let mut address = [0u8; 20];
    address.copy_from_slice(&hash[12..]);
    format!("0x{}", hex::encode(address))
}

/// The point of the parity trial: a signature the canister reports must recover, with the
/// parity it reports, to the canister's own address. Recovered here from the raw r, s and
/// parity, with no help from the canister beyond those three values.
#[test]
fn a_signature_recovers_to_the_canisters_own_address() {
    let (pic, canister, admin) = setup_with_keys();
    assert_eq!(
        evm_address(&pic, canister, admin),
        None,
        "nothing is derived until something asks"
    );

    let mine = derive_evm_address(&pic, canister, admin).expect("the test key derives an address");
    assert_eq!(
        evm_address(&pic, canister, admin),
        Some(mine.clone()),
        "the derived address is cached"
    );

    let hash = digest(b"swapic signing test");
    let signature = test_sign(&pic, canister, admin, hash).expect("the test key signs");
    let mut raw = [0u8; 64];
    raw[..32].copy_from_slice(&signature.r);
    raw[32..].copy_from_slice(&signature.s);
    let recovered = recover(
        &Message::parse(&hash),
        &Signature::parse_standard_slice(&raw).expect("the signature parses"),
        &RecoveryId::parse(u8::from(signature.y_parity)).expect("the parity is 0 or 1"),
    )
    .expect("the signature recovers a key");
    assert_eq!(
        address_of(&recovered),
        mine.to_lowercase(),
        "the parity the canister reported is the one that recovers to it"
    );

    // s is in the lower half of the range, which is what every EVM chain accepts
    let half_order = {
        let mut half = [0u8; 32];
        hex::decode_to_slice(
            "7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0",
            &mut half,
        )
        .unwrap();
        half
    };
    assert!(signature.s <= half_order, "s is normalised low");
}

/// The address is derived once and kept in stable memory, so an upgrade does not spend
/// another derivation, and it cannot come back different.
#[test]
fn the_cached_address_survives_an_upgrade() {
    let (pic, canister, admin) = setup_with_keys();
    let mine = derive_evm_address(&pic, canister, admin).expect("the test key derives an address");
    upgrade(&pic, canister, admin).expect("the upgrade goes through");
    assert_eq!(evm_address(&pic, canister, admin), Some(mine.clone()));
    assert_eq!(
        derive_evm_address(&pic, canister, admin),
        Ok(mine),
        "a second derivation answers the cached address"
    );
}

/// A network with no threshold key rejects the management call, and the canister answers
/// the refusal rather than trapping: the same shape a signing subnet refusing under load
/// has, and the one a retry must not treat as a signature that never existed.
///
/// Signing derives the address first, so a canister with no key at all is refused at the
/// key rather than at the signature, and the error says which of the two calls failed
/// instead of flattening both into one.
#[test]
fn a_signature_the_management_canister_refuses_is_an_error_and_not_a_trap() {
    let (pic, canister, admin) = setup();
    let answer = test_sign(&pic, canister, admin, digest(b"no key here"));
    assert!(
        matches!(answer, Err(SignError::Ecdsa(EcdsaError::PublicKey { .. }))),
        "{answer:?}"
    );
    let answer = derive_evm_address(&pic, canister, admin);
    assert!(
        matches!(answer, Err(SignError::Ecdsa(EcdsaError::PublicKey { .. }))),
        "{answer:?}"
    );
    assert_eq!(
        evm_address(&pic, canister, admin),
        None,
        "nothing was cached"
    );
}

/// Both doors are the controller's.
#[test]
fn the_signing_doors_refuse_a_stranger() {
    let (pic, canister, _admin) = setup();
    let stranger = Principal::from_slice(&[9; 29]);
    assert!(matches!(
        derive_evm_address(&pic, canister, stranger),
        Err(SignError::Guard(_))
    ));
    assert!(matches!(
        test_sign(&pic, canister, stranger, digest(b"stranger")),
        Err(SignError::Guard(_))
    ));
}

/// The key name is deploy-time truth that stops being editable the moment it has been
/// acted on. Every vault on every chain is configured to obey the address derived from the
/// named key, and the canister's gas account is that address, so a controller who changes
/// the name afterwards would leave the canister signing under a key nothing on any chain
/// recognises. The write is refused, by name, on the wire.
#[test]
fn the_key_name_cannot_move_once_the_address_is_derived() {
    let (pic, canister, admin) = setup_with_keys();
    let before = get_config_full(&pic, canister, admin).expect("a controller may read it");
    assert_eq!(before.ecdsa_key_name, "dfx_test_key");

    // a name change before anything has been derived is an ordinary config write
    let renamed = Config {
        ecdsa_key_name: "key_1".to_string(),
        ..before.clone()
    };
    assert_eq!(set_config(&pic, canister, admin, &renamed), Ok(()));
    assert_eq!(set_config(&pic, canister, admin, &before), Ok(()));

    derive_evm_address(&pic, canister, admin).expect("the test key derives an address");
    let address = evm_address(&pic, canister, Principal::anonymous());
    assert!(address.is_some());

    assert_eq!(
        set_config(&pic, canister, admin, &renamed),
        Err(SetConfigError::InvalidConfig(
            ConfigError::EcdsaKeyNameFixed {
                current: "dfx_test_key".to_string(),
                requested: "key_1".to_string(),
            }
        )),
        "the address is derived, so the name it was derived under is fixed"
    );
    assert_eq!(
        get_config_full(&pic, canister, admin)
            .expect("a controller may read it")
            .ecdsa_key_name,
        "dfx_test_key",
        "a refused write leaves the config alone"
    );
    assert_eq!(
        evm_address(&pic, canister, Principal::anonymous()),
        address,
        "and leaves the address alone"
    );

    // every other knob still moves
    let other_knob = Config {
        max_batch_items: 25,
        ..before
    };
    assert_eq!(set_config(&pic, canister, admin, &other_knob), Ok(()));
}
