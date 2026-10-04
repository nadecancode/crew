use super::*;

fn serving() -> Registry {
    Registry {
        serving: Some(Serving {
            label: random_label(),
            port: 23417,
        }),
        ..Registry::default()
    }
}

#[test]
fn a_code_works_once_until_it_expires() {
    let mut r = serving();
    let code = r.code(0);
    let paired = r.pair(&code, 1_000).unwrap();
    assert_eq!(paired.verifier.len(), 64);
    assert_eq!(r.pair(&code, 1_000).unwrap_err().code, "unauthorized");
    let late = r.code(0);
    assert_eq!(r.pair(&late, CODE_TTL_MS).unwrap_err().code, "unauthorized");
    let fresh = r.code(0);
    assert!(r.pair(&fresh, CODE_TTL_MS - 1).is_ok());
    assert_eq!(r.pair("", 0).unwrap_err().code, "unauthorized");
}

#[test]
fn the_ninth_code_drops_the_oldest() {
    let mut r = serving();
    let codes: Vec<String> = (0..=MAX_CODES).map(|i| r.code(i as i64)).collect();
    assert_eq!(r.pair(&codes[0], 100).unwrap_err().code, "unauthorized");
    for code in &codes[1..] {
        assert!(r.pair(code, 100).is_ok());
    }
}

#[test]
fn the_seventeenth_session_drops_the_idle_one_with_the_oldest_hello() {
    let mut r = serving();
    let mut tokens = Vec::new();
    for i in 0..MAX_SESSIONS {
        let code = r.code(i as i64);
        tokens.push(r.pair(&code, i as i64).unwrap().token);
    }
    // Every session but the first two has a connection open; of the two
    // idle ones, the second said hello last.
    for token in &tokens[2..] {
        r.hello(token, 50).unwrap();
    }
    let (second, _) = r.hello(&tokens[1], 60).unwrap();
    r.authenticated -= 1;
    r.sessions.iter_mut().find(|s| s.id == second).unwrap().open = 0;
    let code = r.code(70);
    r.pair(&code, 70).unwrap();
    assert_eq!(r.sessions(), MAX_SESSIONS);
    assert_eq!(r.hello(&tokens[0], 80).unwrap_err().code, "unauthorized");
    // All sixteen have connections: refused, and the code stays good.
    for s in &mut r.sessions {
        s.open = 1;
    }
    let code = r.code(90);
    assert_eq!(r.pair(&code, 90).unwrap_err().code, "unavailable");
    r.sessions[0].open = 0;
    assert!(r.pair(&code, 91).is_ok());
}

#[test]
fn hello_answers_the_secret_whose_hash_the_verifier_is() {
    let mut r = serving();
    let code = r.code(0);
    let paired = r.pair(&code, 0).unwrap();
    let (_, secret) = r.hello(&paired.token, 1).unwrap();
    assert_eq!(verifier(&secret), paired.verifier);
    assert_ne!(secret, paired.token);
    assert_eq!(r.hello("nope", 1).unwrap_err().code, "unauthorized");
    // Stopping: nothing works, and nothing survives.
    r.stopping = true;
    assert_eq!(r.hello(&paired.token, 2).unwrap_err().code, "unavailable");
    r.revoke();
    r.stopping = false;
    assert_eq!(r.hello(&paired.token, 3).unwrap_err().code, "unauthorized");
}

#[test]
fn authenticated_connections_are_bounded() {
    let mut r = serving();
    let code = r.code(0);
    let token = r.pair(&code, 0).unwrap().token;
    for _ in 0..MAX_AUTHENTICATED {
        r.hello(&token, 1).unwrap();
    }
    assert_eq!(r.hello(&token, 1).unwrap_err().code, "unavailable");
}

#[test]
fn refused_logins_are_reported_at_most_once_a_minute() {
    let mut r = serving();
    assert_eq!(r.failed(0), Some(1));
    for t in 1..7 {
        assert_eq!(r.failed(t * 1000), None);
    }
    assert_eq!(r.failed(AUTH_FAILED_EVERY_MS), Some(7));
}

#[test]
fn two_starts_never_give_the_same_label() {
    let labels: std::collections::HashSet<String> = (0..1000).map(|_| random_label()).collect();
    assert_eq!(labels.len(), 1000);
    assert!(labels
        .iter()
        .all(|l| l.len() == 32 && l.bytes().all(|b| b.is_ascii_hexdigit())));
}
