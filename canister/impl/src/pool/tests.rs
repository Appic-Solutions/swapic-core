use super::*;
use candid::Nat;

/// An empty pool is closed until it has settings and the books' guard opens, and shows
/// every line at zero with the launch caps.
#[test]
fn the_state_of_an_empty_pool() {
    let empty = PoolLedger::default();
    let view = state_view(&empty, closed_reason(Ok(()), &empty));
    assert!(!view.open);
    assert_eq!(
        view.closed_reason.as_deref(),
        Some("the pool has no settings")
    );
    assert_eq!(view.total_assets, Nat::from(0_u8));
    assert_eq!(view.total_shares, Nat::from(0_u8));
    assert_eq!(view.base.free, Nat::from(0_u8));
    assert_eq!(view.icp.reserved, Nat::from(0_u8));
    assert_eq!(view.tvl_cap, Nat::from(10_000_000_000_u64));
    assert_eq!(view.per_swap_cap, Nat::from(1_000_000_000_u64));
    assert_eq!(view.premium_bps, 10);
    // the books' guard speaks first
    let view = state_view(&empty, closed_reason(Err(PoolClosed::Paused), &empty));
    assert_eq!(
        view.closed_reason.as_deref(),
        Some("a controller paused the pool")
    );
    let set = PoolLedger {
        settings: Some(types::pool::PoolSettings::default()),
        ..PoolLedger::default()
    };
    let view = state_view(&set, closed_reason(Ok(()), &set));
    assert!(view.open);
    assert_eq!(view.closed_reason, None);
}
