use super::*;

#[test]
fn interval_never_falls_below_a_second() {
    assert_eq!(interval(0), Duration::from_secs(1), "a zero interval spins");
    assert_eq!(interval(1), Duration::from_secs(1));
    assert_eq!(interval(21_600), Duration::from_secs(21_600));
}
