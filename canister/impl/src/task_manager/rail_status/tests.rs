use super::*;

/// The engine ticks on the interval the config names, and on the floor every timer here is
/// held to when the knob is below it: a zero interval would otherwise put the engine on
/// every round of the subnet. What wires the timer needs a canister; what decides how often
/// it fires does not, and this is that decision.
#[test]
fn the_engine_ticks_on_the_configured_interval() {
    let with = |rail_status_max_age| {
        every(&types::Config {
            rail_status_max_age,
            ..types::Config::default()
        })
    };
    assert_eq!(
        every(&types::Config::default()),
        Duration::from_secs(30),
        "the spec's own interval"
    );
    assert_eq!(with(Duration::from_secs(5)), Duration::from_secs(5));
    assert_eq!(
        with(Duration::ZERO),
        Duration::from_secs(1),
        "the floor, not every round of the subnet"
    );
}
