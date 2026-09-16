use super::*;

fn p(b: u8) -> Principal {
    Principal::from_slice(&[b; 29])
}

/// The rule the `require_*` wrappers are made of. They read the caller from the
/// system, which only exists inside a canister, so the rule is tested here instead.
#[test]
fn check_refuses_when_unset_and_when_the_caller_is_wrong() {
    assert_eq!(check(Some(p(2)), p(2), "quoter"), Ok(()));

    let unset = check(None, p(2), "quoter").expect_err("an unset role refuses everyone");
    assert!(unset.contains("not set"), "{unset}");

    let wrong = check(Some(p(2)), p(9), "quoter").expect_err("the wrong caller refuses");
    assert!(wrong.contains("quoter"), "{wrong}");
}

/// The either-service rule, including the arm a fresh install sits in: unset roles
/// refuse both services rather than falling open to everyone.
#[test]
fn check_either_refuses_when_neither_role_is_set() {
    let unset = check_either(&Roles::default(), p(2)).expect_err("nothing is set yet");
    assert!(unset.contains("not set"), "{unset}");

    let set = Roles {
        quoter: Some(p(2)),
        watcher: Some(p(3)),
    };
    assert_eq!(check_either(&set, p(2)), Ok(()), "the quoter passes");
    assert_eq!(check_either(&set, p(3)), Ok(()), "so does the watcher");
    let wrong = check_either(&set, p(9)).expect_err("a stranger does not");
    assert!(!wrong.contains("not set"), "a set role is not an unset one");
}

/// An error a stranger reads must not name the principal that would have worked.
#[test]
fn a_refusal_names_the_role_and_never_a_principal() {
    let holder = p(2);
    let err = check(Some(holder), p(9), "quoter").unwrap_err();
    assert!(!err.contains(&holder.to_text()), "leaked the holder: {err}");
    assert!(!err.contains(&p(9).to_text()), "echoed the caller: {err}");
}
