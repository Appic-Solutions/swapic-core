use super::*;
use crate::state::MemoryStore;
use types::books::UserFeeKey;
use types::cctp::CctpDomain;
use types::config::ChainTable;
use types::{
    BlockNumber, Flow, FlowKind, FlowStatus, GasAmount, Nonce, QuoteHash, Timestamp, Wei, WeiPerGas,
};

const KEY: QuoteHash = QuoteHash::new([0x11; 32]);
const SOURCE_TX: TxHash = TxHash::new([0x22; 32]);
const CANISTER: EvmAddress = EvmAddress::new([0xca; 20]);

/// A real finish's calldata, from the live log: a Swap's completion, burned on Base (domain 6).
fn complete_swap() -> Vec<u8> {
    hex::decode(include_str!("samples/finish_complete_swap.hex").trim()).unwrap()
}

/// A real Deliver's `receiveMessage`, burned on Polygon (domain 7).
fn relay_deliver() -> Vec<u8> {
    hex::decode(include_str!("samples/finish_relay_deliver.hex").trim()).unwrap()
}

fn chain() -> EvmChain {
    EvmChain {
        swapic: EvmAddress::new([0x51; 20]),
        runner: EvmAddress::new([0x50; 20]),
        cctp_receiver: EvmAddress::new([0x52; 20]),
        usdc: EvmAddress::new([0x53; 20]),
        token_messenger: EvmAddress::new([0x54; 20]),
        message_transmitter: EvmAddress::new([0x55; 20]),
        gas_limit: types::config::PerAction::all(GasAmount::from(300_000_u64)),
        gas_budget: types::config::PerAction::all(Wei::ZERO),
        min_network_fee: types::config::PerAction::all(TokenAmount::ZERO),
        usdc_name: None,
        usdc_version: None,
        block_time: Duration::from_secs(2),
        usdc_decimals: None,
        across: None,
    }
}

fn tables() -> (ChainTable<EvmChain>, ChainTable<CctpDomain>) {
    let polygon = ChainId::new(137);
    let mut chains = ChainTable::default();
    chains.0.insert(ChainId::BASE, chain());
    chains.0.insert(polygon, chain());
    let mut domains = ChainTable::default();
    domains.0.insert(ChainId::BASE, CctpDomain::new(6));
    domains.0.insert(polygon, CctpDomain::new(7));
    (chains, domains)
}

fn created(purpose: TxPurpose, data: Vec<u8>) -> EventType {
    EventType::TxCreated {
        purpose,
        chain_id: ChainId::new(137),
        nonce: Nonce::new(1),
        to: EvmAddress::new([0x60; 20]),
        value: Wei::ZERO,
        data,
        gas_limit: GasAmount::from(300_000_u64),
        max_fee: WeiPerGas::ZERO,
        max_priority_fee: WeiPerGas::ZERO,
        action: None,
    }
}

fn finish_flow(source_tx: Option<TxHash>) -> Flow {
    Flow {
        kind: FlowKind::Finish,
        status: FlowStatus::FinishSigned,
        chain_id: None,
        tx_hash: None,
        tx_open: false,
        tx_block: None,
        created_at: Timestamp::from_nanos(1),
        signed_id: None,
        source_tx,
        arrival: None,
        gas_action: None,
    }
}

/// A store holding one finish flow whose source transaction is `SOURCE_TX`.
fn with_finish() -> MemoryStore {
    let mut store = MemoryStore::default();
    store.put_flow(KEY, finish_flow(Some(SOURCE_TX)));
    store
}

fn due(store: MemoryStore, line: &EventType) -> Option<Due> {
    let (chains, domains) = tables();
    let setup = Setup {
        chain_id: ChainId::BASE,
        evm_chains: &chains,
        domains: &domains,
        canister: CANISTER,
    };
    due_of(&State::new(store), line, &setup)
}

/// A real line names its source chain through the message in its own calldata.
#[test]
fn a_past_finish_names_its_source_chain_and_transaction() {
    let line = created(TxPurpose::Finish(KEY), complete_swap());
    let found = due(with_finish(), &line).expect("a past finish is due");
    assert_eq!(found.chain, ChainId::BASE);
    assert_eq!(found.tx, SOURCE_TX);

    let line = created(TxPurpose::Finish(KEY), relay_deliver());
    let found = due(with_finish(), &line).expect("a past finish is due");
    assert_eq!(found.chain, ChainId::new(137));
}

/// Only a finish spends a read.
#[test]
fn any_other_line_is_not_due() {
    let line = created(TxPurpose::Relay(KEY), complete_swap());
    assert_eq!(due(with_finish(), &line), None);
}

/// A finish that never signed has no source transaction yet.
#[test]
fn a_finish_with_no_source_is_not_due() {
    let mut store = MemoryStore::default();
    store.put_flow(KEY, finish_flow(None));
    assert_eq!(
        due(store, &created(TxPurpose::Finish(KEY), complete_swap())),
        None
    );
}

/// A gasless source was booked at the relay's close: no read is spent on it.
#[test]
fn a_relay_the_canister_signed_is_not_due() {
    let mut store = with_finish();
    store.put_flow_tx(SOURCE_TX, QuoteHash::new([0x33; 32]));
    assert_eq!(
        due(store, &created(TxPurpose::Finish(KEY), complete_swap())),
        None
    );
}

/// A transaction a live finish already booked is not read again.
#[test]
fn a_transaction_already_booked_is_not_due() {
    let mut store = with_finish();
    store.put_user_fee_booked(UserFeeKey {
        chain: ChainId::BASE,
        tx: SOURCE_TX,
    });
    assert_eq!(
        due(store, &created(TxPurpose::Finish(KEY), complete_swap())),
        None
    );
}

/// Calldata that does not read is passed by, never a trap in a timer.
#[test]
fn a_line_that_does_not_read_is_not_due() {
    assert_eq!(
        due(
            with_finish(),
            &created(TxPurpose::Finish(KEY), vec![1, 2, 3])
        ),
        None
    );
}

/// On the pool chain a past fee needs what the balance holds beyond the books: exactly enough
/// passes, a micro more does not, and USDC the pool has on its way out is not counted.
#[test]
fn a_past_fee_must_fit_the_surplus() {
    let state = State::new(MemoryStore::default());
    let config = types::Config {
        pool_chain: Some(ChainId::BASE),
        ..types::Config::default()
    };
    let holds = |amount: u32| {
        surplus_holds(
            &state,
            &config,
            ChainId::BASE,
            BlockNumber::new(10),
            TokenAmount::from(14_974_902_u32),
            TokenAmount::from(amount),
        )
    };
    assert!(holds(14_974_902));
    assert!(!holds(14_974_903));
    let mut ledger = types::pool::PoolLedger::default();
    ledger.pockets.base.in_flight = TokenAmount::from(1_u8);
    let mut store = MemoryStore::default();
    store.put_pool_ledger(ledger);
    let moving = State::new(store);
    assert!(!surplus_holds(
        &moving,
        &config,
        ChainId::BASE,
        BlockNumber::new(10),
        TokenAmount::from(14_974_902_u32),
        TokenAmount::from(14_974_902_u32),
    ));
}

/// The walk is done at its end, and an empty log has nothing to walk.
#[test]
fn the_walk_ends_at_the_end() {
    assert!(FeeCatchUp::default().is_done());
    let job = FeeCatchUp {
        next: 5,
        end: 6,
        ..FeeCatchUp::default()
    };
    assert!(!job.is_done());
    assert!(FeeCatchUp { next: 6, ..job }.is_done());
}
