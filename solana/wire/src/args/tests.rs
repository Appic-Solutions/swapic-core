use super::*;

fn swap<'a>(
    ref_bytes: &'a [u8; 32],
    token: &'a [u8; 20],
    min_out: &'a [u8],
    jupiter: &'a [u8],
) -> BurnArgs<'a> {
    BurnArgs {
        tag: TAG_SWAP_BURN,
        fast: true,
        event_keypair: false,
        float: true,
        domain: 6,
        ref_source: RefSource::Direct(ref_bytes),
        deadline: 1_800_000_000,
        recipient: &[0x11; 20],
        payload: Payload::Swap {
            token_out: token,
            min_out,
        },
        min_gas: 709_154,
        max_fee_deci_bps: 13,
        amount: 890_820_000,
        fee: 50_000,
        jupiter,
    }
}

#[test]
fn a_swap_with_an_8_byte_min_out_takes_106_bytes() {
    let min_out = 1_000_000u64.to_be_bytes();
    let args = swap(&[7; 32], &[2; 20], &min_out, &[]);
    let bytes = args.encode();
    assert_eq!(bytes.len(), 106);
    assert_eq!(bytes[0], TAG_SWAP_BURN);
    // action 1, fast, float
    assert_eq!(bytes[1], 0b0010_0101);
    assert_eq!(bytes[2], 6);
    assert_eq!(&bytes[3..35], &[7; 32]);
    assert_eq!(
        u32::from_le_bytes(bytes[35..39].try_into().unwrap()),
        1_800_000_000
    );
    assert_eq!(&bytes[39..59], &[0x11; 20]);
    assert_eq!(&bytes[59..79], &[2; 20]);
    assert_eq!(bytes[79], 8);
    assert_eq!(&bytes[80..88], &min_out);
    assert_eq!(BurnArgs::decode(&bytes), Ok(args));
}

#[test]
fn the_jupiter_data_is_passed_through_unchanged() {
    let min_out = [9u8];
    let args = swap(&[7; 32], &[2; 20], &min_out, &[0xbb, 0x64, 1, 2, 3]);
    let bytes = args.encode();
    let decoded = BurnArgs::decode(&bytes).unwrap();
    assert_eq!(decoded.jupiter, &[0xbb, 0x64, 1, 2, 3]);
}

#[test]
fn a_deliver_with_a_memo_derives_its_ref_from_the_memo_and_has_no_ref_bytes() {
    let memo = [0x42u8; 180];
    let args = BurnArgs {
        tag: TAG_SWAP_BURN,
        fast: true,
        event_keypair: false,
        float: false,
        domain: 6,
        ref_source: RefSource::Memo(&memo),
        deadline: 1_800_000_000,
        recipient: &[0x11; 20],
        payload: Payload::Deliver,
        min_gas: 0,
        max_fee_deci_bps: 13,
        amount: 1,
        fee: 2,
        jupiter: &[],
    };
    let bytes = args.encode();
    // the memo replaces the 32-byte ref: 1 byte of length plus the memo
    assert_eq!(bytes.len(), 3 + 1 + 180 + 4 + 20 + 4 + 2 + 8 + 4);
    assert_eq!(bytes[1] & (1 << 6), 1 << 6);
    assert_eq!(BurnArgs::decode(&bytes), Ok(args));
}

#[test]
fn a_ck_deposit_carries_its_principal_and_optional_subaccount() {
    let principal = [8u8; 32];
    let sub = [9u8; 32];
    let mut args = BurnArgs {
        tag: TAG_BURN,
        fast: false,
        event_keypair: false,
        float: false,
        domain: 0,
        ref_source: RefSource::Direct(&[1; 32]),
        deadline: 1_800_000_000,
        recipient: &[0x11; 20],
        payload: Payload::CkDeposit {
            principal: &principal,
            subaccount: None,
        },
        min_gas: 1,
        max_fee_deci_bps: 0,
        amount: 5,
        fee: 0,
        jupiter: &[],
    };
    assert_eq!(BurnArgs::decode(&args.encode()), Ok(args));
    args.payload = Payload::CkDeposit {
        principal: &principal,
        subaccount: Some(&sub),
    };
    let bytes = args.encode();
    assert_eq!(BurnArgs::decode(&bytes), Ok(args));
    let envelope = args.envelope(&[1; 32]);
    assert_eq!(envelope.principal, principal);
    assert_eq!(envelope.subaccount, sub);
    assert_eq!(envelope.min_out, [0; 32]);
    assert_eq!(envelope.token_out, [0; 20]);
}

#[test]
fn the_envelope_right_aligns_min_out_and_widens_the_narrow_fields() {
    let min_out = [0x12u8, 0x34];
    let args = swap(&[7; 32], &[2; 20], &min_out, &[]);
    let envelope = args.envelope(&[7; 32]);
    assert_eq!(envelope.min_out[30..], [0x12, 0x34]);
    assert_eq!(envelope.min_out[..30], [0u8; 30]);
    assert_eq!(envelope.deadline, 1_800_000_000);
    assert_eq!(envelope.min_gas, 709_154);
}

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

fn corrupted(change: impl FnOnce(&mut Vec<u8>)) -> Result<(), SwapicError> {
    let min_out = 1_000_000u64.to_be_bytes();
    let mut bytes = swap(&[7; 32], &[2; 20], &min_out, &[]).encode();
    change(&mut bytes);
    BurnArgs::decode(&bytes).map(|_| ())
}

#[test]
fn decode_refuses_every_malformed_burn() {
    assert_eq!(
        corrupted(|b| b[1] |= 1 << 7),
        Err(SwapicError::ReservedFlags)
    );
    assert_eq!(
        corrupted(|b| b[1] = (b[1] & !3) | 3),
        Err(SwapicError::BadAction)
    );
    // a memo on a Swap
    assert_eq!(
        corrupted(|b| b[1] |= 1 << 6),
        Err(SwapicError::MemoWithoutDeliver)
    );
    // a subaccount flag on a Swap
    assert_eq!(
        corrupted(|b| b[1] |= 1 << 3),
        Err(SwapicError::ReservedFlags)
    );
    // recipient zero
    assert_eq!(
        corrupted(|b| b[39..59].fill(0)),
        Err(SwapicError::BadRecipient)
    );
    // tokenOut zero
    assert_eq!(
        corrupted(|b| b[59..79].fill(0)),
        Err(SwapicError::BadTokenOutOrMinOut)
    );
    // minOut length 0, then 33
    assert_eq!(
        corrupted(|b| b[79] = 0),
        Err(SwapicError::BadTokenOutOrMinOut)
    );
    assert_eq!(
        corrupted(|b| b[79] = 33),
        Err(SwapicError::BadTokenOutOrMinOut)
    );
    // minOut zero
    assert_eq!(
        corrupted(|b| b[80..88].fill(0)),
        Err(SwapicError::BadTokenOutOrMinOut)
    );
    // minGas zero on a Swap
    assert_eq!(
        corrupted(|b| b[88..92].fill(0)),
        Err(SwapicError::BadMinGas)
    );
    // short
    assert_eq!(corrupted(|b| b.truncate(100)), Err(SwapicError::ShortData));
    assert_eq!(BurnArgs::decode(&[]), Err(SwapicError::ShortData));
    // another tag
    assert_eq!(corrupted(|b| b[0] = 3), Err(SwapicError::BadAction));
    // `burn` takes no trailing bytes
    assert_eq!(
        corrupted(|b| {
            b[0] = TAG_BURN;
            b.push(1);
        }),
        Err(SwapicError::ShortData)
    );
}

#[test]
fn a_deliver_with_min_gas_is_refused() {
    let args = BurnArgs {
        tag: TAG_SWAP_BURN,
        fast: true,
        event_keypair: false,
        float: false,
        domain: 6,
        ref_source: RefSource::Direct(&[1; 32]),
        deadline: 1,
        recipient: &[1; 20],
        payload: Payload::Deliver,
        min_gas: 1,
        max_fee_deci_bps: 0,
        amount: 1,
        fee: 0,
        jupiter: &[],
    };
    assert_eq!(
        BurnArgs::decode(&args.encode()),
        Err(SwapicError::BadMinGas)
    );
}

#[test]
fn an_empty_memo_is_refused() {
    let mut bytes = vec![TAG_SWAP_BURN, 1 << 6, 6, 0];
    bytes.extend_from_slice(&[0; 60]);
    assert_eq!(BurnArgs::decode(&bytes), Err(SwapicError::BadMemoLength));
}

#[test]
fn arrival_args_round_trip_and_bound_the_attestation() {
    let message = [1u8; 486];
    let attestation = [2u8; 130];
    let args = ArrivalArgs {
        tag: TAG_COMPLETE,
        message: &message,
        attestation: &attestation,
        jupiter: &[9, 9],
    };
    let bytes = args.encode();
    assert_eq!(ArrivalArgs::decode(&bytes), Ok(args));

    let mut bad = bytes.clone();
    bad[3 + 486] = 64; // not a multiple of 65
    assert_eq!(
        ArrivalArgs::decode(&bad),
        Err(SwapicError::BadMessageLength)
    );
    bad[3 + 486] = 0;
    assert_eq!(
        ArrivalArgs::decode(&bad),
        Err(SwapicError::BadMessageLength)
    );

    // release takes nothing after the attestation
    let release = ArrivalArgs {
        tag: TAG_RELEASE,
        message: &message,
        attestation: &attestation,
        jupiter: &[],
    };
    let mut bytes = release.encode();
    assert_eq!(ArrivalArgs::decode(&bytes), Ok(release));
    bytes.push(0);
    assert_eq!(ArrivalArgs::decode(&bytes), Err(SwapicError::ShortData));
}

#[test]
fn reclaim_args_round_trip_exactly() {
    let args = ReclaimArgs {
        attestation: &[1; 130],
        destination_message: &[2; 696],
    };
    let mut bytes = args.encode();
    assert_eq!(ReclaimArgs::decode(&bytes), Ok(args));
    bytes.push(0);
    assert_eq!(ReclaimArgs::decode(&bytes), Err(SwapicError::ShortData));
}

fn config() -> Config {
    Config {
        paused: false,
        event_keypair_allowed: true,
        bump: 254,
        swap_domains: 0b1000_0000_0100_0100_1100_1101,
        ck_domains: 1,
        min_burn: 1_000_000,
        min_deadline_s: 600,
        max_deadline_s: 7200,
    }
}

#[test]
fn config_has_the_design_layout_and_masks() {
    let bytes = config().encode();
    assert_eq!(bytes.len(), 40);
    assert_eq!(bytes[0], 1);
    assert_eq!(bytes[2], 1);
    assert_eq!(bytes[3], 254);
    assert_eq!(
        u64::from_le_bytes(bytes[20..28].try_into().unwrap()),
        1_000_000
    );
    assert_eq!(u32::from_le_bytes(bytes[28..32].try_into().unwrap()), 600);
    assert_eq!(u32::from_le_bytes(bytes[32..36].try_into().unwrap()), 7200);
    assert_eq!(&bytes[36..], &[0; 4]);
    assert_eq!(Config::parse(&bytes), Ok(config()));
    assert!(config().allows_swap_to(0));
    assert!(!config().allows_swap_to(1));
    assert!(config().allows_ck_to(0));
    assert!(!config().allows_ck_to(6));
    assert!(!config().allows_swap_to(64));
}

#[test]
fn set_config_refuses_a_bad_window_and_a_ck_domain_other_than_ethereum() {
    let data = encode_set_config(&config());
    assert_eq!(data.len(), 40);
    assert_eq!(decode_set_config(&data), Ok(config()));

    let mut bad = config();
    bad.min_deadline_s = bad.max_deadline_s;
    assert_eq!(
        decode_set_config(&encode_set_config(&bad)),
        Err(SwapicError::DeadlineOutOfWindow)
    );

    let mut bad = config();
    bad.ck_domains = 1 << 6;
    assert_eq!(
        decode_set_config(&encode_set_config(&bad)),
        Err(SwapicError::DomainNotAllowed)
    );

    assert_eq!(decode_set_config(&data[..39]), Err(SwapicError::ShortData));
}
