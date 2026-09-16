use ic_stable_structures::memory_manager::{MemoryId, MemoryManager, VirtualMemory};
use ic_stable_structures::DefaultMemoryImpl;
use std::cell::RefCell;

pub type Memory = VirtualMemory<DefaultMemoryImpl>;

thread_local! {
    static MEMORY_MANAGER: RefCell<MemoryManager<DefaultMemoryImpl>> =
        RefCell::new(MemoryManager::init(DefaultMemoryImpl::default()));
}

// assigned once, never reused for anything else
const EVENTS_INDEX_MEMORY_ID: MemoryId = MemoryId::new(0);
const EVENTS_DATA_MEMORY_ID: MemoryId = MemoryId::new(1);
const CONFIG_MEMORY_ID: MemoryId = MemoryId::new(2);
const ROLES_MEMORY_ID: MemoryId = MemoryId::new(3);
const HALT_MEMORY_ID: MemoryId = MemoryId::new(4);
// 5 is reserved for the Plan 3 liveness stamp

// The canister's one memory manager; every stable structure takes its pages from here,
// under an id from the list above.

pub fn events_index_memory() -> Memory {
    MEMORY_MANAGER.with(|m| m.borrow().get(EVENTS_INDEX_MEMORY_ID))
}

pub fn events_data_memory() -> Memory {
    MEMORY_MANAGER.with(|m| m.borrow().get(EVENTS_DATA_MEMORY_ID))
}

pub fn config_memory() -> Memory {
    MEMORY_MANAGER.with(|m| m.borrow().get(CONFIG_MEMORY_ID))
}

pub fn roles_memory() -> Memory {
    MEMORY_MANAGER.with(|m| m.borrow().get(ROLES_MEMORY_ID))
}

pub fn halt_memory() -> Memory {
    MEMORY_MANAGER.with(|m| m.borrow().get(HALT_MEMORY_ID))
}
