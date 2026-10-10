use super::*;
use crate::storage::on_fresh_memory;

/// The cursor is forty fixed bytes, so the cell it lives in never moves, and a cursor read
/// back is the cursor written.
#[test]
fn a_cursor_round_trips_through_its_forty_bytes() {
    let cursor = AuditCursor {
        next_index: EventIndex::new(1_700_000),
        parent_hash: EventHash::new([0x5a; 32]),
    };
    let bytes = cursor.to_bytes();
    assert_eq!(bytes.len(), 40);
    assert_eq!(AuditCursor::from_bytes(bytes), cursor);
    assert_eq!(
        AuditCursor::from_bytes(AuditCursor::GENESIS.to_bytes()),
        AuditCursor::GENESIS
    );
}

/// A canister that has never audited starts at genesis, which is the anchor index 0 links
/// to, and a written cursor survives in stable memory.
#[test]
fn a_fresh_canister_starts_the_audit_at_genesis() {
    on_fresh_memory(|| {
        init();
        assert_eq!(get(), AuditCursor::GENESIS);
        let moved = AuditCursor {
            next_index: EventIndex::new(9),
            parent_hash: EventHash::new([7; 32]),
        };
        set(moved);
        assert_eq!(get(), moved);
    });
}
