use crate::storage::events;
use ic_cdk::query;
pub use settlement_api::types::events::Event;

/// How long a line's transaction bytes stay hidden from a public reader: by then what was
/// signed is long sent and mined (audit C5). The watcher and the controllers read them at once.
pub const HIDE_TX_BYTES_NS: u64 = 3_600 * 1_000_000_000;

/// The most reply bytes one page carries, well under the 3 MiB a reply may hold: a page is
/// cut by its size as well as its count, and always holds at least one line (audit C15).
pub const MAX_PAGE_BYTES: usize = 1_500_000;

/// `len` is capped server-side; ask for the count first and page through. A page may be
/// shorter than asked (its size): read on until a page comes back empty.
#[query]
pub fn events_page(start: u64, len: u64) -> Vec<Event> {
    let sees_all = crate::guards::require_watcher_or_controller().is_ok();
    page_of(
        events::events_page(start, len),
        sees_all,
        ic_cdk::api::time(),
    )
}

/// The wire page of `lines`: a young line's transaction bytes emptied for a reader who does
/// not see all, and the page cut once its encoded size passes [`MAX_PAGE_BYTES`].
pub fn page_of(lines: Vec<types::events::Event>, sees_all: bool, now_ns: u64) -> Vec<Event> {
    let mut page = Vec::new();
    let mut bytes = 0usize;
    for mut line in lines {
        if !sees_all && now_ns.saturating_sub(line.timestamp.as_nanos()) < HIDE_TX_BYTES_NS {
            line.payload = line.payload.without_tx_bytes();
        }
        let wire = Event::from(line);
        let size = candid::encode_one(&wire)
            .map(|encoded| encoded.len())
            .unwrap_or(usize::MAX);
        if !page.is_empty() && bytes.saturating_add(size) > MAX_PAGE_BYTES {
            break;
        }
        bytes = bytes.saturating_add(size);
        page.push(wire);
    }
    page
}
