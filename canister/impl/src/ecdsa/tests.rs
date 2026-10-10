use super::*;

/// Anvil's first account, whose key every test in the world already knows:
/// `cast wallet address --private-key 0xac09...ff80`.
const SIGNER: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

/// The signature inside the `cast mktx` fixture pinned in `types/src/evm/tests.rs`, over
/// the signing hash of that same transaction. Parity 0, s already in the lower half.
const FIXTURE_HASH: &str = "f8e4ebc000523733a7f3eea5dc736252ded44721f18072b542e24234dd0e5cfa";
const FIXTURE_R: &str = "065d2f5624c8b0d3235f7435fb9dd16c2bdb1e30ca04e866881d29ef6557599e";
const FIXTURE_S: &str = "5441d6f2ff10f4eb89b7334ed48f4e5b9956e951062cafc99ba2758491b06c06";
/// The same signature with s reflected to the upper half: `n - s`, which is the other
/// valid encoding of the same signature and the one no EVM chain accepts.
const FIXTURE_HIGH_S: &str = "abbe290d00ef0b147648ccb12b70b1a32157f395a91bf072242fe9083e85d53b";

fn word(text: &str) -> [u8; 32] {
    hex::decode(text)
        .expect("a fixture word is hex")
        .try_into()
        .expect("a fixture word is 32 bytes")
}

fn raw(r: &str, s: &str) -> [u8; 64] {
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&word(r));
    out[32..].copy_from_slice(&word(s));
    out
}

fn signer() -> EvmAddress {
    SIGNER.parse().expect("the fixture address parses")
}

fn hash() -> TxHash {
    TxHash::new(word(FIXTURE_HASH))
}

/// An EVM address is the twenty low bytes of the keccak of the uncompressed public key
/// without its prefix byte, and the key here is the one that signed the pinned fixture.
#[test]
fn a_public_key_derives_the_address_its_signatures_recover_to() {
    let key = recover_key(&raw(FIXTURE_R, FIXTURE_S), hash(), false)
        .expect("the fixture signature recovers");
    assert_eq!(address_of(&key), signer());
}

/// The parity is not in the signature the threshold signer answers, so it is found by
/// trying both and keeping the one that recovers to the address the canister already
/// knows is its own.
#[test]
fn the_parity_is_the_one_that_recovers_to_our_own_address() {
    let signature = signature_for(&raw(FIXTURE_R, FIXTURE_S), hash(), signer())
        .expect("one parity recovers to the signer");
    assert_eq!(signature.r(), &word(FIXTURE_R));
    assert_eq!(signature.s(), &word(FIXTURE_S));
    assert!(!signature.y_parity(), "the fixture's parity is 0");
}

/// A high s is the other encoding of the same signature, and the one no EVM chain accepts
/// (EIP-2). It is reflected back into the lower half, and the parity that is then right is
/// the one the trial finds, so nothing has to track the flip.
#[test]
fn a_high_s_signature_is_reflected_into_the_lower_half() {
    let signature = signature_for(&raw(FIXTURE_R, FIXTURE_HIGH_S), hash(), signer())
        .expect("the reflected signature is the fixture signature");
    assert_eq!(signature.r(), &word(FIXTURE_R));
    assert_eq!(
        signature.s(),
        &word(FIXTURE_S),
        "s came back into the lower half"
    );
    assert!(!signature.y_parity());
}

/// A signature that recovers to somebody else is not this canister's signature, and a
/// transaction built on it would be sent from a stranger's address.
#[test]
fn a_signature_that_recovers_to_another_address_is_refused() {
    let stranger: EvmAddress = "0x7551A66653f9a20979ed81835a0b7008EC83401b"
        .parse()
        .unwrap();
    assert_eq!(
        signature_for(&raw(FIXTURE_R, FIXTURE_S), hash(), stranger),
        Err(EcdsaError::NoParityRecovers)
    );
}

/// Bytes the management canister could not have answered are refused by name rather than
/// trapping inside the curve library.
#[test]
fn bytes_that_are_not_a_signature_or_a_key_are_refused() {
    assert_eq!(
        signature_for(&[0xff; 64], hash(), signer()),
        Err(EcdsaError::UnreadableSignature)
    );
    assert!(matches!(
        public_key_of(&[1, 2, 3]),
        Err(EcdsaError::UnreadablePublicKey)
    ));
    // a well-formed compressed key is 33 bytes and is read
    let key = recover_key(&raw(FIXTURE_R, FIXTURE_S), hash(), false).unwrap();
    assert_eq!(
        public_key_of(&key.serialize_compressed()).map(|key| address_of(&key)),
        Ok(signer())
    );
}
