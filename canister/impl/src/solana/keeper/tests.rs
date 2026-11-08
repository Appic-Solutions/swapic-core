use super::*;

const NOW: u64 = 1_800_000_000;

#[test]
fn only_a_recorded_ref_past_its_five_days_with_a_try_left_is_reclaimed() {
    assert!(reclaimable(None, NOW).is_err());
    assert!(reclaimable(Some((NOW - RECLAIM_AFTER_S + 1, 0)), NOW).is_err());
    assert!(reclaimable(Some((NOW - RECLAIM_AFTER_S, 0)), NOW).is_ok());
    assert!(reclaimable(Some((NOW - RECLAIM_AFTER_S, 1)), NOW).is_ok());
    assert!(reclaimable(Some((NOW - RECLAIM_AFTER_S, 2)), NOW).is_err());
}

#[test]
fn a_top_up_comes_only_below_the_floor_up_to_the_target_once_an_hour() {
    assert_eq!(top_up(200, 200, 500, None, NOW), None);
    assert_eq!(top_up(199, 200, 500, None, NOW), Some(301));
    assert_eq!(top_up(0, 200, 500, Some(NOW - 3599), NOW), None);
    assert_eq!(top_up(0, 200, 500, Some(NOW - 3600), NOW), Some(500));
}
