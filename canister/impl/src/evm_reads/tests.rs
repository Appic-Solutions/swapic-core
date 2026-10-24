use super::*;

/// A call's answer is one word only when it returned exactly 32 bytes.
#[test]
fn a_word_is_exactly_32_returned_bytes() {
    let word = [7; 32];
    assert_eq!(Simulated::Returned(word.to_vec()).word(), Some(word));
    assert_eq!(Simulated::Returned(vec![7; 31]).word(), None);
    assert_eq!(Simulated::Returned(vec![7; 33]).word(), None);
    assert_eq!(Simulated::Returned(vec![]).word(), None);
    assert_eq!(Simulated::Reverted("no".to_string()).word(), None);
}
