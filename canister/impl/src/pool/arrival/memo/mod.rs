//! A pool deposit's memo read back from the chain: from its order's own `OrderStarted`, in
//! the Base receipt or a CCTP deposit's source receipt, taken only under the ref it hashes
//! to; and a report or a read of Base recorded on those terms.

use super::*;

/// The terms a memo encodes, under the ref it hashes to.
pub(crate) fn given_of(memo: &[u8], src_chain: ChainId) -> Result<Given, PoolError> {
    let terms = DepositTerms::from_memo(memo)
        .map_err(|error| PoolError::BadArrival(format!("the memo: {error}")))?;
    Ok(Given {
        terms,
        r#ref: types::pool::memo::memo_ref(memo),
        src_chain,
    })
}

/// The memo an `OrderStarted` of `swapic` in `receipt` carries under `r#ref`, taken only when
/// its keccak is that ref.
pub fn memo_in(receipt: &Receipt, swapic: EvmAddress, r#ref: PoolRef) -> Option<Vec<u8>> {
    receipt
        .logs
        .iter()
        .filter(|log| log.is(swapic, topics::order_started()))
        .filter_map(|log| swapic::read_order_started(&log.topics, &log.data))
        .find(|order| {
            order.r#ref == r#ref.into_bytes() && types::pool::memo::memo_ref(&order.memo) == r#ref
        })
        .map(|order| order.memo)
}

/// The chain a CCTP deposit under `r#ref` in `base` left from, and its Swapic: by the
/// domain of the received message whose envelope carries the ref.
pub(super) fn cctp_source_of<'a>(
    base: &Receipt,
    chain: &'a PoolChain,
    r#ref: PoolRef,
) -> Option<&'a CctpSource> {
    let domain = received_messages(base, chain)
        .into_iter()
        .find_map(|(_, message)| {
            let burn = burn_body(&message.body)?;
            let envelope = Envelope::decode(burn.hook).ok()?;
            (envelope.reference == r#ref.into_bytes()).then_some(message.source_domain)
        })?;
    chain
        .cctp_sources
        .iter()
        .find(|source| source.domain == domain)
}

/// The source transaction of the CCTP message the canister finished itself in `base_tx`.
pub(super) fn own_finish_source(base_tx: TxHash) -> Option<TxHash> {
    read_state(|state| {
        state
            .finish_landed_in(&base_tx)
            .and_then(|flow| flow.source_tx)
    })
}

/// The memo of the first deposit in `found` the chain shows: a same-chain order's own
/// `OrderStarted` in `base`; a CCTP deposit's in its source transaction (`source` the
/// report's, or the canister's own finish's), read on the source chain. None when no
/// deposit's is found.
pub async fn memo_from_chain(
    base_tx: TxHash,
    base: &Receipt,
    found: &[Found],
    chain: &PoolChain,
    source: Option<(ChainId, TxHash)>,
) -> Option<Vec<u8>> {
    for one in found {
        let Proof::Deposit { r#ref, .. } = one.proof else {
            continue;
        };
        match one.rail {
            PoolRail::SameChain => {
                if let Some(memo) = memo_in(base, chain.swapic, r#ref) {
                    return Some(memo);
                }
            }
            PoolRail::Cctp => {
                let Some(from) = cctp_source_of(base, chain, r#ref) else {
                    continue;
                };
                let src_tx = source
                    .filter(|(src_chain, _)| *src_chain == from.chain)
                    .map(|(_, tx)| tx)
                    .or_else(|| own_finish_source(base_tx));
                let Some(src_tx) = src_tx else {
                    continue;
                };
                let Ok(receipt) = read_source(from.chain, src_tx).await else {
                    continue;
                };
                if let Some(memo) = memo_in(&receipt, from.swapic, r#ref) {
                    return Some(memo);
                }
            }
            // Eco is off; its report brings the memo
            PoolRail::Eco => {}
        }
    }
    None
}

/// Reads `base_tx`, proves its transfers, and records them on the terms of the memo the
/// chain shows (`memo_from_chain`), or with none: a deposit's transfer then awaits its
/// terms, as the canister's own read of Base leaves it.
pub(super) async fn read_memo_and_record(
    base_tx: TxHash,
    source: Option<(ChainId, TxHash)>,
    src_chain: ChainId,
) -> Result<Recorded, PoolError> {
    let chain = pool_chain().await?;
    let base = read_base(&chain, base_tx).await?;
    let found = found_in(&chain, &base, source).await?;
    let memo = memo_from_chain(base_tx, &base, &found, &chain, source).await;
    pool::require_running()?;
    let given = memo
        .as_deref()
        .map(|memo| given_of(memo, src_chain))
        .transpose()?;
    let judge = match &given {
        Some(given) => Judge {
            given: Some(given),
            closed: pool::pool_closed(),
            sanctioned: &crate::storage::sanctions::is_sanctioned,
        },
        None => Judge::without_terms(),
    };
    Ok(record(&found, &judge, pool::now()))
}
