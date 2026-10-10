//! The canister's new roles as folded state: a gasless relay through the router, ICP to
//! EVM through the ck minter, and EVM to ICP into a quote's own subaccount.
//!
//! A flow is keyed by a quote hash and moves only by [`FlowStep`] lines in the event log.
//! [`Flow::admits`] is every rule that can refuse a step and [`Flow::apply`] records one
//! that was admitted, so the fold of the log is the state and a replay rebuilds it.
//!
//! Every inter-canister call that can move money is bracketed: a [`FlowStep::Begun`]
//! marker is appended before the await, and its result after it. A flow found still in a
//! begun stage is a call whose outcome is not known. The ledger stages are retried with
//! the same deduplication key (memo = quote hash, `created_at_time` = the marker's
//! instant), so a retry is answered `Duplicate` instead of moving the money twice; a swap
//! or a withdrawal has no such key and freezes for a human.
//!
//! Stored as minicbor: `#[n]` indices are append-only, never renumbered or reused, and a
//! new field is optional.

use crate::chain::ChainId;
use crate::hash::{QuoteHash, TxHash};
use crate::numeric::{BlockNumber, Nonce, Timestamp, TokenAmount, UnixSeconds};
use minicbor::{Decode, Encode};
use sha2::Digest;
use thiserror::Error;

/// What a flow is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum FlowKind {
    /// The canister relays a user's signed `swapFor` and pays the gas.
    #[n(0)]
    Relay,
    /// The user pays on ICP and is paid on an EVM chain.
    #[n(1)]
    IcpToEvm,
    /// The user pays on an EVM chain and is paid an ICP token other than ckUSDC.
    #[n(2)]
    EvmToIcp,
}

/// Where a flow is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum FlowStatus {
    /// Registered and waiting: for `start_icp_swap`, or for the ckUSDC to arrive.
    #[n(0)]
    Registered,
    /// The pull from the user's allowance is out.
    #[n(1)]
    Pulling,
    /// The user's token is held for the quote.
    #[n(2)]
    Pulled,
    /// The ICPSwap call is out.
    #[n(3)]
    Swapping,
    /// The swap's output is held for the quote.
    #[n(4)]
    Swapped,
    /// The ck minter's `withdraw_erc20` is out.
    #[n(5)]
    Withdrawing,
    /// The minter took the ckUSDC; the USDC is on its way to this canister's address.
    #[n(6)]
    Withdrawn,
    /// The USDC is at this canister's Ethereum address, at depth.
    #[n(7)]
    Landed,
    /// The continuation transaction is out.
    #[n(8)]
    Continuing,
    #[n(9)]
    Done,
    /// A step before the withdrawal failed: the held token goes back to the user.
    #[n(10)]
    RefundDue,
    /// The refund transfer is out.
    #[n(11)]
    Refunding,
    #[n(12)]
    Refunded,
    #[n(13)]
    Frozen,
    /// Nothing ever moved and the quote ran out.
    #[n(14)]
    Expired,
    /// The relay is signed and on its way.
    #[n(15)]
    RelaySigned,
    #[n(16)]
    RelayConfirmed,
    #[n(17)]
    RelayFailed,
    /// The ckUSDC is in the quote's subaccount.
    #[n(18)]
    Arrived,
    /// The move from the quote's subaccount to the canister's own account is out.
    #[n(19)]
    Consolidating,
    /// The ckUSDC is in the canister's own account, ready for ICPSwap.
    #[n(20)]
    Consolidated,
    /// The transfer to the user is out.
    #[n(21)]
    Delivering,
}

impl FlowStatus {
    /// Terminal: no further work is ever scheduled for the flow.
    pub fn is_closed(self) -> bool {
        matches!(
            self,
            Self::Done
                | Self::Refunded
                | Self::Frozen
                | Self::Expired
                | Self::RelayConfirmed
                | Self::RelayFailed
        )
    }
}

/// The inter-canister call a [`FlowStep::Begun`] marker brackets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum Stage {
    #[n(0)]
    Pull,
    #[n(1)]
    Swap,
    #[n(2)]
    Withdraw,
    #[n(3)]
    Consolidate,
    #[n(4)]
    Deliver,
    #[n(5)]
    Refund,
}

impl Stage {
    /// The status a flow is in while this call is out.
    fn status(self) -> FlowStatus {
        match self {
            Stage::Pull => FlowStatus::Pulling,
            Stage::Swap => FlowStatus::Swapping,
            Stage::Withdraw => FlowStatus::Withdrawing,
            Stage::Consolidate => FlowStatus::Consolidating,
            Stage::Deliver => FlowStatus::Delivering,
            Stage::Refund => FlowStatus::Refunding,
        }
    }

    /// Whether a retry of this call is deduplicated by its ledger, so an outcome in doubt
    /// is settled by asking again.
    pub fn retries_safely(self) -> bool {
        matches!(
            self,
            Stage::Pull | Stage::Consolidate | Stage::Deliver | Stage::Refund
        )
    }
}

/// A principal as raw bytes: the domain carries no candid type.
pub type PrincipalBytes = Vec<u8>;

/// The ICP to EVM quote as the quoter registers it (spec 4b.1, 4b.7): who pays on ICP and
/// with what, and the user's ORIGINAL destination terms, which are the route template's
/// `dstChainId` and `dst` (the destination, the token and the `min_out`).
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct IcpToEvmQuote {
    /// The user's principal: whose allowance is pulled, and where a refund goes.
    #[cbor(n(0), with = "minicbor::bytes")]
    pub user: PrincipalBytes,
    /// The ICRC ledger of the token the user pays.
    #[cbor(n(1), with = "minicbor::bytes")]
    pub src_ledger: PrincipalBytes,
    #[n(2)]
    pub amount_in: TokenAmount,
    /// The ICPSwap pool from the paid token to ckUSDC, when the token is not ckUSDC.
    #[n(3)]
    pub pool: Option<minicbor::bytes::ByteVec>,
    /// The least ckUSDC the ICP side must produce: the ICPSwap floor, and the withdrawal.
    #[n(4)]
    pub min_ckusdc: TokenAmount,
    /// The router params for the Ethereum continuation, as one ABI tuple. At landing the
    /// canister keeps the route (`rail`, `dstChainId`, `maxFee`, `minFinality`, `eco`,
    /// `dst`) and fills in itself as the payer of what landed.
    #[cbor(n(5), with = "minicbor::bytes")]
    pub route: Vec<u8>,
    #[n(6)]
    pub expires_at: UnixSeconds,
    #[n(7)]
    pub nonce: u64,
}

/// The EVM to ICP quote, registered up front (spec 4b.8): the user's principal is recorded
/// with it, and the ckUSDC lands in the canister's subaccount named by the router params'
/// `ref`. The ref and not the router's quote hash, because an Eco route is fixed before
/// the quote hash exists and so cannot carry it (abi.md section 6); the ref is chosen by
/// the quoter before any provider quote, and works for CCTP as well. The flow is keyed by
/// it.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct EvmToIcpQuote {
    #[cbor(n(0), with = "minicbor::bytes")]
    pub user: PrincipalBytes,
    /// The ICRC ledger of the token the user is paid. ckUSDC itself is forwarded.
    #[cbor(n(1), with = "minicbor::bytes")]
    pub target_ledger: PrincipalBytes,
    /// The ICPSwap pool from ckUSDC to the target, when the target is not ckUSDC.
    #[n(2)]
    pub pool: Option<minicbor::bytes::ByteVec>,
    /// The least of the target the user is paid by the swap.
    #[n(3)]
    pub min_out: TokenAmount,
    /// The EVM chain the user pays on, for the record.
    #[n(4)]
    pub src_chain: ChainId,
    /// How long the arrival is waited for.
    #[n(5)]
    pub expires_at: UnixSeconds,
    #[n(6)]
    pub nonce: u64,
    /// `SwapParams.ref` of the router quote that pays this one: the flow's key and the
    /// subaccount the ckUSDC lands in.
    #[n(7)]
    pub reference: QuoteHash,
}

/// The domain tags the two quote hashes are taken under, so a hash of one kind is never a
/// hash of the other.
const ICP_TO_EVM_TAG: &[u8] = b"swapic-icp-to-evm-v1";
const EVM_TO_ICP_TAG: &[u8] = b"swapic-evm-to-icp-v1";

fn tagged_hash(tag: &[u8], body: &[u8]) -> QuoteHash {
    let mut hasher = sha2::Sha256::new();
    hasher.update((tag.len() as u32).to_be_bytes());
    hasher.update(tag);
    hasher.update(body);
    QuoteHash::new(hasher.finalize().into())
}

impl IcpToEvmQuote {
    /// The quote's hash: what the user's approval names as its spender subaccount.
    pub fn hash(&self) -> QuoteHash {
        tagged_hash(
            ICP_TO_EVM_TAG,
            &minicbor::to_vec(self).expect("BUG: encoding into a Vec is infallible"),
        )
    }
}

impl EvmToIcpQuote {
    /// The flow's key and the subaccount the ckUSDC lands in: the router params' `ref`.
    pub fn hash(&self) -> QuoteHash {
        self.reference
    }

    /// A digest of every term, for the record.
    pub fn terms_digest(&self) -> QuoteHash {
        tagged_hash(
            EVM_TO_ICP_TAG,
            &minicbor::to_vec(self).expect("BUG: encoding into a Vec is infallible"),
        )
    }
}

/// One step of a flow, as the log records it.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub enum FlowStep {
    #[n(0)]
    IcpQuoteRegistered {
        #[n(0)]
        quote: IcpToEvmQuote,
    },
    #[n(1)]
    EvmQuoteRegistered {
        #[n(0)]
        quote: EvmToIcpQuote,
    },
    /// An inter-canister call that can move money is about to go out.
    #[n(2)]
    Begun {
        #[n(0)]
        stage: Stage,
    },
    /// The user's token arrived from the allowance that names the quote.
    #[n(3)]
    Pulled {
        #[n(0)]
        amount: TokenAmount,
        #[n(1)]
        block_index: u64,
    },
    /// The ledger refused the pull for good (no allowance, too little): nothing moved.
    #[n(4)]
    PullRefused {
        #[n(0)]
        reason: String,
    },
    /// ICPSwap paid out `amount` of `ledger`.
    #[n(5)]
    Swapped {
        #[cbor(n(0), with = "minicbor::bytes")]
        ledger: PrincipalBytes,
        #[n(1)]
        amount: TokenAmount,
    },
    /// The swap was not made, because the pool would pay below the floor: nothing moved.
    #[n(6)]
    SwapSkipped {
        #[n(0)]
        reason: String,
    },
    /// The minter burned the ckUSDC for a withdrawal to this canister's address.
    #[n(7)]
    Withdrawn {
        #[n(0)]
        withdrawal_id: u64,
        #[n(1)]
        amount: TokenAmount,
    },
    /// The withdrawal's USDC is at this canister's Ethereum address, at depth.
    #[n(8)]
    Landed {
        #[n(0)]
        chain_id: ChainId,
        #[n(1)]
        tx_hash: TxHash,
        #[n(2)]
        amount: TokenAmount,
    },
    /// The relay's `swapFor` was signed, with its exact bytes, before it is broadcast. It
    /// spends the nonce `TxCreated` handed out for it.
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
    /// The continuation on Ethereum was signed, spending `amount` of the USDC that landed
    /// for the quote: `action` is the route's (0 a Deliver, the fallback).
    #[n(10)]
    ContinuationSigned {
        #[n(0)]
        chain_id: ChainId,
        #[n(1)]
        nonce: Nonce,
        #[n(2)]
        tx_hash: TxHash,
        #[cbor(n(3), with = "minicbor::bytes")]
        raw_tx: Vec<u8>,
        #[n(4)]
        action: u8,
        #[n(5)]
        amount: TokenAmount,
    },
    /// The flow's transaction has a receipt at depth.
    #[n(11)]
    TxClosed {
        #[n(0)]
        tx_hash: TxHash,
        #[n(1)]
        block: BlockNumber,
        #[n(2)]
        success: bool,
    },
    /// A step before the withdrawal failed for good.
    #[n(12)]
    RefundStarted {
        #[n(0)]
        reason: String,
    },
    /// The held token went back to the user's principal.
    #[n(13)]
    Refunded {
        #[n(0)]
        amount: TokenAmount,
        #[n(1)]
        block_index: u64,
    },
    /// ckUSDC is in the quote's subaccount.
    #[n(14)]
    Arrived {
        #[cbor(n(0), with = "minicbor::bytes")]
        ledger: PrincipalBytes,
        #[n(1)]
        amount: TokenAmount,
    },
    /// The ckUSDC moved from the quote's subaccount to the canister's own account.
    #[n(15)]
    Consolidated {
        #[n(0)]
        amount: TokenAmount,
    },
    /// The user was paid on ICP.
    #[n(16)]
    Delivered {
        #[n(0)]
        amount: TokenAmount,
        #[n(1)]
        block_index: u64,
    },
    #[n(17)]
    Frozen {
        #[n(0)]
        reason: String,
    },
    #[n(18)]
    Expired,
}

impl FlowStep {
    /// The step as the bytes the log's preimage and the wire carry.
    pub fn to_bytes(&self) -> Vec<u8> {
        minicbor::to_vec(self).expect("BUG: encoding into a Vec is infallible")
    }

    /// A step back from [`FlowStep::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        minicbor::decode(bytes).ok()
    }

    /// Whether this line gives the flow a next step the engine should act on now. The
    /// markers, the registrations and the signed records do not: a marker's own call is
    /// running, a registration waits for its user, and a signed transaction for its
    /// receipt.
    pub fn moves_a_step(&self) -> bool {
        !matches!(
            self,
            FlowStep::IcpQuoteRegistered { .. }
                | FlowStep::EvmQuoteRegistered { .. }
                | FlowStep::Begun { .. }
                | FlowStep::GaslessRelayed { .. }
                | FlowStep::ContinuationSigned { .. }
                | FlowStep::Frozen { .. }
                | FlowStep::Expired
        )
    }

    /// The step's name, for the wire.
    pub fn name(&self) -> &'static str {
        match self {
            FlowStep::IcpQuoteRegistered { .. } => "IcpQuoteRegistered",
            FlowStep::EvmQuoteRegistered { .. } => "EvmQuoteRegistered",
            FlowStep::Begun { .. } => "Begun",
            FlowStep::Pulled { .. } => "Pulled",
            FlowStep::PullRefused { .. } => "PullRefused",
            FlowStep::Swapped { .. } => "Swapped",
            FlowStep::SwapSkipped { .. } => "SwapSkipped",
            FlowStep::Withdrawn { .. } => "Withdrawn",
            FlowStep::Landed { .. } => "Landed",
            FlowStep::GaslessRelayed { .. } => "GaslessRelayed",
            FlowStep::ContinuationSigned { .. } => "ContinuationSigned",
            FlowStep::TxClosed { .. } => "TxClosed",
            FlowStep::RefundStarted { .. } => "RefundStarted",
            FlowStep::Refunded { .. } => "Refunded",
            FlowStep::Arrived { .. } => "Arrived",
            FlowStep::Consolidated { .. } => "Consolidated",
            FlowStep::Delivered { .. } => "Delivered",
            FlowStep::Frozen { .. } => "Frozen",
            FlowStep::Expired => "Expired",
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
    #[n(2)]
    pub icp_quote: Option<IcpToEvmQuote>,
    #[n(3)]
    pub evm_quote: Option<EvmToIcpQuote>,
    /// What the canister holds for the quote on ICP, in `held_ledger`.
    #[n(4)]
    pub held: TokenAmount,
    #[n(5)]
    pub held_ledger: Option<minicbor::bytes::ByteVec>,
    /// Whether what is held sits in the quote's subaccount rather than the canister's own
    /// account.
    #[n(6)]
    pub held_in_subaccount: bool,
    /// When the call the flow is in a begun stage for was marked: its ledger's
    /// `created_at_time`, so a retry is the same transfer.
    #[n(7)]
    pub begun_at: Option<Timestamp>,
    #[n(8)]
    pub withdrawal_id: Option<u64>,
    /// The ckUSDC the withdrawal burned.
    #[n(9)]
    pub withdrawn: Option<TokenAmount>,
    /// The USDC this canister holds on Ethereum for the quote (spec 4b.7).
    #[n(10)]
    pub eth_held: TokenAmount,
    /// The transfer that landed it.
    #[n(11)]
    pub landed_tx: Option<TxHash>,
    /// The chain the flow's transaction is on, once it has one.
    #[n(12)]
    pub chain_id: Option<ChainId>,
    /// The flow's latest signed transaction.
    #[n(13)]
    pub tx_hash: Option<TxHash>,
    /// Whether that transaction is still waiting for its receipt.
    #[n(14)]
    pub tx_open: bool,
    /// The block its receipt is in, once it has one.
    #[n(15)]
    pub tx_block: Option<BlockNumber>,
    /// The continuation's route action and the USDC it spends, while one is out.
    #[n(16)]
    pub continuation: Option<(u8, TokenAmount)>,
    /// How many continuations reverted: the first failure re-plans as a Deliver.
    #[n(17)]
    pub failed_continuations: u32,
    /// What the user was paid on ICP, once paid.
    #[n(18)]
    pub delivered: Option<TokenAmount>,
    /// Why the flow refunded or froze.
    #[n(19)]
    pub reason: Option<String>,
    /// When the flow was registered.
    #[n(20)]
    pub created_at: Timestamp,
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
    #[error("transfer {0} is already credited to flow {1}")]
    LandedTwice(TxHash, QuoteHash),
    #[error("the continuation spends {spends}, above the {held} that landed for the quote")]
    AboveHeld {
        spends: TokenAmount,
        held: TokenAmount,
    },
    #[error("the receipt names {0}, which is not the flow's transaction")]
    NotTheFlowsTx(TxHash),
    #[error("a flow amount overflowed")]
    Overflow,
}

impl Flow {
    fn new(kind: FlowKind, status: FlowStatus, at: Timestamp) -> Self {
        Self {
            kind,
            status,
            icp_quote: None,
            evm_quote: None,
            held: TokenAmount::ZERO,
            held_ledger: None,
            held_in_subaccount: false,
            begun_at: None,
            withdrawal_id: None,
            withdrawn: None,
            eth_held: TokenAmount::ZERO,
            landed_tx: None,
            chain_id: None,
            tx_hash: None,
            tx_open: false,
            tx_block: None,
            continuation: None,
            failed_continuations: 0,
            delivered: None,
            reason: None,
            created_at: at,
        }
    }

    /// The ledger of what the flow holds, as raw principal bytes.
    pub fn held_ledger(&self) -> Option<&[u8]> {
        self.held_ledger.as_deref().map(|bytes| bytes.as_slice())
    }

    /// Every rule a step on an existing flow, or a step that creates one, must meet.
    /// `existing` is the flow the store holds under `quote_hash`, and `landed_elsewhere`
    /// answers which other flow a transfer is already credited to.
    pub fn admits(
        quote_hash: QuoteHash,
        existing: Option<&Flow>,
        step: &FlowStep,
        landed_elsewhere: impl Fn(&TxHash) -> Option<QuoteHash>,
    ) -> Result<(), FlowError> {
        use FlowStatus::*;
        let creates = matches!(
            step,
            FlowStep::IcpQuoteRegistered { .. }
                | FlowStep::EvmQuoteRegistered { .. }
                | FlowStep::GaslessRelayed { .. }
        );
        if creates {
            if existing.is_some() {
                return Err(FlowError::Exists(quote_hash));
            }
            let computed = match step {
                FlowStep::IcpQuoteRegistered { quote } => Some(quote.hash()),
                FlowStep::EvmQuoteRegistered { quote } => Some(quote.hash()),
                // the relay's hash is the router's EIP-712 digest, recomputed by the door
                _ => None,
            };
            if let Some(computed) = computed.filter(|computed| *computed != quote_hash) {
                return Err(FlowError::HashMismatch {
                    declared: quote_hash,
                    computed,
                });
            }
            return Ok(());
        }
        let flow = existing.ok_or(FlowError::Unknown(quote_hash))?;
        let refused = || FlowError::NotFromHere {
            quote_hash,
            status: flow.status,
            step: step.name(),
        };
        let kind = flow.kind;
        let ok = match step {
            FlowStep::IcpQuoteRegistered { .. }
            | FlowStep::EvmQuoteRegistered { .. }
            | FlowStep::GaslessRelayed { .. } => unreachable!("handled above"),
            FlowStep::Begun { stage } => match (kind, stage, flow.status) {
                (FlowKind::IcpToEvm, Stage::Pull, Registered) => true,
                (FlowKind::IcpToEvm, Stage::Swap, Pulled) => true,
                (FlowKind::IcpToEvm, Stage::Withdraw, Pulled | Swapped) => true,
                (FlowKind::IcpToEvm, Stage::Refund, RefundDue) => true,
                (FlowKind::EvmToIcp, Stage::Consolidate, Arrived) => true,
                (FlowKind::EvmToIcp, Stage::Swap, Consolidated) => true,
                (FlowKind::EvmToIcp, Stage::Deliver, Arrived | Swapped) => true,
                _ => false,
            },
            FlowStep::Pulled { .. } | FlowStep::PullRefused { .. } => flow.status == Pulling,
            FlowStep::Swapped { .. } => flow.status == Swapping,
            FlowStep::SwapSkipped { .. } => {
                kind == FlowKind::EvmToIcp && flow.status == Consolidated
            }
            FlowStep::Withdrawn { .. } => flow.status == Withdrawing,
            FlowStep::Landed { tx_hash, .. } => {
                if let Some(other) = landed_elsewhere(tx_hash) {
                    return Err(FlowError::LandedTwice(*tx_hash, other));
                }
                flow.status == Withdrawn
            }
            FlowStep::ContinuationSigned { amount, .. } => {
                if *amount > flow.eth_held {
                    return Err(FlowError::AboveHeld {
                        spends: *amount,
                        held: flow.eth_held,
                    });
                }
                flow.status == Landed
            }
            FlowStep::TxClosed { tx_hash, .. } => {
                // the outbox closes on whichever of a nonce's transactions landed, a
                // replacement included, so the hash is recorded rather than matched
                let _ = tx_hash;
                flow.tx_open && matches!(flow.status, RelaySigned | Continuing)
            }
            FlowStep::RefundStarted { .. } => {
                kind == FlowKind::IcpToEvm
                    && matches!(flow.status, Pulled | Swapped | Withdrawing | Swapping)
            }
            FlowStep::Refunded { .. } => flow.status == Refunding,
            FlowStep::Arrived { .. } => kind == FlowKind::EvmToIcp && flow.status == Registered,
            FlowStep::Consolidated { .. } => flow.status == Consolidating,
            FlowStep::Delivered { .. } => flow.status == Delivering,
            FlowStep::Frozen { .. } => !flow.status.is_closed() && !flow.tx_open,
            FlowStep::Expired => flow.status == Registered,
        };
        if !ok {
            return Err(refused());
        }
        Ok(())
    }

    /// Records a step [`Flow::admits`] admitted. `ckusdc` is the ledger a Landed flow's
    /// withdrawal was made from, recorded so the flow says what it holds.
    pub fn apply(existing: Option<Flow>, step: &FlowStep, at: Timestamp) -> Flow {
        use FlowStatus::*;
        let mut flow = match (existing, step) {
            (None, FlowStep::IcpQuoteRegistered { quote }) => {
                let mut flow = Flow::new(FlowKind::IcpToEvm, Registered, at);
                flow.icp_quote = Some(quote.clone());
                return flow;
            }
            (None, FlowStep::EvmQuoteRegistered { quote }) => {
                let mut flow = Flow::new(FlowKind::EvmToIcp, Registered, at);
                flow.evm_quote = Some(quote.clone());
                return flow;
            }
            (
                None,
                FlowStep::GaslessRelayed {
                    chain_id, tx_hash, ..
                },
            ) => {
                let mut flow = Flow::new(FlowKind::Relay, RelaySigned, at);
                flow.chain_id = Some(*chain_id);
                flow.tx_hash = Some(*tx_hash);
                flow.tx_open = true;
                return flow;
            }
            (Some(flow), _) => flow,
            (None, _) => unreachable!("BUG: Flow::admits refuses a step on no flow"),
        };
        match step {
            FlowStep::IcpQuoteRegistered { .. }
            | FlowStep::EvmQuoteRegistered { .. }
            | FlowStep::GaslessRelayed { .. } => {}
            FlowStep::Begun { stage } => {
                flow.status = stage.status();
                flow.begun_at = Some(at);
            }
            FlowStep::Pulled { amount, .. } => {
                flow.status = Pulled;
                flow.held = *amount;
                flow.held_ledger = flow
                    .icp_quote
                    .as_ref()
                    .map(|quote| quote.src_ledger.clone().into());
                flow.held_in_subaccount = false;
                flow.begun_at = None;
            }
            FlowStep::PullRefused { reason } => {
                flow.status = Registered;
                flow.reason = Some(reason.clone());
                flow.begun_at = None;
            }
            FlowStep::Swapped { ledger, amount } => {
                flow.status = Swapped;
                flow.held = *amount;
                flow.held_ledger = Some(ledger.clone().into());
                flow.held_in_subaccount = false;
                flow.begun_at = None;
            }
            FlowStep::SwapSkipped { reason } => {
                // the ckUSDC stays held, and is what the user is paid
                flow.status = Swapped;
                flow.reason = Some(reason.clone());
            }
            FlowStep::Withdrawn {
                withdrawal_id,
                amount,
            } => {
                flow.status = Withdrawn;
                flow.withdrawal_id = Some(*withdrawal_id);
                flow.withdrawn = Some(*amount);
                flow.held = TokenAmount::ZERO;
                flow.begun_at = None;
            }
            FlowStep::Landed {
                chain_id,
                tx_hash,
                amount,
            } => {
                flow.status = Landed;
                flow.chain_id = Some(*chain_id);
                flow.landed_tx = Some(*tx_hash);
                flow.eth_held = flow
                    .eth_held
                    .checked_add(*amount)
                    .unwrap_or(TokenAmount::MAX);
            }
            FlowStep::ContinuationSigned {
                chain_id,
                tx_hash,
                action,
                amount,
                ..
            } => {
                flow.status = Continuing;
                flow.chain_id = Some(*chain_id);
                flow.tx_hash = Some(*tx_hash);
                flow.tx_open = true;
                flow.continuation = Some((*action, *amount));
            }
            FlowStep::TxClosed {
                tx_hash,
                block,
                success,
            } => {
                flow.tx_open = false;
                flow.tx_hash = Some(*tx_hash);
                flow.tx_block = Some(*block);
                match (flow.kind, success) {
                    (FlowKind::Relay, true) => flow.status = RelayConfirmed,
                    (FlowKind::Relay, false) => flow.status = RelayFailed,
                    (_, true) => {
                        let spent = flow.continuation.map(|(_, amount)| amount);
                        flow.eth_held = spent
                            .and_then(|spent| flow.eth_held.checked_sub(spent))
                            .unwrap_or(TokenAmount::ZERO);
                        flow.status = Done;
                    }
                    (_, false) => {
                        // a revert spent nothing but gas: the USDC is still held, and the
                        // next plan falls back to a Deliver
                        flow.failed_continuations = flow.failed_continuations.saturating_add(1);
                        flow.continuation = None;
                        flow.status = Landed;
                    }
                }
            }
            FlowStep::RefundStarted { reason } => {
                flow.status = RefundDue;
                flow.reason = Some(reason.clone());
                flow.begun_at = None;
            }
            FlowStep::Refunded { .. } => {
                flow.status = Refunded;
                flow.held = TokenAmount::ZERO;
                flow.begun_at = None;
            }
            FlowStep::Arrived { ledger, amount } => {
                flow.status = Arrived;
                flow.held = *amount;
                flow.held_ledger = Some(ledger.clone().into());
                flow.held_in_subaccount = true;
            }
            FlowStep::Consolidated { amount } => {
                flow.status = Consolidated;
                flow.held = *amount;
                flow.held_in_subaccount = false;
                flow.begun_at = None;
            }
            FlowStep::Delivered { amount, .. } => {
                flow.status = Done;
                flow.delivered = Some(*amount);
                flow.held = TokenAmount::ZERO;
                flow.begun_at = None;
            }
            FlowStep::Frozen { reason } => {
                flow.status = Frozen;
                flow.reason = Some(reason.clone());
            }
            FlowStep::Expired => flow.status = Expired,
        }
        flow
    }
}

crate::storable_as_cbor!(Flow);
