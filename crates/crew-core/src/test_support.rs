//! Operator tests must not inherit an agent session's ambient authority.
//! Re-exec only the affected test; never mutate this multithreaded process's
//! environment or bypass Client::operator in production.

pub fn isolate_operator_test(name: &str) -> bool {
    const CHILD: &str = "CREW_OPERATOR_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return false;
    }
    if std::env::var_os("CREW_IDENTITY").is_none() && std::env::var_os("CREW_SOCKET").is_none() {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, "1")
        .env_remove("CREW_IDENTITY")
        .env_remove("CREW_SOCKET")
        .output()
        .expect("running the isolated operator test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let counts: Vec<usize> = stdout
        .lines()
        .filter_map(|line| {
            let count = line.strip_prefix("running ")?;
            count
                .strip_suffix(" test")
                .or_else(|| count.strip_suffix(" tests"))?
                .parse()
                .ok()
        })
        .collect();
    assert!(
        output.status.success() && counts == [1],
        "isolated {name} did not run and pass exactly once:\n{stdout}\n{stderr}"
    );
    true
}
