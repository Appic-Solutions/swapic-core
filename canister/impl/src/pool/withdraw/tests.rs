use super::*;
use crate::storage::events::read_state;
use crate::storage::on_fresh_memory;
use crate::storage::pool::tests::{at, base_stake, funded_pool, usdc};
use crate::storage::pool::{ledger, payout, record_at, shares_of};
use std::collections::BTreeMap;
use types::config::{ChainTable, EvmChain, PerAction};
use types::pool::payout::{PayoutStatus, PayoutTerms};
use types::pool::PayoutSettings;
use types::TokenAmount;

fn key() -> libsecp256k1::SecretKey {
    libsecp256k1::SecretKey::parse(&[0x42; 32]).unwrap()
}

fn address_of(key: &libsecp256k1::SecretKey) -> EvmAddress {
    EvmAddress::from_public_key(&libsecp256k1::PublicKey::from_secret_key(key).serialize())
}

/// `r ++ s ++ v`, `v` 27 or 28, as a wallet signs.
fn sign(digest: [u8; 32], key: &libsecp256k1::SecretKey) -> Vec<u8> {
    let (signature, recovery) = libsecp256k1::sign(&libsecp256k1::Message::parse(&digest), key);
    let mut out = signature.serialize().to_vec();
    out.push(recovery.serialize() + 27);
    out
}

fn swapic() -> EvmAddress {
    "0xF0Ba68cBc87c31eCaA9654EB14daF3632eaCa406"
        .parse()
        .unwrap()
}

fn config() -> types::Config {
    let chain = EvmChain {
        swapic: swapic(),
        runner: EvmAddress::new([0x22; 20]),
        cctp_receiver: EvmAddress::new([0x23; 20]),
        usdc: EvmAddress::new([0x24; 20]),
        token_messenger: EvmAddress::new([0x25; 20]),
        message_transmitter: EvmAddress::new([0x26; 20]),
        gas_limit: PerAction::all(types::GasAmount::from(500_000_u64)),
        gas_budget: PerAction::all(types::Wei::from(1_000_000_000_000_000_u64)),
        min_network_fee: PerAction::all(TokenAmount::from(50_000_u64)),
    };
    types::Config {
        evm_chains: ChainTable(BTreeMap::from([
            (ChainId::BASE, chain),
            (ChainId::ARBITRUM, chain),
        ])),
        pool_chain: Some(ChainId::BASE),
        ..types::Config::default()
    }
}

/// A pool with 1,000 ckUSDC on ICP and the staker's 2,000 USDC staked on Base.
fn pool() {
    funded_pool(usdc(1_000));
    record_at(
        PoolStep::PayoutSettingsSet {
            settings: PayoutSettings::default(),
        },
        at(1),
    )
    .unwrap();
    base_stake(address_of(&key()), usdc(2_000), 0x71, 1);
}

fn request(usdc_out: u64, nonce: u64) -> WithdrawalRequest {
    let staker = address_of(&key());
    let pool = ledger();
    let burned = Shares::burn_for(usdc(usdc_out), pool.total_assets(), pool.total_shares).unwrap();
    WithdrawalRequest {
        staker,
        usdc: usdc(usdc_out),
        max_shares: burned,
        rebalance_share: usdc(usdc_out)
            .checked_mul(2_u8)
            .and_then(|scaled| scaled.checked_div_ceil(10_000_u32))
            .unwrap(),
        network_fee: TokenAmount::from(50_000_u32),
        payout: PayoutTerms {
            dst_chain: ChainId::ARBITRUM,
            recipient: staker,
            token_out: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831"
                .parse()
                .unwrap(),
            min_out: TokenAmount::from(1_u8),
            deadline: at(10).checked_add(MIN_DEADLINE).unwrap(),
        },
        nonce,
    }
}

fn signed(request: &WithdrawalRequest, key: &libsecp256k1::SecretKey) -> Vec<u8> {
    sign(request.digest(ChainId::BASE, swapic()), key)
}

fn planned(request: &WithdrawalRequest, signature: &[u8]) -> Result<Planned, PoolError> {
    read_state(|state| {
        plan(
            state,
            &config(),
            request,
            signature,
            at(10),
            Some(EvmAddress::new([0x36; 20])),
            None,
            |_| false,
        )
    })
}

/// Review Focus 3: a good signature burns the shares `usdc` is worth and opens a payout of
/// the rest, waiting with its USDC reserved; the nonce is spent with it.
#[test]
fn a_signed_withdrawal_burns_its_shares_and_waits() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let request = request(500, 7);
        let staker = PoolAccount::Evm(request.staker);
        let before = shares_of(&staker);
        let planned = planned(&request, &signed(&request, &key())).unwrap();
        let burned = planned.withdrawal.burned;
        assert_eq!(burned, request.max_shares);
        let id = record(planned, request.payout, at(10));
        assert_eq!(shares_of(&staker), before.checked_sub(burned).unwrap());
        let opened = payout(id).unwrap();
        assert_eq!(opened.status, PayoutStatus::Waiting);
        let amount = usdc(500)
            .checked_sub(request.rebalance_share)
            .and_then(|left| left.checked_sub(request.network_fee))
            .unwrap();
        assert_eq!(opened.amount, Some(amount));
        assert_eq!(
            ledger().pockets.base.reserved,
            amount.checked_add(request.network_fee).unwrap()
        );
        // used once: the same signature again is refused
        assert!(matches!(
            planned_again(&request),
            Err(PoolError::NotAllowed(why)) if why.contains("nonce")
        ));
    });
}

fn planned_again(request: &WithdrawalRequest) -> Result<Planned, PoolError> {
    planned(request, &signed(request, &key()))
}

#[test]
fn another_keys_signature_is_refused() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let request = request(500, 7);
        let stranger = libsecp256k1::SecretKey::parse(&[0x43; 32]).unwrap();
        assert!(matches!(
            planned(&request, &signed(&request, &stranger)),
            Err(PoolError::NotAllowed(why)) if why.contains("not the staker's")
        ));
        // a signature over other terms recovers to someone else
        let mut other = request;
        other.payout.min_out = TokenAmount::from(2_u8);
        assert!(matches!(
            planned(&request, &signed(&other, &key())),
            Err(PoolError::NotAllowed(_))
        ));
        // a malformed signature
        let mut bad = signed(&request, &key());
        bad[64] = 29;
        assert!(matches!(
            planned(&request, &bad),
            Err(PoolError::NotAllowed(_))
        ));
        assert!(matches!(
            planned(&request, &bad[..64]),
            Err(PoolError::NotAllowed(_))
        ));
    });
}

#[test]
fn an_expired_deadline_or_terms_below_the_floors_are_refused() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        let mut late = request(500, 7);
        late.payout.deadline = at(10)
            .checked_add(MIN_DEADLINE)
            .and_then(|deadline| deadline.checked_sub(std::time::Duration::from_nanos(1)))
            .unwrap();
        assert_eq!(
            planned(&late, &signed(&late, &key())).unwrap_err(),
            PoolError::Expired
        );
        let mut cheap = request(500, 7);
        cheap.rebalance_share = cheap.rebalance_share.checked_sub(TokenAmount::ONE).unwrap();
        assert!(matches!(
            planned(&cheap, &signed(&cheap, &key())),
            Err(PoolError::NotAllowed(why)) if why.contains("rebalancing share")
        ));
        let mut cheap = request(500, 7);
        cheap.network_fee = TokenAmount::from(49_999_u32);
        assert!(matches!(
            planned(&cheap, &signed(&cheap, &key())),
            Err(PoolError::NotAllowed(why)) if why.contains("network fee")
        ));
        let good = request(500, 7);
        let sanctioned = read_state(|state| {
            plan(
                state,
                &config(),
                &good,
                &signed(&good, &key()),
                at(10),
                None,
                None,
                |address| *address == Address::from(good.staker),
            )
        });
        assert!(matches!(sanctioned, Err(PoolError::Sanctioned(_))));
        let closed = read_state(|state| {
            plan(
                state,
                &config(),
                &good,
                &signed(&good, &key()),
                at(10),
                None,
                Some("paused".to_string()),
                |_| false,
            )
        });
        assert_eq!(closed.unwrap_err(), PoolError::Closed("paused".to_string()));
    });
}

/// Above the staker's value, or `max_shares` below the burn: refused, nothing burned.
#[test]
fn a_withdrawal_above_its_value_or_its_cap_is_refused() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        // 2,001 is above the staker's 2,000
        let mut above = request(2_000, 7);
        above.usdc = usdc(2_001);
        above.rebalance_share = usdc(1);
        above.max_shares = Shares::new(u128::MAX);
        assert!(matches!(
            planned(&above, &signed(&above, &key())),
            Err(PoolError::ShortShares { .. })
        ));
        let mut capped = request(500, 7);
        capped.max_shares = Shares::new(capped.max_shares.get() - 1);
        assert!(matches!(
            planned(&capped, &signed(&capped, &key())),
            Err(PoolError::ShortShares { .. })
        ));
        assert!(ledger().lines().open.is_empty());
    });
}

/// The Base pocket short: the withdrawal is queued, its shares burned all the same.
#[test]
fn a_withdrawal_the_base_pocket_cannot_cover_is_queued() {
    on_fresh_memory(|| {
        crate::storage::init();
        pool();
        // the staker also stakes 1,000 on ICP: its value is above what the Base pocket holds
        let id = types::hash::PoolOpId::new([0xef; 32]);
        let owner = PoolAccount::Evm(address_of(&key()));
        record_at(
            PoolStep::StakeStarted {
                id,
                owner,
                from: crate::storage::pool::tests::account(9),
                amount: usdc(1_000),
                created_at: at(2),
            },
            at(2),
        )
        .unwrap();
        let shares = ledger().mint_now(usdc(1_000)).unwrap();
        record_at(
            PoolStep::StakedIcp {
                id,
                owner,
                amount: usdc(1_000),
                rebalance_share: TokenAmount::ZERO,
                shares,
                block: 3,
            },
            at(2),
        )
        .unwrap();
        let request = request(1_999, 7);
        let first = planned(&request, &signed(&request, &key())).unwrap();
        let id = record(first, request.payout, at(10));
        // 1,999 less its share and fee, and the fee, are within the 2,000: it waits
        assert_eq!(payout(id).unwrap().status, PayoutStatus::Waiting);
        let staker = PoolAccount::Evm(request.staker);
        assert!(shares_of(&staker) > Shares::ZERO);
        let second = WithdrawalRequest {
            usdc: TokenAmount::from(1_500_000_u32),
            max_shares: Shares::new(u128::MAX),
            rebalance_share: TokenAmount::from(1_000_u32),
            ..request
        };
        let second = WithdrawalRequest { nonce: 8, ..second };
        let next = planned(&second, &signed(&second, &key()));
        // what is left on Base cannot cover it
        let id = record(next.unwrap(), second.payout, at(10));
        assert_eq!(payout(id).unwrap().status, PayoutStatus::Queued);
        assert_eq!(ledger().lines().queue_len, 1);
    });
}
