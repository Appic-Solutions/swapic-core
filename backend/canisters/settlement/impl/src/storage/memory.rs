use ic_stable_structures::memory_manager::{MemoryId, MemoryManager, VirtualMemory};
use ic_stable_structures::DefaultMemoryImpl;
use std::cell::RefCell;

pub type Memory = VirtualMemory<DefaultMemoryImpl>;

thread_local! {
    static MEMORY_MANAGER: RefCell<MemoryManager<DefaultMemoryImpl>> =
        RefCell::new(MemoryManager::init(DefaultMemoryImpl::default()));
}

// assigned once, never renumbered, never reused for anything else
const EVENTS_INDEX_MEMORY_ID: MemoryId = MemoryId::new(0);
const EVENTS_DATA_MEMORY_ID: MemoryId = MemoryId::new(1);
const CONFIG_MEMORY_ID: MemoryId = MemoryId::new(2);
const ROLES_MEMORY_ID: MemoryId = MemoryId::new(3);
const HALT_MEMORY_ID: MemoryId = MemoryId::new(4);
// 5 is reserved for the Plan 3 liveness stamp
const SWAPS_MEMORY_ID: MemoryId = MemoryId::new(6);
const POCKETS_MEMORY_ID: MemoryId = MemoryId::new(7);
const LEDGER_META_MEMORY_ID: MemoryId = MemoryId::new(8);
const PENDING_QUOTES_MEMORY_ID: MemoryId = MemoryId::new(9);
const AUTO_REFUND_WAITING_MEMORY_ID: MemoryId = MemoryId::new(10);
const AUDIT_CURSOR_MEMORY_ID: MemoryId = MemoryId::new(11);
const PENDING_EXPIRY_MEMORY_ID: MemoryId = MemoryId::new(12);
const REPLAY_CURSOR_MEMORY_ID: MemoryId = MemoryId::new(13);
const CHAIN_DATA_MEMORY_ID: MemoryId = MemoryId::new(14);
const NONCES_MEMORY_ID: MemoryId = MemoryId::new(15);
const OUTBOX_MEMORY_ID: MemoryId = MemoryId::new(16);
// 17 to 19 are reserved for the rest of Plan 3: attestation inbox, in-flight markers,
// sanctions
const ECDSA_ADDRESS_MEMORY_ID: MemoryId = MemoryId::new(20);
const UNSIGNED_NONCES_MEMORY_ID: MemoryId = MemoryId::new(21);

fn memory(id: MemoryId) -> Memory {
    MEMORY_MANAGER.with(|m| m.borrow().get(id))
}

pub fn events_index_memory() -> Memory {
    memory(EVENTS_INDEX_MEMORY_ID)
}

pub fn events_data_memory() -> Memory {
    memory(EVENTS_DATA_MEMORY_ID)
}

pub fn config_memory() -> Memory {
    memory(CONFIG_MEMORY_ID)
}

pub fn roles_memory() -> Memory {
    memory(ROLES_MEMORY_ID)
}

pub fn halt_memory() -> Memory {
    memory(HALT_MEMORY_ID)
}

pub fn swaps_memory() -> Memory {
    memory(SWAPS_MEMORY_ID)
}

pub fn pockets_memory() -> Memory {
    memory(POCKETS_MEMORY_ID)
}

pub fn ledger_meta_memory() -> Memory {
    memory(LEDGER_META_MEMORY_ID)
}

pub fn pending_quotes_memory() -> Memory {
    memory(PENDING_QUOTES_MEMORY_ID)
}

pub fn auto_refund_waiting_memory() -> Memory {
    memory(AUTO_REFUND_WAITING_MEMORY_ID)
}

pub fn audit_cursor_memory() -> Memory {
    memory(AUDIT_CURSOR_MEMORY_ID)
}

pub fn pending_expiry_memory() -> Memory {
    memory(PENDING_EXPIRY_MEMORY_ID)
}

pub fn replay_cursor_memory() -> Memory {
    memory(REPLAY_CURSOR_MEMORY_ID)
}

pub fn chain_data_memory() -> Memory {
    memory(CHAIN_DATA_MEMORY_ID)
}

pub fn nonces_memory() -> Memory {
    memory(NONCES_MEMORY_ID)
}

pub fn outbox_memory() -> Memory {
    memory(OUTBOX_MEMORY_ID)
}

pub fn ecdsa_address_memory() -> Memory {
    memory(ECDSA_ADDRESS_MEMORY_ID)
}

pub fn unsigned_nonces_memory() -> Memory {
    memory(UNSIGNED_NONCES_MEMORY_ID)
}
