use super::*;

#[test]
fn decimals_are_exact_and_trimmed() {
    let cases = [
        (0_u128, 6, "0"),
        (1, 6, "0.000001"),
        (5_100_000, 6, "5.1"),
        (100_000_000, 6, "100"),
        (99_990_000, 6, "99.99"),
        (2_000, 6, "0.002"),
        (25_000_000, 8, "0.25"),
        (250_000, 8, "0.0025"),
        (408_660_260_803_591, 18, "0.000408660260803591"),
        (1_000_000_000_000_000_000, 18, "1"),
        (7, 0, "7"),
        (u128::MAX, 6, "340282366920938463463374607431768.211455"),
    ];
    for (units, decimals, text) in cases {
        assert_eq!(decimal(TokenAmount::new(units), decimals), text, "{units} at {decimals}");
    }
}

/// Review Focus 3: a token the canister has no decimals for keeps its raw form.
#[test]
fn amounts_name_their_token_or_its_id() {
    let vchf = Principal::from_text("ly36x-wiaaa-aaaai-aqj7q-cai").unwrap();
    let other: EvmAddress = "0x3333333333333333333333333333333333333333".parse().unwrap();
    let amount = |units: u128, token: TokenName| Amount { units: TokenAmount::new(units), token }.to_string();
    assert_eq!(amount(5_100_000, CKUSDC), "5.1 ckUSDC");
    assert_eq!(amount(9_900_000, USDC), "9.9 USDC");
    assert_eq!(
        amount(250_000, TokenName::IcpLedger(vchf)),
        "250000 units of token ly36x-wiaaa-aaaai-aqj7q-cai"
    );
    assert_eq!(
        amount(12_345, TokenName::Evm(other)),
        "12345 units of token 0x3333333333333333333333333333333333333333"
    );
    assert_eq!(
        amount(5, TokenName::NativeOf(ChainId::new(5042))),
        "5 units of chain 5042's own coin"
    );
    assert_eq!(CKUSDC.short(), "ckUSDC");
    assert_eq!(TokenName::IcpLedger(vchf).short(), "an ICP token");
}

#[test]
fn chains_are_named_or_numbered() {
    assert_eq!(ChainText(ChainId::ARBITRUM).to_string(), "Arbitrum");
    assert_eq!(ChainText(ChainId::new(5042)).to_string(), "chain 5042");
}

/// Review Focus 1 and 2.
#[test]
fn accounts_are_named_in_full_and_the_asking_wallet_is_marked() {
    // kdt24-ukskj-jfeus-skjjf-eussk-jjfeu-sskjj-feuss-kjjfe-usskj-jfe
    let me = Principal::from_slice(&[0x52; 29]);
    // im66i-jsrkf-ivcuk-rkfiv-cukrk-fivcu-krkfi-vcukr-kfivc-ukrkf-ivc
    let other = Principal::from_slice(&[0x51; 29]);
    let sevens = "0707070707070707070707070707070707070707070707070707070707070707";
    let account = |owner, subaccount| IcpAccount { owner, subaccount };
    assert_eq!(AccountText::Yours { subaccount: None }.to_string(), "your account");
    assert_eq!(AccountText::Yours { subaccount: Some([0; 32]) }.to_string(), "your account");
    assert_eq!(
        AccountText::Yours { subaccount: Some([7; 32]) }.to_string(),
        format!("your account (subaccount {sevens})")
    );
    assert_eq!(
        AccountText::Other { account: account(me, None), asker: me }.to_string(),
        "kdt24-ukskj-jfeus-skjjf-eussk-jjfeu-sskjj-feuss-kjjfe-usskj-jfe (this wallet)"
    );
    assert_eq!(
        AccountText::Other { account: account(me, Some([7; 32])), asker: me }.to_string(),
        format!("kdt24-ukskj-jfeus-skjjf-eussk-jjfeu-sskjj-feuss-kjjfe-usskj-jfe (this wallet, subaccount {sevens})")
    );
    assert_eq!(
        AccountText::Other { account: account(other, None), asker: me }.to_string(),
        "im66i-jsrkf-ivcuk-rkfiv-cukrk-fivcu-krkfi-vcukr-kfivc-ukrkf-ivc"
    );
    assert_eq!(
        AccountText::Other { account: account(other, Some([7; 32])), asker: me }.to_string(),
        format!("im66i-jsrkf-ivcuk-rkfiv-cukrk-fivcu-krkfi-vcukr-kfivc-ukrkf-ivc (subaccount {sevens})")
    );
    // a cold signer asks anonymously: the anonymous principal is never "this wallet"
    let anonymous = Principal::anonymous();
    assert_eq!(
        AccountText::Other { account: account(anonymous, None), asker: anonymous }.to_string(),
        "2vxsx-fae"
    );
    assert_eq!(normal(Some([0; 32])), None);
    assert_eq!(normal(Some([7; 32])), Some([7; 32]));
}

#[test]
fn instants_are_written_in_utc_or_the_offset_asked() {
    let at = |secs: u64| Timestamp::from_nanos(secs * 1_000_000_000);
    let cases = [
        (at(1_793_189_700), None, "28 Oct 2026, 12:15 UTC"),
        (at(1_793_189_700), Some(240), "28 Oct 2026, 16:15 UTC+04:00"),
        (at(1_793_189_700), Some(-330), "28 Oct 2026, 06:45 UTC-05:30"),
        (at(1_793_189_700), Some(841), "28 Oct 2026, 12:15 UTC"),
        (at(1_793_189_700), Some(i16::MIN), "28 Oct 2026, 12:15 UTC"),
        (at(0), None, "1 Jan 1970, 00:00 UTC"),
        (at(0), Some(-60), "31 Dec 1969, 23:00 UTC-01:00"),
        (at(951_782_400), None, "29 Feb 2000, 00:00 UTC"),
        (at(4_102_444_799), None, "31 Dec 2099, 23:59:59 UTC"),
        // seconds are written when they are not zero: a deadline is never rounded
        (Timestamp::from_nanos(1_793_205_930_999_999_999), None, "28 Oct 2026, 16:45:30 UTC"),
        (Timestamp::from_nanos(u64::MAX), None, "21 Jul 2554, 23:34:33 UTC"),
    ];
    for (instant, asked, text) in cases {
        let local = LocalTime { at: instant, offset: UtcOffset::of(asked) };
        assert_eq!(local.to_string(), text, "{instant:?} at {asked:?}");
    }
}

/// Review Focus 4.
#[test]
fn an_offset_beyond_fourteen_hours_is_utc() {
    assert_eq!(UtcOffset::of(None).minutes(), 0);
    assert_eq!(UtcOffset::of(Some(-840)).minutes(), -840);
    assert_eq!(UtcOffset::of(Some(840)).minutes(), 840);
    assert_eq!(UtcOffset::of(Some(841)).minutes(), 0);
    assert_eq!(UtcOffset::of(Some(i16::MIN)).minutes(), 0);
}
