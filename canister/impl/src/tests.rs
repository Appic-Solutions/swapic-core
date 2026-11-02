/// The committed interface is the one the code exports: a door or a wire type changed
/// without `can.did` fails here. `UPDATE_DID=1` rewrites the file, and never passes, so a
/// change of interface is always a deliberate diff.
#[test]
fn the_committed_did_is_the_exported_one() {
    const DID: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../api/can.did");
    let exported = crate::__export_service();
    if std::env::var("UPDATE_DID").as_deref() == Ok("1") {
        std::fs::write(DID, &exported).unwrap();
        panic!("can.did rewritten, inspect the diff and rerun: {DID}");
    }
    let committed = std::fs::read_to_string(DID).unwrap();
    assert!(
        committed == exported,
        "can.did is not the exported interface: rerun with UPDATE_DID=1"
    );
}
