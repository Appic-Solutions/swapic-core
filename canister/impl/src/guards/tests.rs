use super::*;

fn p(b: u8) -> Principal {
    Principal::from_slice(&[b; 29])
}

/// The rule the `require_*` wrappers are made of. They read the caller from the
/// system, which only exists inside a canister, so the rule is tested here instead.
#[test]
fn check_refuses_when_unset_and_when_the_caller_is_wrong() {
    assert_eq!(check(Some(p(2)), p(2), Role::Quoter), Ok(()));
    assert_eq!(
        check(None, p(2), Role::Quoter),
        Err(GuardError::RoleNotSet(Role::Quoter)),
        "an unset role refuses everyone"
    );
    assert_eq!(
        check(Some(p(2)), p(9), Role::Quoter),
        Err(GuardError::CallerNotRole(Role::Quoter)),
        "the wrong caller refuses"
    );
}

/// The either-service rule, including the arm a fresh install sits in: unset roles
/// refuse both services rather than falling open to everyone.
#[test]
fn check_either_refuses_when_neither_role_is_set() {
    assert_eq!(
        check_either(&Roles::default(), p(2)),
        Err(GuardError::RolesNotSet),
        "nothing is set yet"
    );

    let set = Roles {
        quoter: Some(p(2)),
        watcher: Some(p(3)),
    };
    assert_eq!(check_either(&set, p(2)), Ok(()), "the quoter passes");
    assert_eq!(check_either(&set, p(3)), Ok(()), "so does the watcher");
    assert_eq!(
        check_either(&set, p(9)),
        Err(GuardError::CallerNotQuoterOrWatcher),
        "a stranger does not, and a set role is not an unset one"
    );
}

/// An error a stranger reads must not name the principal that would have worked.
#[test]
fn a_refusal_names_the_role_and_never_a_principal() {
    let holder = p(2);
    let err = check(Some(holder), p(9), Role::Quoter).unwrap_err();
    let shown = format!("{err:?}");
    assert!(
        !shown.contains(&holder.to_text()),
        "leaked the holder: {shown}"
    );
    assert!(
        !shown.contains(&p(9).to_text()),
        "echoed the caller: {shown}"
    );
    assert!(shown.contains("Quoter"), "names the role: {shown}");
}

/// The watcher-or-controller rule: a controller passes whether or not the watcher is set, the
/// watcher passes, and anyone else is refused without being told which it failed to be.
#[test]
fn check_watcher_or_controller_admits_either_and_nobody_else() {
    assert_eq!(check_watcher_or_controller(None, p(1), true), Ok(()));
    assert_eq!(check_watcher_or_controller(Some(p(3)), p(3), false), Ok(()));
    assert_eq!(
        check_watcher_or_controller(None, p(3), false),
        Err(GuardError::CallerNotWatcherOrController),
        "an unset watcher refuses everyone but a controller"
    );
    assert_eq!(
        check_watcher_or_controller(Some(p(3)), p(2), false),
        Err(GuardError::CallerNotWatcherOrController),
        "the quoter is neither"
    );
}
