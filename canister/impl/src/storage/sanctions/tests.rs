use super::*;
use crate::storage::on_fresh_memory;

fn address(text: &str) -> Address {
    text.parse().expect("a test address is inside the bound")
}

/// An EVM address is the same address in any case, so the set answers for the bytes and
/// not the spelling; text that is no EVM address is held exactly as it came, because a
/// base58 address differs by case.
#[test]
fn an_evm_address_is_sanctioned_in_any_case_and_other_text_exactly() {
    on_fresh_memory(|| {
        init();
        let checksummed = address("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        let lower = address("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913");
        let upper = address("0x833589FCD6EDB6E08F4C7C32D4F71B54BDA02913");
        assert!(
            !is_sanctioned(&checksummed),
            "nothing is sanctioned at first"
        );
        assert_eq!(apply(std::slice::from_ref(&checksummed), &[]), Ok(1));
        assert!(is_sanctioned(&checksummed));
        assert!(is_sanctioned(&lower), "the same bytes, spelled lower");
        assert!(is_sanctioned(&upper), "the same bytes, spelled upper");
        assert_eq!(
            apply(std::slice::from_ref(&lower), &[]),
            Ok(1),
            "the same address again is one entry"
        );

        let solana = address("7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU");
        let flipped = address("7xkxtg2cw87d97txjsdpbd5jbkhetqa83tzrujosgasu");
        assert_eq!(apply(std::slice::from_ref(&solana), &[]), Ok(2));
        assert!(is_sanctioned(&solana));
        assert!(
            !is_sanctioned(&flipped),
            "text that is not an EVM address is another address in another case"
        );

        // removal answers for the bytes as well, and the count follows
        assert_eq!(apply(&[], &[upper]), Ok(1));
        assert!(!is_sanctioned(&checksummed));
        assert!(is_sanctioned(&solana));
        assert_eq!(
            apply(&[], &[flipped]),
            Ok(1),
            "removing what is not held removes nothing"
        );
        assert_eq!(apply(&[], &[solana]), Ok(0));
    });
}

/// One call adds and then removes, so an address in both lists ends up absent, and the
/// count answered is what the set holds when the call is done.
#[test]
fn additions_land_before_removals_and_the_count_is_what_is_left() {
    on_fresh_memory(|| {
        init();
        let a = address("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let b = address("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(
            apply(&[a.clone(), b.clone()], std::slice::from_ref(&a)),
            Ok(1)
        );
        assert!(!is_sanctioned(&a));
        assert!(is_sanctioned(&b));
        assert_eq!(len(), 1);
    });
}

/// The set is bounded, because the watcher is a service and not a controller: a call that
/// would take it past the cap is refused whole, so nothing of it lands, and one that only
/// re-adds or removes still goes through at the cap.
#[test]
fn the_set_refuses_to_grow_past_its_cap_and_writes_nothing_of_the_call_that_would() {
    on_fresh_memory(|| {
        init();
        let nth = |n: u64| address(&format!("0x{n:040x}"));
        let full: Vec<Address> = (0..MAX_SANCTIONED).map(nth).collect();
        assert_eq!(apply(&full, &[]), Ok(MAX_SANCTIONED));
        let over = nth(MAX_SANCTIONED);
        let also_over = nth(MAX_SANCTIONED + 1);
        assert_eq!(
            apply(&[over.clone(), also_over.clone()], &[]),
            Err(SanctionsError::SetFull {
                capacity: MAX_SANCTIONED
            })
        );
        assert!(!is_sanctioned(&over), "a refused call writes nothing");
        assert!(!is_sanctioned(&also_over));
        assert_eq!(len(), MAX_SANCTIONED);

        // re-adding what is held and removing are not growth
        assert_eq!(apply(&[nth(0)], &[nth(1)]), Ok(MAX_SANCTIONED - 1));
        // and one in, one out, in the same call fits again
        assert_eq!(
            apply(std::slice::from_ref(&over), &[nth(2)]),
            Ok(MAX_SANCTIONED - 1)
        );
        assert!(is_sanctioned(&over));
    });
}
