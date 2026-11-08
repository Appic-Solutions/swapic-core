//! The canister's flows as folded state: a gasless relay it signs and pays the gas of, and a
//! CCTP arrival it finishes. A flow is keyed by a quote hash and moves only by [`FlowStep`]
//! lines in the event log: [`Flow::admits`] is every rule that can refuse a step and
//! [`Flow::apply`] records one that was admitted.
//!
//! Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
//! new field is optional.

use crate::chain::ChainId;
use crate::hash::{QuoteHash, TxHash};
use crate::numeric::{BlockNumber, Nonce, Timestamp};
use minicbor::{Decode, Encode};
use thiserror::Error;

/// What a flow is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum FlowKind {
    /// The canister relays a user's signed order and pays the gas.
    #[n(0)]
    Relay,
    /// The canister finishes a CCTP arrival: keyed by the arrival and the action.
    #[n(3)]
    Finish,
}

/// Where a flow is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum FlowStatus {
    /// The relay is signed and on its way.
    #[n(15)]
    RelaySigned,
    #[n(16)]
    RelayConfirmed,
    #[n(17)]
    RelayFailed,
    /// The CCTP finish is signed and on its way.
    #[n(22)]
    FinishSigned,
    #[n(23)]
    FinishConfirmed,
    #[n(24)]
    FinishFailed,
}

impl FlowStatus {
    /// Terminal: no further work is ever scheduled for the flow.
    pub fn is_closed(self) -> bool {
        !matches!(self, Self::RelaySigned | Self::FinishSigned)
    }
}

/// One step of a flow, as the log records it.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum FlowStep {
    /// A gasless order relayed: the transaction the canister signed for it.
    #[n(9)]
    GaslessRelayed {
        #[n(0)]
        chain_id: ChainId,
        #[n(1)]
        nonce: Nonce,
        #[n(2)]
        tx_hash: TxHash,
        #[cbor(n(3), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
    },
    /// The flow's transaction reached the depth the outbox waits for: `tx_hash` the one
    /// that landed, a replacement included.
    #[n(11)]
    TxClosed {
        #[n(0)]
        tx_hash: TxHash,
        #[n(1)]
        block: BlockNumber,
        #[n(2)]
        success: bool,
    },
    /// A CCTP arrival's finish signed.
    #[n(19)]
    CctpFinishSigned {
        #[n(0)]
        chain_id: ChainId,
        #[n(1)]
        nonce: Nonce,
        #[n(2)]
        tx_hash: TxHash,
        #[cbor(n(3), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
        #[n(4)]
        action: FinishAction,
        #[cbor(n(5), with = "minicbor::bytes")]
        arrival: [u8; 32],
        /// The source transaction that burned the message, whose fee pays for this finish:
        /// no more messages per source transaction are finished than it paid for (`paid`), a
        /// Complete and its Release counting as one.
        #[n(6)]
        source_tx: Option<TxHash>,
        /// How many arrivals the source transaction paid fee steps for (audit C10). None on a
        /// line written before there was a count, and on the canister's own sources: one.
        #[n(7)]
        paid: Option<u32>,
    },
}

/// What the canister does with a CCTP arrival.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum FinishAction {
    /// A Deliver: `receiveMessage` on the message transmitter mints to the user.
    #[n(0)]
    Receive,
    /// A Swap or a CkDeposit before its deadline: `completeCctp` on Swapic.
    #[n(1)]
    Complete,
    /// A Swap or a CkDeposit after its deadline: `release` on Swapic pays the user the
    /// stable.
    #[n(2)]
    Release,
}

impl FinishAction {
    pub fn byte(self) -> u8 {
        match self {
            Self::Receive => 0,
            Self::Complete => 1,
            Self::Release => 2,
        }
    }
}

/// The key a finish is held once under: one signed transaction per arrival and action.
pub fn finish_key(arrival: [u8; 32], action: FinishAction) -> QuoteHash {
    let mut preimage = [0_u8; 33];
    preimage[..32].copy_from_slice(&arrival);
    preimage[32] = action.byte();
    QuoteHash::new(alloy_primitives::keccak256(preimage).0)
}

impl FlowStep {
    /// The step as the bytes the wire carries.
    pub fn to_bytes(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("BUG: encoding into a Vec is infallible")
    }

    /// A step back from [`FlowStep::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        minicbor::decode(bytes).ok()
    }

    /// Whether this line gives the flow a next step the engine should act on now: a closed
    /// transaction does; a signed one waits for its receipt.
    pub fn moves_a_step(&self) -> bool {
        matches!(self, FlowStep::TxClosed { .. })
    }

    /// The step's name, for the wire.
    pub fn name(&self) -> &'static str {
        match self {
            FlowStep::GaslessRelayed { .. } => "GaslessRelayed",
            FlowStep::TxClosed { .. } => "TxClosed",
            FlowStep::CctpFinishSigned { .. } => "CctpFinishSigned",
        }
    }
}

/// The folded state of one flow.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Flow {
    #[n(0)]
    pub kind: FlowKind,
    #[n(1)]
    pub status: FlowStatus,
    /// The chain its transaction is on.
    #[n(12)]
    pub chain_id: Option<ChainId>,
    /// Its transaction: the one signed, then the one that landed once it closed.
    #[n(13)]
    pub tx_hash: Option<TxHash>,
    #[n(14)]
    pub tx_open: bool,
    #[n(15)]
    pub tx_block: Option<BlockNumber>,
    /// When the flow was registered.
    #[n(20)]
    pub created_at: Timestamp,
    /// The log index of the line that recorded the flow's signed transaction: its id in
    /// `signed_txs`.
    #[n(21)]
    pub signed_id: Option<u64>,
    /// A finish's source transaction, which no other message may be finished under.
    #[n(22)]
    pub source_tx: Option<TxHash>,
    /// A finish's arrival: the burn's id, whatever Circle wrote into its message.
    #[cbor(n(23), with = "minicbor::bytes")]
    pub arrival: Option<[u8; 32]>,
    /// The gas action the flow's latest transaction was created under (`TxCreated`): the
    /// budget a replacement of it is held to.
    #[n(24)]
    pub gas_action: Option<crate::config::GasAction>,
}

/// Why a flow step was refused.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FlowError {
    #[error("flow {0} already exists")]
    Exists(QuoteHash),
    #[error("no flow {0}")]
    Unknown(QuoteHash),
    #[error("flow {quote_hash} is {status:?}, and a {step} does not follow from there")]
    NotFromHere {
        quote_hash: QuoteHash,
        status: FlowStatus,
        step: &'static str,
    },
    #[error("the quote's hash is {computed}, not the {declared} the line names")]
    HashMismatch {
        declared: QuoteHash,
        computed: QuoteHash,
    },
    #[error(
        "source transaction {source_tx} already finished another message: it paid for no more \
         arrivals"
    )]
    SourceFinished { source_tx: TxHash },
}

impl Flow {
    fn new(kind: FlowKind, status: FlowStatus, at: Timestamp) -> Self {
        Self {
            kind,
            status,
            chain_id: None,
            tx_hash: None,
            tx_open: false,
            tx_block: None,
            created_at: at,
            signed_id: None,
            source_tx: None,
            arrival: None,
            gas_action: None,
        }
    }

    /// Every rule a step must meet: a signed record creates its flow once (a finish under
    /// its own key, and no more messages under one source transaction than it paid for); a
    /// close ends an open transaction. `others_finished` answers how many arrivals other than
    /// the one named a source transaction was finished under.
    pub fn admits(
        quote_hash: QuoteHash,
        existing: Option<&Flow>,
        step: &FlowStep,
        others_finished: impl Fn(&TxHash, [u8; 32]) -> u32,
    ) -> Result<(), FlowError> {
        match step {
            FlowStep::GaslessRelayed { .. } | FlowStep::CctpFinishSigned { .. } => {
                if existing.is_some() {
                    return Err(FlowError::Exists(quote_hash));
                }
                if let FlowStep::CctpFinishSigned {
                    action,
                    arrival,
                    source_tx,
                    paid,
                    ..
                } = step
                {
                    let computed = finish_key(*arrival, *action);
                    if computed != quote_hash {
                        return Err(FlowError::HashMismatch {
                            declared: quote_hash,
                            computed,
                        });
                    }
                    if let Some(source_tx) = source_tx {
                        if others_finished(source_tx, *arrival) >= paid.unwrap_or(1).max(1) {
                            return Err(FlowError::SourceFinished {
                                source_tx: *source_tx,
                            });
                        }
                    }
                }
                Ok(())
            }
            // the outbox closes on whichever of a nonce's transactions landed, a replacement
            // included, so the hash is recorded rather than matched
            FlowStep::TxClosed { .. } => {
                let flow = existing.ok_or(FlowError::Unknown(quote_hash))?;
                if flow.tx_open
                    && matches!(
                        flow.status,
                        FlowStatus::RelaySigned | FlowStatus::FinishSigned
                    )
                {
                    Ok(())
                } else {
                    Err(FlowError::NotFromHere {
                        quote_hash,
                        status: flow.status,
                        step: step.name(),
                    })
                }
            }
        }
    }

    /// Records a step [`Flow::admits`] admitted, sealed at `index` and `at`.
    pub fn apply(existing: Option<Flow>, step: &FlowStep, index: u64, at: Timestamp) -> Flow {
        match (existing, step) {
            (
                None,
                FlowStep::GaslessRelayed {
                    chain_id, tx_hash, ..
                },
            ) => {
                let mut flow = Flow::new(FlowKind::Relay, FlowStatus::RelaySigned, at);
                flow.chain_id = Some(*chain_id);
                flow.tx_hash = Some(*tx_hash);
                flow.tx_open = true;
                flow.signed_id = Some(index);
                flow
            }
            (
                None,
                FlowStep::CctpFinishSigned {
                    chain_id,
                    tx_hash,
                    arrival,
                    source_tx,
                    ..
                },
            ) => {
                let mut flow = Flow::new(FlowKind::Finish, FlowStatus::FinishSigned, at);
                flow.chain_id = Some(*chain_id);
                flow.tx_hash = Some(*tx_hash);
                flow.tx_open = true;
                flow.signed_id = Some(index);
                flow.source_tx = *source_tx;
                flow.arrival = Some(*arrival);
                flow
            }
            (
                Some(mut flow),
                FlowStep::TxClosed {
                    tx_hash,
                    block,
                    success,
                },
            ) => {
                flow.tx_open = false;
                flow.tx_hash = Some(*tx_hash);
                flow.tx_block = Some(*block);
                flow.status = match (flow.kind, success) {
                    (FlowKind::Relay, true) => FlowStatus::RelayConfirmed,
                    (FlowKind::Relay, false) => FlowStatus::RelayFailed,
                    (FlowKind::Finish, true) => FlowStatus::FinishConfirmed,
                    (FlowKind::Finish, false) => FlowStatus::FinishFailed,
                };
                flow
            }
            (Some(flow), _) => flow,
            (None, _) => unreachable!("BUG: Flow::admits refuses a step on no flow"),
        }
    }
}

crate::storable_as_cbor!(Flow);
