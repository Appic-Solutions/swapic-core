use super::*;

fn sample() -> Envelope {
    let mut min_out = [0u8; 32];
    min_out[24..].copy_from_slice(&1_000_000u64.to_be_bytes());
    Envelope {
        ref_bytes: [7; 32],
        action: Action::Swap,
        deadline: 1_800_000_000,
        recipient: [1; 20],
        token_out: [2; 20],
        min_out,
        principal: [0; 32],
        subaccount: [0; 32],
        min_gas: 600_000,
    }
}

#[test]
fn envelope_round_trips_with_the_solidity_word_layout() {
    let bytes = sample().encode();
    assert_eq!(bytes.len(), 320);
    assert_eq!(bytes[31], 3);
    assert_eq!(&bytes[32..64], &[7; 32]);
    assert_eq!(bytes[95], 1);
    assert_eq!(
        u64::from_be_bytes(bytes[120..128].try_into().unwrap()),
        1_800_000_000
    );
    assert_eq!(&bytes[140..160], &[1; 20]);
    assert_eq!(&bytes[172..192], &[2; 20]);
    assert_eq!(
        u64::from_be_bytes(bytes[312..320].try_into().unwrap()),
        600_000
    );
    assert_eq!(Envelope::parse(&bytes), Ok(sample()));
}

#[test]
fn envelope_parse_refuses_dirty_narrow_words() {
    for dirty in [0usize, 30, 64, 94, 96, 119, 128, 139, 160, 171, 288, 311] {
        let mut bytes = sample().encode();
        bytes[dirty] = 1;
        assert_eq!(
            Envelope::parse(&bytes),
            Err(SwapicError::BadHook),
            "byte {dirty}"
        );
    }
    let mut bad_action = sample().encode();
    bad_action[95] = 3;
    assert_eq!(Envelope::parse(&bad_action), Err(SwapicError::BadHook));
    let mut bad_version = sample().encode();
    bad_version[31] = 2;
    assert_eq!(Envelope::parse(&bad_version), Err(SwapicError::BadHook));
    assert_eq!(
        Envelope::parse(&sample().encode()[..319]),
        Err(SwapicError::BadHook)
    );
}

fn solana() -> SolanaEnvelope {
    SolanaEnvelope {
        ref_bytes: [9; 32],
        action: SOLANA_ACTION_SWAP,
        deadline: 1_800_000_000,
        recipient: [3; 32],
        mint_out: [4; 32],
        min_out: 42,
    }
}

#[test]
fn solana_envelope_round_trips_and_is_strict_on_length() {
    let bytes = solana().encode();
    assert_eq!(bytes.len(), 110);
    assert_eq!(SolanaEnvelope::parse(&bytes), Ok(solana()));
    assert_eq!(
        SolanaEnvelope::parse(&bytes[..109]),
        Err(SwapicError::BadHook)
    );
    let mut long = [0u8; 111];
    long[..110].copy_from_slice(&bytes);
    assert_eq!(SolanaEnvelope::parse(&long), Err(SwapicError::BadHook));
}

#[test]
fn release_reads_only_version_deadline_and_recipient() {
    let bytes = solana().encode();
    // one byte short of a whole envelope is still readable: a hand-edited order's money must leave
    let terms = read_release_terms(&bytes[..109]).unwrap();
    assert_eq!(terms.recipient, [3; 32]);
    assert_eq!(terms.deadline, 1_800_000_000);
    assert!(read_release_terms(&bytes[..70]).is_some());
    assert!(read_release_terms(&bytes[..69]).is_none());
    let mut other_version = bytes;
    other_version[0] = 2;
    assert!(read_release_terms(&other_version).is_none());
    assert!(read_release_terms(&[]).is_none());
}

#[test]
fn forwarder_hooks_match_the_measured_bytes() {
    let mut out = [0u8; 65];
    let n = ForwarderHook::Existing.write(&mut out);
    assert_eq!(n, 32);
    assert_eq!(&out[..12], b"cctp-forward");
    assert_eq!(
        ForwarderHook::parse(&out[..n]),
        Some(ForwarderHook::Existing)
    );

    let n = ForwarderHook::CreateFor([5; 32]).write(&mut out);
    assert_eq!(n, 65);
    // from a real message: the magic, then zeroes, then 0x21 and the flag
    assert_eq!(out[31], 0x21);
    assert_eq!(out[32], 0x01);
    assert_eq!(
        ForwarderHook::parse(&out[..n]),
        Some(ForwarderHook::CreateFor([5; 32]))
    );

    assert_eq!(ForwarderHook::parse(&out[..33]), None);
    let mut bad_magic = out;
    bad_magic[0] = b'x';
    assert_eq!(ForwarderHook::parse(&bad_magic[..65]), None);
    let mut bad_flag = out;
    bad_flag[32] = 0;
    assert_eq!(ForwarderHook::parse(&bad_flag[..65]), None);
}
