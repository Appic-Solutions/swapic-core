use super::*;

#[test]
fn text_is_kept_exactly_as_it_arrived() {
    let checksummed = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
    let address: Address = checksummed.parse().unwrap();
    assert_eq!(
        address.as_str(),
        checksummed,
        "no lowercasing, no checksumming"
    );
    assert_eq!(address.to_string(), checksummed);
    let token: TokenId = "USDC".parse().unwrap();
    assert_eq!(token.as_ref(), "USDC");
}

#[test]
fn text_is_capped_in_bytes_not_chars() {
    let at_cap = "a".repeat(MAX_TEXT_BYTES);
    assert!(at_cap.parse::<Address>().is_ok());
    assert_eq!(
        "a".repeat(MAX_TEXT_BYTES + 1).parse::<TokenId>(),
        Err(TextTooLong {
            len: MAX_TEXT_BYTES + 1
        })
    );
    // 129 two-byte chars: under the cap in chars, over it in bytes
    assert_eq!(
        "é".repeat(129).parse::<Address>(),
        Err(TextTooLong { len: 258 })
    );
}

#[test]
fn empty_text_is_representable() {
    // the canonical layouts give the empty string a meaning of its own, so the rule
    // for it lives with each layout rather than here
    assert_eq!("".parse::<Address>().unwrap().as_str(), "");
}

const SECRET: &str = "https://eth-mainnet.g.alchemy.com/v2/secret-key";

#[test]
fn an_rpc_url_never_prints_its_secret() {
    let url: RpcUrl = SECRET.parse().unwrap();
    assert_eq!(url.to_string(), REDACTED);
    assert_eq!(format!("{url:?}"), REDACTED);
    assert_eq!(format!("{:?}", Some(&url)), "Some(***)");
    assert_eq!(url.expose(), SECRET);
}

#[test]
fn the_redaction_placeholder_is_not_an_rpc_url() {
    assert_eq!(REDACTED.parse::<RpcUrl>(), Err(RedactedRpcUrl));
}
