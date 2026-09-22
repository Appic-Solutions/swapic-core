use super::*;
use crate::rails::tests::{
    config, fixture_swap, leg, quote, MINE, USDC_ARBITRUM, USDC_BASE, VAULT_ARBITRUM, VAULT_BASE,
};
use crate::rails::{RailStep, WaitingFor};
use types::abi::{decode_cctp_deposit_for_burn, decode_cctp_receive_message, decode_vault_execute};
use types::config::ChainTable;
use types::{Attestation, ChainId, Config, EvmAddress, Outcome, Timestamp};

fn fast() -> Cctp {
    Cctp { fast: true }
}

fn standard() -> Cctp {
    Cctp { fast: false }
}

/// The fast path is attested at threshold 1000 and pays at most two basis points, rounded
/// up so a tiny burn still offers Circle its minimum; the standard path is attested at
/// 2000 and pays nothing. The amount the destination is promised is the burn less that
/// ceiling.
#[test]
fn fee_and_threshold_follow_the_path() {
    assert_eq!(fast().finality_threshold(), 1_000);
    assert_eq!(standard().finality_threshold(), 2_000);
    let usdc = |units: u128| TokenAmount::from(units);
    assert_eq!(
        fast().max_fee(usdc(25_000_000)),
        Ok(usdc(5_000)),
        "two bps of 25 USDC"
    );
    assert_eq!(
        fast().max_fee(usdc(1)),
        Ok(usdc(1)),
        "rounded up, never zero"
    );
    assert_eq!(fast().max_fee(usdc(4_999)), Ok(usdc(1)));
    assert_eq!(fast().max_fee(usdc(5_001)), Ok(usdc(2)));
    assert_eq!(standard().max_fee(usdc(25_000_000)), Ok(usdc(0)));
    assert_eq!(fast().least_minted(usdc(25_000_000)), Ok(usdc(24_995_000)));
    assert_eq!(
        standard().least_minted(usdc(25_000_000)),
        Ok(usdc(25_000_000))
    );
    assert_eq!(
        fast().max_fee(TokenAmount::MAX),
        Err(RailError::FeeOverflow {
            amount: TokenAmount::MAX
        })
    );
}

/// The burn goes through the source vault's `execute`: one call to the token messenger,
/// approved for exactly the amount of the source USDC, whose `depositForBurn` names the
/// destination's domain, mints to the destination vault padded to a word, may be
/// delivered only by this canister, and asks the path's threshold and fee; and the vault
/// checks its USDC fell by no more than the amount.
#[test]
fn the_burn_is_a_vault_execute_of_deposit_for_burn_to_the_destination_vault() {
    let quote = quote();
    let swap = fixture_swap(None, None);
    let config = config();
    let leg = leg(&quote, &swap, &config, None, None);
    let burn = fast().burn(&leg).expect("everything is configured");
    assert_eq!(burn.purpose, TxPurpose::Burn(leg.quote_hash));
    assert_eq!(burn.chain_id, ChainId::BASE);
    assert_eq!(burn.to, VAULT_BASE.parse::<EvmAddress>().unwrap());
    assert_eq!(burn.value, Wei::ZERO);
    assert_eq!(burn.gas_limit, BURN_GAS_LIMIT);
    let (swap_ref, calls, deltas) = decode_vault_execute(&burn.data).expect("an execute");
    assert_eq!(swap_ref, leg.quote_hash);
    assert_eq!(calls.len(), 1);
    let usdc_base: EvmAddress = USDC_BASE.parse().unwrap();
    assert_eq!(calls[0].target, config.token_messenger.unwrap());
    assert_eq!(calls[0].value, Wei::ZERO);
    assert_eq!(calls[0].approve_token, usdc_base);
    assert_eq!(calls[0].approve_amount, swap.amount_in);
    let inner = decode_cctp_deposit_for_burn(&calls[0].data).expect("a depositForBurn");
    assert_eq!(inner.amount, swap.amount_in);
    assert_eq!(
        inner.destination_domain, 3,
        "Arbitrum's CCTP domain, never its chain id"
    );
    assert_eq!(
        inner.mint_recipient,
        VAULT_ARBITRUM.parse::<EvmAddress>().unwrap().to_word(),
        "the destination vault, padded to a word"
    );
    assert_eq!(inner.burn_token, usdc_base);
    assert_eq!(
        inner.destination_caller,
        MINE.parse::<EvmAddress>().unwrap().to_word(),
        "only this canister delivers the mint"
    );
    assert_eq!(inner.max_fee, TokenAmount::from(5_000_u32));
    assert_eq!(inner.min_finality_threshold, 1_000);
    assert_eq!(
        deltas,
        vec![VaultDelta {
            token: usdc_base,
            min_change: -25_000_000,
        }]
    );

    let standard = standard().burn(&leg).unwrap();
    let (_, calls, _) = decode_vault_execute(&standard.data).unwrap();
    let inner = decode_cctp_deposit_for_burn(&calls[0].data).unwrap();
    assert_eq!(
        (inner.max_fee, inner.min_finality_threshold),
        (TokenAmount::ZERO, 2_000)
    );
}

/// A knob the deploy left unset refuses the burn by name, before anything is signed.
#[test]
fn a_missing_knob_refuses_the_burn_by_name() {
    let quote = quote();
    let swap = fixture_swap(None, None);
    let no_domain = Config {
        cctp_domains: ChainTable::default(),
        ..config()
    };
    assert_eq!(
        fast()
            .burn(&leg(&quote, &swap, &no_domain, None, None))
            .map(drop),
        Err(RailError::NoDomain {
            chain_id: ChainId::ARBITRUM
        })
    );
    let no_usdc = Config {
        usdc_addresses: ChainTable::default(),
        ..config()
    };
    assert_eq!(
        fast()
            .burn(&leg(&quote, &swap, &no_usdc, None, None))
            .map(drop),
        Err(RailError::NoUsdc {
            chain_id: ChainId::BASE
        })
    );
    let no_messenger = Config {
        token_messenger: None,
        ..config()
    };
    assert_eq!(
        fast()
            .burn(&leg(&quote, &swap, &no_messenger, None, None))
            .map(drop),
        Err(RailError::NoTokenMessenger)
    );
    let no_transmitter = Config {
        message_transmitter: None,
        ..config()
    };
    let attestation = Attestation::new(vec![1], vec![2], Timestamp::from_nanos(1)).unwrap();
    let burned = fixture_swap(Some(SwapLeg::Burn), Some(Outcome::Confirmed));
    assert_eq!(
        fast()
            .step(&leg(
                &quote,
                &burned,
                &no_transmitter,
                Some(&attestation),
                None
            ))
            .map(drop),
        Err(RailError::NoMessageTransmitter)
    );
}

/// The rail's whole table: the burn first, then a wait for the attestation, then the mint
/// carrying exactly what the watcher handed in, then the arrival with the least the
/// destination vault holds; and a burn is never reclaimed.
#[test]
fn the_steps_run_burn_attestation_mint_arrival_and_a_burn_is_final() {
    let quote = quote();
    let config = config();
    let fresh = fixture_swap(None, None);
    assert!(matches!(
        fast().step(&leg(&quote, &fresh, &config, None, None)),
        Ok(RailStep::Send(RailTx {
            purpose: TxPurpose::Burn(_),
            ..
        }))
    ));

    let burned = fixture_swap(Some(SwapLeg::Burn), Some(Outcome::Confirmed));
    assert_eq!(
        fast().step(&leg(&quote, &burned, &config, None, None)),
        Ok(RailStep::Wait(WaitingFor::Attestation))
    );
    let attestation =
        Attestation::new(vec![0xaa; 376], vec![0xbb; 65], Timestamp::from_nanos(1)).unwrap();
    let mint = match fast().step(&leg(&quote, &burned, &config, Some(&attestation), None)) {
        Ok(RailStep::Send(tx)) => tx,
        other => panic!("the mint is sent once the attestation is in: {other:?}"),
    };
    assert_eq!(
        mint.purpose,
        TxPurpose::Mint(leg(&quote, &burned, &config, None, None).quote_hash)
    );
    assert_eq!(mint.chain_id, ChainId::ARBITRUM);
    assert_eq!(mint.to, config.message_transmitter.unwrap());
    assert_eq!(mint.gas_limit, MINT_GAS_LIMIT);
    assert_eq!(
        decode_cctp_receive_message(&mint.data),
        Some((vec![0xaa; 376], vec![0xbb; 65]))
    );

    let minted = fixture_swap(Some(SwapLeg::Mint), Some(Outcome::Confirmed));
    assert_eq!(
        fast().step(&leg(&quote, &minted, &config, None, None)),
        Ok(RailStep::Arrived {
            chain_id: ChainId::ARBITRUM,
            amount: TokenAmount::from(24_995_000_u32),
        })
    );
    assert_eq!(
        standard().step(&leg(&quote, &minted, &config, None, None)),
        Ok(RailStep::Arrived {
            chain_id: ChainId::ARBITRUM,
            amount: TokenAmount::from(25_000_000_u32),
        })
    );

    assert!(matches!(
        fast().reclaim(&leg(&quote, &burned, &config, None, None)),
        Ok(RailStep::Stuck(_))
    ));
    let paid_out = fixture_swap(Some(SwapLeg::Payout), Some(Outcome::Confirmed));
    assert!(matches!(
        fast().step(&leg(&quote, &paid_out, &config, None, None)),
        Ok(RailStep::Stuck(_))
    ));

    // a quote paying out a token that is no address is refused before the burn
    let odd_token = types::Quote {
        dst_token: "USDC".parse().unwrap(),
        ..quote.clone()
    };
    assert_eq!(
        fast().step(&leg(&odd_token, &fresh, &config, None, None)),
        Err(RailError::QuoteNotAnAddress { field: "dst_token" })
    );
    let _ = USDC_ARBITRUM;
}
