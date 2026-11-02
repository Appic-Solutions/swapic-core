use super::*;
use crate::storage::on_fresh_memory;

/// L-08: stored settings that no longer decode, or no longer parse under this build's
/// rules, are never read as the defaults: `check` refuses them (the upgrade traps on it and
/// keeps the wasm it has) and a read traps. A cell never written is the defaults, as hfisl's
/// is.
#[test]
fn stored_settings_that_do_not_decode_or_parse_are_never_the_defaults() {
    on_fresh_memory(|| {
        init();
        assert_eq!(check(), Ok(()));
        assert_eq!(get_wire(), Wire::default());
        let plant = |bytes: Vec<u8>| SETTINGS.with(|cell| cell.borrow_mut().set(bytes).unwrap());
        // bytes that are no settings
        plant(vec![0xde, 0xad]);
        assert!(check().is_err());
        assert!(std::panic::catch_unwind(get_wire).is_err());
        assert!(std::panic::catch_unwind(get).is_err());
        // settings this build's rules no longer take
        plant(
            Encode!(&Wire {
                ck_helper: Some("not an address".to_string()),
                ..Wire::default()
            })
            .unwrap(),
        );
        assert!(check().is_err());
        assert!(std::panic::catch_unwind(get).is_err());
    });
}

/// Review of the merged canister (item 5): a read of the parsed settings parses the stored
/// ones once.
#[test]
fn a_read_parses_the_stored_settings_once() {
    on_fresh_memory(|| {
        init();
        assert_eq!(set(Wire::default()), Ok(()));
        PARSES.with(|parses| parses.set(0));
        let _ = get();
        assert_eq!(PARSES.with(|parses| parses.get()), 1);
    });
}
