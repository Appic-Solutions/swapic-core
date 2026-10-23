use super::*;
use crate::storage::on_fresh_memory;

/// Review fix 10: the ICP flows still sign router ABI v2's `swapFor`, which a design C
/// Swapic does not have, so they cannot be turned on until they move to ABI v3 (P11): a
/// setting that asks for them is refused, and a stored one never reads as on.
#[test]
fn the_icp_flows_cannot_be_turned_on_before_abi_v3() {
    on_fresh_memory(|| {
        init();
        let on = Wire {
            icp_enabled: Some(true),
            ..Wire::default()
        };
        let refused = set(on);
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.starts_with("IcpFlowsNotOnV3")),
            "{refused:?}"
        );
        assert!(!get().icp_enabled);
        let off = Wire {
            icp_enabled: Some(false),
            ..Wire::default()
        };
        assert_eq!(set(off), Ok(()));
        assert!(!get().icp_enabled);
    });
}

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
                icp_enabled: Some(true),
                ..Wire::default()
            })
            .unwrap(),
        );
        assert!(check().is_err());
        assert!(std::panic::catch_unwind(get).is_err());
    });
}
