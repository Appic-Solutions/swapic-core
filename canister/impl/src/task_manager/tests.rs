use super::*;

#[test]
fn interval_never_falls_below_a_second() {
    let secs = Duration::from_secs;
    assert_eq!(interval(secs(0)), secs(1), "a zero interval spins");
    assert_eq!(interval(Duration::from_millis(999)), secs(1));
    assert_eq!(interval(secs(1)), secs(1));
    assert_eq!(interval(secs(21_600)), secs(21_600));
}
