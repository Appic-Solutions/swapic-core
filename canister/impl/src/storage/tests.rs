//! The two stable cells that hold candid, not minicbor (C-11): the roles and the flow
//! settings. Their bytes are pinned the way `types/golden/storage_v1.txt` pins every minicbor
//! layout: every golden line still decodes to its sample under this build, and the file only
//! grows. A candid record that gains an `opt` field encodes differently, so a line is only
//! ever decoded, never compared with a new encoding.

use super::flow_settings;
use super::roles::Roles;
use candid::{Decode, Encode, Nat, Principal};
use ic_stable_structures::Storable;
use settlement_api::types::flow::FlowSettings as Wire;
use std::borrow::Cow;

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/golden/candid_cells_v1.txt");

/// Asserts that golden bytes decode, and parse, to a sample.
type DecodeCheck = Box<dyn Fn(&[u8])>;

struct Sample {
    name: &'static str,
    bytes: Vec<u8>,
    decodes_from: DecodeCheck,
}

fn roles(name: &'static str, roles: Roles) -> Sample {
    Sample {
        name,
        bytes: roles.to_bytes().into_owned(),
        decodes_from: Box::new(move |golden| {
            assert_eq!(Roles::from_bytes(Cow::Borrowed(golden)), roles, "{name}")
        }),
    }
}

fn flow_settings(name: &'static str, wire: Wire) -> Sample {
    Sample {
        name,
        bytes: Encode!(&wire).unwrap(),
        decodes_from: Box::new(move |golden| {
            let decoded = Decode!(golden, Wire).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(decoded, wire, "{name}");
            flow_settings::parse_for_tests(&decoded).unwrap_or_else(|e| panic!("{name}: {e}"));
        }),
    }
}

fn samples() -> Vec<Sample> {
    let principal = |byte: u8| Principal::from_slice(&[byte; 10]);
    vec![
        roles("roles unset", Roles::default()),
        roles(
            "roles set",
            Roles {
                quoter: Some(principal(0x01)),
                watcher: Some(principal(0x02)),
            },
        ),
        flow_settings(
            "flow settings, every field",
            Wire {
                ck_minter: Some(principal(0x04)),
                cketh_ledger: Some(principal(0x05)),
                cketh_fee_allowance: Nat::from(6_000_000_000_000_000_u64),
                relay_window_s: 3_600,
                ck_helper: Some("0x18901044688D3756C35Ed2b36D93e6a5B8e00E68".to_string()),
                eco_prover: Some("0xEC08fb4647f3f50d1162a578d481266687C60fc5".to_string()),
            },
        ),
    ]
}

/// Every candid cell's stored bytes decode, and parse, under this build: a change that
/// breaks them fails here, in CI, and not at an upgrade.
#[test]
fn every_candid_cell_decodes_from_the_golden_file() {
    let samples = samples();
    // regeneration is opt-in and never green, so a blessing is always a deliberate diff
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        let lines: Vec<String> = samples.iter().map(|s| hex::encode(&s.bytes)).collect();
        std::fs::write(GOLDEN, lines.join("\n") + "\n").unwrap();
        panic!("golden regenerated, inspect the diff and rerun: {GOLDEN}");
    }
    let raw = std::fs::read_to_string(GOLDEN).unwrap_or_else(|e| {
        panic!("golden missing or unreadable ({e}); regenerate with UPDATE_GOLDEN=1: {GOLDEN}")
    });
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(
        lines.len(),
        samples.len(),
        "a new sample appends a line, an old line never changes"
    );
    for (sample, line) in samples.iter().zip(lines) {
        let golden = hex::decode(line)
            .unwrap_or_else(|e| panic!("{}: golden line is not hex ({e})", sample.name));
        (sample.decodes_from)(&golden);
    }
}
