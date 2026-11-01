use super::*;

#[test]
fn a_domain_text_is_one_to_thirty_two_printable_ascii_characters() {
    for good in ["USD Coin", "USDC", "2", &"x".repeat(32)] {
        let text: DomainText = good.parse().unwrap();
        assert_eq!(text.as_str(), good);
        assert_eq!(text.to_string(), good);
    }
    for bad in ["", &"x".repeat(33), "USD\nCoin", "USDC\u{0}", "Ünicode"] {
        assert_eq!(bad.parse::<DomainText>(), Err(NotDomainText), "{bad:?}");
    }
}

#[test]
fn a_domain_text_is_stored_as_its_text() {
    let text: DomainText = "USDC".parse().unwrap();
    let bytes = minicbor::to_vec(text).unwrap();
    assert_eq!(bytes, minicbor::to_vec("USDC").unwrap());
    assert_eq!(minicbor::decode::<DomainText>(&bytes).unwrap(), text);
    let stored_bad = minicbor::to_vec("").unwrap();
    assert!(minicbor::decode::<DomainText>(&stored_bad).is_err());
}
