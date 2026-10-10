use super::*;

const USDC_BASE: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const USER: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";

/// `cast wallet address --private-key 0xac09...ff80` (anvil's first account, a key every
/// test in the world already knows).
const SIGNER: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

fn address(text: &str) -> EvmAddress {
    text.parse().expect("a fixture address parses")
}

/// The transaction `cast mktx` signed, field for field.
///
/// ```text
/// cast mktx --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
///   --nonce 7 --gas-limit 120000 --gas-price 2000000000 --priority-gas-price 100000000 \
///   --chain 8453 --value 0 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913 \
///   "transfer(address,uint256)" 0x7551A66653f9a20979ed81835a0b7008EC83401b 25000000
/// ```
fn fixture_tx() -> Eip1559Tx {
    Eip1559Tx {
        chain_id: ChainId::BASE,
        nonce: Nonce::new(7),
        max_priority_fee: WeiPerGas::from(100_000_000_u64),
        max_fee: WeiPerGas::from(2_000_000_000_u64),
        gas_limit: GasAmount::from(120_000_u32),
        to: address(USDC_BASE),
        value: Wei::ZERO,
        data: hex::decode(FIXTURE_DATA).expect("the fixture calldata is hex"),
    }
}

const FIXTURE_DATA: &str = "a9059cbb0000000000000000000000007551a66653f9a20979ed81835a0b7008ec83401b00000000000000000000000000000000000000000000000000000000017d7840";

/// The output of the `cast mktx` above.
const FIXTURE_RAW: &str = "02f8b2822105078405f5e10084773594008301d4c094833589fcd6edb6e08f4c7c32d4f71b54bda0291380b844a9059cbb0000000000000000000000007551a66653f9a20979ed81835a0b7008ec83401b00000000000000000000000000000000000000000000000000000000017d7840c080a0065d2f5624c8b0d3235f7435fb9dd16c2bdb1e30ca04e866881d29ef6557599ea05441d6f2ff10f4eb89b7334ed48f4e5b9956e951062cafc99ba2758491b06c06";

/// `cast keccak <the raw bytes above>`.
const FIXTURE_TX_HASH: &str = "56e6181813d491f6a7a3e652737012d3a0c9393a42a75256b6d63f54a9d256c3";

/// The signature inside the raw bytes above: y parity 0, then r and s.
fn fixture_signature() -> EcdsaSignature {
    EcdsaSignature::new(
        hex32("065d2f5624c8b0d3235f7435fb9dd16c2bdb1e30ca04e866881d29ef6557599e"),
        hex32("5441d6f2ff10f4eb89b7334ed48f4e5b9956e951062cafc99ba2758491b06c06"),
        false,
    )
}

fn hex32(text: &str) -> [u8; 32] {
    hex::decode(text)
        .expect("a fixture word is hex")
        .try_into()
        .expect("a fixture word is 32 bytes")
}

/// The address of a key is the keccak rule, and the key here is the one whose address
/// `cast wallet address` prints: `cast wallet public-key --private-key 0xac09...ff80`.
#[test]
fn an_address_is_derived_from_its_public_key() {
    let mut uncompressed = [0u8; 65];
    uncompressed[0] = 0x04;
    hex::decode_to_slice(
        "8318535b54105d4a7aae60c08fc45f9687181b4fdfc625bd1a753fa7397fed7535\
         47f11ca8696646f2f3acb08e31016afac23e630c5d11f59f61fef57b0d2aa5",
        &mut uncompressed[1..],
    )
    .expect("the fixture key is hex");
    assert_eq!(EvmAddress::from_public_key(&uncompressed), address(SIGNER));
}

/// Addresses print in their EIP-55 checksum, whatever case they arrived in, and the
/// checksums here are `cast to-check-sum-address`.
#[test]
fn an_address_prints_in_its_eip_55_checksum() {
    for checksummed in [
        USDC_BASE,
        USER,
        "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
        "0x0000000000000000000000000000000000000000",
        SIGNER,
    ] {
        assert_eq!(address(checksummed).to_string(), checksummed);
        assert_eq!(
            address(&checksummed.to_lowercase()).to_string(),
            checksummed,
            "a lowercase address is not a wrong checksum, it is no checksum"
        );
    }
    assert_eq!(
        EvmAddress::ZERO.to_string(),
        "0x0000000000000000000000000000000000000000"
    );
}

/// A mixed-case address carries a checksum, and one that does not hold is a typo or a
/// tampered address, never an address to pay.
#[test]
fn an_address_with_a_broken_checksum_is_refused() {
    // the same twenty bytes as USDC_BASE, with one letter's case flipped
    let tampered = "0x833589fcD6eDb6E08f4c7C32D4f71b54bdA02913";
    assert_eq!(
        tampered.parse::<EvmAddress>(),
        Err(EvmAddressError::BadChecksum)
    );
}

/// Everything else an address can fail to be.
#[test]
fn text_that_is_not_an_address_is_refused() {
    assert_eq!(
        "0x1234".parse::<EvmAddress>(),
        Err(EvmAddressError::WrongLength { len: 4 })
    );
    assert_eq!(
        "833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".parse::<EvmAddress>(),
        Err(EvmAddressError::NoPrefix)
    );
    assert_eq!(
        "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA0291z".parse::<EvmAddress>(),
        Err(EvmAddressError::NotHex)
    );
}

/// CCTP and the vault carry a recipient as a 32-byte word, left-padded.
#[test]
fn an_address_pads_to_the_thirty_two_byte_word_a_rail_carries() {
    let padded = address(USER).to_word();
    assert_eq!(&padded[..12], &[0; 12]);
    assert_eq!(&padded[12..], address(USER).as_bytes());
}

/// The hash the canister signs is the one `cast` computes over the same transaction:
/// `cast keccak 0x02f86f8221050784...c0`, the unsigned envelope.
#[test]
fn the_signing_hash_is_the_one_cast_signs() {
    assert_eq!(
        fixture_tx().signing_hash().to_string(),
        "f8e4ebc000523733a7f3eea5dc736252ded44721f18072b542e24234dd0e5cfa"
    );
}

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/golden/evm_tx_v1.txt");

/// The envelope a chain accepts is a consensus artifact like the quote hash: a canister
/// that encodes it differently signs a different transaction, and a vault that already
/// trusts an address would see a stranger. Two lines: the unsigned envelope, so a layout
/// change says what moved, and the hash that is signed.
#[test]
fn the_unsigned_envelope_matches_the_golden_file() {
    let tx = fixture_tx();
    let got = format!(
        "{}\n{}\n",
        hex::encode(tx.signing_payload()),
        tx.signing_hash()
    );

    // regeneration is opt-in and never green, so a blessing is always a deliberate diff
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(GOLDEN, &got).unwrap();
        panic!("golden regenerated, inspect the diff and rerun: {GOLDEN}");
    }
    let want = std::fs::read_to_string(GOLDEN).unwrap_or_else(|e| {
        panic!("golden missing or unreadable ({e}); regenerate with UPDATE_GOLDEN=1: {GOLDEN}")
    });
    assert_eq!(got, want, "the eip-1559 envelope changed: breaking");
}

/// The raw bytes the canister broadcasts are the bytes `cast mktx` produced for the same
/// transaction and the same signature, and the hash is the hash of those bytes.
#[test]
fn a_signed_transaction_is_the_raw_cast_produced() {
    let signed = fixture_tx().signed(fixture_signature());
    assert_eq!(hex::encode(signed.raw()), FIXTURE_RAW);
    assert_eq!(signed.hash().to_string(), FIXTURE_TX_HASH);
}

/// Every field is in the preimage, so moving any of them moves the hash.
#[test]
fn the_signing_hash_changes_with_every_field() {
    let base = fixture_tx().signing_hash();
    let moved = [
        Eip1559Tx {
            chain_id: ChainId::ARBITRUM,
            ..fixture_tx()
        },
        Eip1559Tx {
            nonce: Nonce::new(8),
            ..fixture_tx()
        },
        Eip1559Tx {
            max_fee: WeiPerGas::from(2_000_000_001_u64),
            ..fixture_tx()
        },
        Eip1559Tx {
            max_priority_fee: WeiPerGas::from(100_000_001_u64),
            ..fixture_tx()
        },
        Eip1559Tx {
            gas_limit: GasAmount::from(120_001_u32),
            ..fixture_tx()
        },
        Eip1559Tx {
            to: address(USER),
            ..fixture_tx()
        },
        Eip1559Tx {
            value: Wei::ONE,
            ..fixture_tx()
        },
        Eip1559Tx {
            data: vec![],
            ..fixture_tx()
        },
    ];
    for tx in moved {
        assert_ne!(tx.signing_hash(), base, "{tx:?}");
    }
}

/// A large value and a large fee are 256-bit numbers in the envelope, written without
/// leading zeros like every other RLP integer.
#[test]
fn a_transaction_carries_full_width_amounts() {
    let tx = Eip1559Tx {
        value: Wei::MAX,
        max_fee: WeiPerGas::MAX,
        ..fixture_tx()
    };
    // it hashes rather than panicking, and it differs from the small one
    assert_ne!(tx.signing_hash(), fixture_tx().signing_hash());
}

/// The parity is the one bit of the signature that is not r or s, and it is in the raw
/// bytes: a transaction signed with the other parity recovers to another address, so it
/// must not encode the same.
#[test]
fn the_parity_is_part_of_the_raw_bytes() {
    let tx = fixture_tx();
    let even = tx.signed(fixture_signature());
    let odd = tx.signed(EcdsaSignature::new(
        *fixture_signature().r(),
        *fixture_signature().s(),
        true,
    ));
    assert_ne!(even.raw(), odd.raw());
    assert_ne!(even.hash(), odd.hash());
}
