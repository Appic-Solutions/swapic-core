use super::*;
use crate::evm::EvmAddress;

const USDC_BASE: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const VAULT_BASE: &str = "0x1111111111111111111111111111111111111111";
const VAULT_ARBITRUM: &str = "0x2222222222222222222222222222222222222222";
const MESSENGER: &str = "0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d";
const MINE: &str = "0x7551A66653f9a20979ed81835a0b7008EC83401b";

fn word(address: &str) -> [u8; 32] {
    address.parse::<EvmAddress>().unwrap().to_word()
}

/// A burn message as Circle attests one of this canister's burns from Base to Arbitrum:
/// the nonce, the fee executed and the expiration block filled in by the attestation
/// service, everything else as the burn emitted it.
fn attested() -> BurnMessage {
    BurnMessage {
        version: MESSAGE_VERSION,
        source_domain: 6,
        destination_domain: 3,
        nonce: [0x9a; 32],
        sender: word(MESSENGER),
        recipient: word(MESSENGER),
        destination_caller: word(MINE),
        min_finality_threshold: 1_000,
        finality_threshold_executed: 1_000,
        body: BurnBody {
            version: BURN_BODY_VERSION,
            burn_token: word(USDC_BASE),
            mint_recipient: word(VAULT_ARBITRUM),
            amount: TokenAmount::from(25_000_000_u32),
            message_sender: word(VAULT_BASE),
            max_fee: TokenAmount::from(5_000_u32),
            fee_executed: TokenAmount::from(2_500_u32),
            expiration_block: BlockNumber::new(19_000_100),
            hook_data: vec![],
        },
    }
}

/// A burn message is 376 bytes, a 148-byte header and a 228-byte burn body, each field at
/// the offset Circle's `MessageV2` and `BurnMessageV2` libraries declare: uint32s
/// left-padded in four bytes, words in thirty-two, and the amounts as 256-bit integers.
/// The layout is pinned byte by byte, so a decoder that read a field at another offset
/// would bind the wrong value to the swap.
#[test]
fn a_burn_message_is_laid_out_as_circles_libraries_declare() {
    let bytes = attested().encode();
    assert_eq!(bytes.len(), HEADER_BYTES + BURN_BODY_BYTES);
    assert_eq!(bytes.len(), 376);
    assert_eq!(&bytes[0..4], &[0, 0, 0, 1], "version at 0");
    assert_eq!(&bytes[4..8], &[0, 0, 0, 6], "source domain at 4");
    assert_eq!(&bytes[8..12], &[0, 0, 0, 3], "destination domain at 8");
    assert_eq!(&bytes[12..44], &[0x9a; 32], "nonce at 12");
    assert_eq!(&bytes[44..76], &word(MESSENGER), "sender at 44");
    assert_eq!(&bytes[76..108], &word(MESSENGER), "recipient at 76");
    assert_eq!(&bytes[108..140], &word(MINE), "destination caller at 108");
    assert_eq!(
        &bytes[140..144],
        &[0, 0, 0x03, 0xe8],
        "min finality threshold at 140"
    );
    assert_eq!(
        &bytes[144..148],
        &[0, 0, 0x03, 0xe8],
        "finality threshold executed at 144"
    );
    let body = &bytes[HEADER_BYTES..];
    assert_eq!(&body[0..4], &[0, 0, 0, 1], "body version at 0");
    assert_eq!(&body[4..36], &word(USDC_BASE), "burn token at 4");
    assert_eq!(&body[36..68], &word(VAULT_ARBITRUM), "mint recipient at 36");
    assert_eq!(
        &body[68..100],
        &TokenAmount::from(25_000_000_u32).to_be_bytes(),
        "amount at 68"
    );
    assert_eq!(&body[100..132], &word(VAULT_BASE), "message sender at 100");
    assert_eq!(
        &body[132..164],
        &TokenAmount::from(5_000_u32).to_be_bytes(),
        "max fee at 132"
    );
    assert_eq!(
        &body[164..196],
        &TokenAmount::from(2_500_u32).to_be_bytes(),
        "fee executed at 164"
    );
    assert_eq!(
        &body[196..228],
        &TokenAmount::from(19_000_100_u32).to_be_bytes()
    );
    assert_eq!(BurnMessage::parse(&bytes), Ok(attested()));
}

/// What the codec writes it reads, hook data included, and a message that is not a burn
/// message is refused by the way it is not: too short for a header, too short for a burn
/// body, another header or body version, or an expiration block no chain has.
#[test]
fn a_burn_message_round_trips_and_a_malformed_one_is_refused_by_name() {
    let with_hook = BurnMessage {
        body: BurnBody {
            hook_data: vec![0xde, 0xad, 0xbe, 0xef],
            ..attested().body
        },
        ..attested()
    };
    let bytes = with_hook.encode();
    assert_eq!(bytes.len(), 380);
    assert_eq!(BurnMessage::parse(&bytes), Ok(with_hook));

    assert_eq!(
        BurnMessage::parse(&bytes[..100]),
        Err(MessageError::TooShort {
            len: 100,
            wanted: HEADER_BYTES
        })
    );
    assert_eq!(
        BurnMessage::parse(&bytes[..300]),
        Err(MessageError::TooShort {
            len: 300,
            wanted: HEADER_BYTES + BURN_BODY_BYTES
        })
    );
    let mut other_version = attested().encode();
    other_version[3] = 2;
    assert_eq!(
        BurnMessage::parse(&other_version),
        Err(MessageError::UnknownVersion { version: 2 })
    );
    let mut other_body = attested().encode();
    other_body[HEADER_BYTES + 3] = 0;
    assert_eq!(
        BurnMessage::parse(&other_body),
        Err(MessageError::UnknownBodyVersion { version: 0 })
    );
    let mut far_block = attested().encode();
    far_block[HEADER_BYTES + 196] = 1;
    assert_eq!(
        BurnMessage::parse(&far_block),
        Err(MessageError::ExpirationBlockTooLarge)
    );
    // an expiration block of zero is Circle's "never", and reads as block zero
    let mut never = attested().encode();
    never[HEADER_BYTES + 196..HEADER_BYTES + 228].fill(0);
    assert_eq!(
        BurnMessage::parse(&never).unwrap().body.expiration_block,
        BlockNumber::new(0)
    );
}
