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
