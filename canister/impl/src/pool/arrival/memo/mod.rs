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

/// The source chain reads a CCTP deposit's memo makes, so a test hands in a fake.
#[allow(async_fn_in_trait)]
pub trait SourceReads {
    /// The source chain's receipt of `tx`, read by every node and agreed on.
    async fn source(&self, chain: ChainId, tx: TxHash) -> Result<Receipt, PoolError>;
}

/// The source chains themselves.
pub struct LiveSource;

impl SourceReads for LiveSource {
    async fn source(&self, chain: ChainId, tx: TxHash) -> Result<Receipt, PoolError> {
        read_source(chain, tx).await
    }
}

/// The memos the chain shows for the deposits in `found`, each from its own order's
/// `OrderStarted` under its own ref: a same-chain order's in `base`; a CCTP deposit's in its
/// source transaction (`source` the report's, or the canister's own finish's), read once on
/// the source chain for every deposit it carries. A deposit whose memo is not there is left
/// out. A source read that failed for now is the error, so the caller asks again rather than
/// leave a deposit awaiting terms the chain holds.
pub async fn memos_from_chain(
    base_tx: TxHash,
    base: &Receipt,
    found: &[Found],
    chain: &PoolChain,
    source: Option<(ChainId, TxHash)>,
    reads: &impl SourceReads,
) -> Result<Vec<Vec<u8>>, PoolError> {
    let mut memos = Vec::new();
    // each source receipt read once; None for one that never reads
    let mut read: Vec<((ChainId, TxHash), Option<Receipt>)> = Vec::new();
    for one in found {
        let Proof::Deposit { r#ref, .. } = one.proof else {
            continue;
        };
        let memo = match one.rail {
            PoolRail::SameChain => memo_in(base, chain.swapic, r#ref),
            PoolRail::Cctp | PoolRail::Across => {
                let from = match one.rail {
                    PoolRail::Cctp => cctp_source_of(base, chain, r#ref).map(|from| (from.chain, from.swapic)),
                    // proven on the report's own source: its chain's Swapic
                    _ => source.and_then(|(src_chain, _)| {
                        crate::storage::config::with(|config| {
                            config.evm_chains.get(src_chain).map(|evm| (src_chain, evm.swapic))
                        })
                    }),
                };
                let Some((from_chain, from_swapic)) = from else {
                    continue;
                };
                let src_tx = source
                    .filter(|(src_chain, _)| *src_chain == from_chain)
                    .map(|(_, tx)| tx)
                    .or_else(|| (one.rail == PoolRail::Cctp).then(|| own_finish_source(base_tx)).flatten());
                let Some(src_tx) = src_tx else {
                    continue;
                };
                let key = (from_chain, src_tx);
                if !read.iter().any(|(at, _)| *at == key) {
                    let receipt = match reads.source(from_chain, src_tx).await {
                        Ok(receipt) => Some(receipt),
                        Err(PoolError::BadArrival(_)) => None,
                        Err(error) => return Err(error),
                    };
                    read.push((key, receipt));
                }
                read.iter()
                    .find(|(at, _)| *at == key)
                    .and_then(|(_, receipt)| receipt.as_ref())
                    .and_then(|receipt| memo_in(receipt, from_swapic, r#ref))
            }
            // Eco is off; its report brings the memo
            PoolRail::Eco => None,
        };
        memos.extend(memo);
    }
    Ok(memos)
}

/// The terms each memo encodes, under the ref it hashes to; a memo that does not decode
/// gives none, and its deposit awaits its terms.
pub(crate) fn givens_of(memos: &[Vec<u8>], src_chain: ChainId) -> Vec<Given> {
    memos
        .iter()
        .filter_map(|memo| given_of(memo, src_chain).ok())
        .collect()
}

/// Reads `base_tx`, proves its transfers, and records them on the terms of the memos the
/// chain shows (`memos_from_chain`): a deposit's transfer with none awaits its terms, as the
/// canister's own read of Base leaves it. A memo read that failed for now records nothing.
pub(super) async fn read_memo_and_record(
    base_tx: TxHash,
    source: Option<(ChainId, TxHash)>,
    src_chain: ChainId,
) -> Result<Recorded, PoolError> {
    let chain = pool_chain().await?;
    let base = read_base(&chain, base_tx).await?;
    let found = found_in(&chain, &base, source).await?;
    let memos = memos_from_chain(base_tx, &base, &found, &chain, source, &LiveSource).await?;
    pool::require_running()?;
    let givens = givens_of(&memos, src_chain);
    Ok(record(&found, &Judge::of(&givens), pool::now()))
}
