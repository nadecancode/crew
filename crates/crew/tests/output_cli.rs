//! Explicit artifact views through the real CLI and authenticated daemon.
//! No native model CLI, execution wrapper or provider call is involved.
mod common;

use std::path::Path;
use std::process::Output;

use base64::Engine;
use common::*;
use serde_json::Value;

const PASS: &[u8] = include_bytes!("../../crew-core/tests/fixtures/output-view/cargo-success.txt");

fn output(s: &Session, member: &str, args: &[&str]) -> Output {
    s.command()
        .env("CREW_IDENTITY", s.identity_path(member))
        .arg("output")
        .args(args)
        .output()
        .unwrap()
}

fn ok_json(out: Output) -> Value {
    assert!(out.status.success(), "{}\n{}", stdout(&out), stderr(&out));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn view(s: &Session, member: &str, file: &Path, args: &[&str]) -> Output {
    let mut command = vec!["view", file.to_str().unwrap()];
    command.extend_from_slice(args);
    output(s, member, &command)
}

fn setup() -> Session {
    let s = Session::new();
    s.run(&s.team(
        "output",
        vec![agent("a", "capture", &[]), agent("b", "other", &[])],
        vec![],
    ));
    s.wait_for("identities", || {
        s.identity_path("a").exists() && s.identity_path("b").exists()
    });
    s
}

#[test]
fn summary_is_stable_exact_raw_survives_restart_and_other_identity_cannot_recover_it() {
    let s = setup();
    let input = s.root().join("captured.txt");
    std::fs::write(&input, PASS).unwrap();
    let first = view(&s, "a", &input, &["--exit-code", "0"]);
    let first_bytes = first.stdout.clone();
    let report = ok_json(first);
    assert_eq!(report["kind"], "summary");
    assert_eq!(report["counts"]["passed"], 198);
    assert_eq!(report["counts"]["ignored"], 2);
    assert_eq!(report["counts"]["suites"], 5);
    assert_eq!(report["original_status"]["exit_code"], 0);
    assert_eq!(report["raw_retained"], true);
    let reference = report["source"]["reference"].as_str().unwrap();
    assert!(!String::from_utf8_lossy(&first_bytes).contains("/home/"));
    assert!(!String::from_utf8_lossy(&first_bytes).contains(&*s.root().to_string_lossy()));
    let denied = output(&s, "b", &["raw", reference]);
    assert!(!denied.status.success());
    assert!(denied.stdout.is_empty());
    // Another process and another source path do not change appended view bytes.
    let moved = s.root().join("same-output-another-name.txt");
    std::fs::rename(&input, &moved).unwrap();
    assert_eq!(
        view(&s, "a", &moved, &["--exit-code", "0"]).stdout,
        first_bytes
    );
    // Nor does an independently admitted identical capture in another member.
    assert_eq!(
        view(&s, "b", &moved, &["--exit-code", "0"]).stdout,
        first_bytes
    );
    std::fs::remove_file(&moved).unwrap();
    let raw = output(&s, "a", &["raw", reference]);
    assert!(raw.status.success(), "{}", stderr(&raw));
    assert_eq!(raw.stdout, PASS);
    let metadata: Value = serde_json::from_slice(&raw.stderr).unwrap();
    assert_eq!(metadata["exact_bytes"], true);
    assert_eq!(metadata["end"], PASS.len());
    s.stop();
    s.ok(&["start", "-C", s.workspace.to_str().unwrap()]);
    let after_restart = output(&s, "a", &["raw", reference]);
    assert!(after_restart.status.success(), "{}", stderr(&after_restart));
    assert_eq!(after_restart.stdout, PASS);
    // Revocation is checked again even though the artifact and ref still exist.
    s.agent_cmd(&["remove", "a"]);
    let revoked = output(&s, "a", &["raw", reference]);
    assert!(!revoked.status.success());
    assert!(revoked.stdout.is_empty());
}

#[test]
fn warnings_machine_and_binary_stay_raw_with_original_status_and_bounded_exact_chunks() {
    let s = setup();
    let input = s.root().join("stdout.txt");
    let err = s.root().join("stderr.txt");
    std::fs::write(&input, PASS).unwrap();
    let warning = b"warning: preserve [redacted $TOKEN]\n";
    std::fs::write(&err, warning).unwrap();
    let report = ok_json(view(
        &s,
        "a",
        &input,
        &["--exit-code", "0", "--stderr", err.to_str().unwrap()],
    ));
    assert_eq!(report["kind"], "raw");
    assert_eq!(
        report["raw_preview"][1]["data"],
        std::str::from_utf8(warning).unwrap()
    );
    let reference = report["source"]["reference"].as_str().unwrap();
    let raw = output(&s, "a", &["raw", reference, "--stream", "stderr"]);
    assert_eq!(raw.stdout, warning);
    assert_eq!(std::fs::read(&err).unwrap(), warning);
    for status in [
        vec!["--exit-code", "101"],
        vec!["--signal", "9"],
        vec!["--timed-out"],
    ] {
        let report = ok_json(view(&s, "a", &input, &status));
        assert_eq!(report["kind"], "raw");
        match status[0] {
            "--exit-code" => assert_eq!(report["original_status"]["exit_code"], 101),
            "--signal" => assert_eq!(report["original_status"]["signal"], 9),
            _ => assert_eq!(report["original_status"]["timed_out"], true),
        }
    }
    let machine = ok_json(view(
        &s,
        "a",
        &input,
        &["--exit-code", "0", "--input-format", "machine"],
    ));
    assert_eq!(machine["kind"], "raw");
    assert_eq!(machine["reason"], "machine_format");
    let mut binary: Vec<u8> = (0..200_000).map(|i| (i % 256) as u8).collect();
    binary.extend_from_slice(b"\ntouch not-executed\n");
    std::fs::write(&input, &binary).unwrap();
    let report = ok_json(view(&s, "a", &input, &["--exit-code", "0"]));
    assert_eq!(report["raw_preview"][0]["encoding"], "base64");
    assert_eq!(report["raw_preview"][0]["truncated"], true);
    let preview = base64::engine::general_purpose::STANDARD
        .decode(report["raw_preview"][0]["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(preview, binary[..16 * 1024]);
    let reference = report["source"]["reference"].as_str().unwrap();
    let mut recovered = Vec::new();
    while recovered.len() < binary.len() {
        let raw = output(
            &s,
            "a",
            &[
                "raw",
                reference,
                "--offset",
                &recovered.len().to_string(),
                "--length",
                "50000",
            ],
        );
        assert!(raw.status.success(), "{}", stderr(&raw));
        assert!(!raw.stdout.is_empty());
        let metadata: Value = serde_json::from_slice(&raw.stderr).unwrap();
        assert_eq!(metadata["start"], recovered.len());
        recovered.extend_from_slice(&raw.stdout);
        assert_eq!(metadata["end"], recovered.len());
    }
    assert_eq!(recovered, binary);
    assert_eq!(std::fs::read(&input).unwrap(), binary);
    assert!(!s.root().join("not-executed").exists());
    for args in [
        vec!["raw", "../identity.json"],
        vec!["raw", reference, "--offset", "18446744073709551615"],
    ] {
        assert!(!output(&s, "a", &args).status.success());
    }
}

#[test]
fn bad_identity_sources_and_storage_never_fabricate_recoverable_summaries() {
    use std::os::unix::fs::symlink;
    let s = setup();
    let input = s.root().join("input");
    std::fs::write(&input, PASS).unwrap();
    let identity_path = s.identity_path("a");
    let saved = std::fs::read(&identity_path).unwrap();
    let mut identity: crew_core::paths::Identity = serde_json::from_slice(&saved).unwrap();
    identity.token = "not-the-active-token".into();
    std::fs::write(&identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
    assert!(!view(&s, "a", &input, &["--exit-code", "0"])
        .status
        .success());
    assert!(!identity_path
        .parent()
        .unwrap()
        .join("rw/output-v1")
        .exists());
    std::fs::write(&identity_path, saved).unwrap();
    let link = s.root().join("linked-input");
    symlink(&input, &link).unwrap();
    for file in [&link, s.root()] {
        let out = view(&s, "a", file, &["--exit-code", "101"]);
        assert!(!out.status.success());
        let report: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(report["kind"], "unavailable");
        assert_eq!(report["original_status"]["exit_code"], 101);
    }
    let oversized = s.root().join("too-big");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(4 * 1024 * 1024 + 1)
        .unwrap();
    let out = view(&s, "a", &oversized, &["--signal", "15"]);
    assert!(!out.status.success());
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["original_status"]["signal"], 15);
    // A hostile store root never redirects writes; original capture survives.
    let outside = s.root().join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink(
        &outside,
        identity_path.parent().unwrap().join("rw/output-v1"),
    )
    .unwrap();
    let out = view(&s, "a", &input, &["--exit-code", "0"]);
    assert!(!out.status.success());
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["kind"], "raw");
    assert_eq!(report["raw_retained"], false);
    assert_eq!(report["reason"], "recovery_unavailable");
    assert!(report.get("recovery").is_none());
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
    assert_eq!(std::fs::read(&input).unwrap(), PASS);
}

#[cfg(target_os = "linux")]
#[test]
fn helper_uses_existing_member_sandbox_rights_for_view_and_recovery() {
    let binary = Path::new(CREW).canonicalize().unwrap();
    if let Err(reason) = crew::sandbox::probe::probe(&binary, std::env::var_os("PATH").as_ref()) {
        assert_ne!(
            std::env::var("CREW_TEST_SANDBOX").as_deref(),
            Ok("require"),
            "sandbox required: {reason}"
        );
        eprintln!("skipped output sandbox test: {reason}");
        return;
    }
    let s = Session::new();
    let mut member = agent("a", "sandbox output", &[]);
    member.command = vec![
        "sh".into(), "-c".into(),
        r#"set -eu
printf 'running 1 test\ntest fixture ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n' > captured.log
"$1" output view captured.log --exit-code 0 > view.json
ref=$(sed -n 's/.*"reference":"\([^"]*\)".*/\1/p' view.json)
"$1" output raw "$ref" > recovered.log 2> recovered.json
cmp captured.log recovered.log
printf 'done\n' > output-done
"#.into(), "output-fixture".into(), "{crew}".into(),
    ];
    let mut team = s.team("sandbox-output", vec![member], vec![]);
    team.workspace.sandbox.mode = crew_core::config::SandboxMode::Required;
    s.run(&team);
    let snapshot = s.snapshot();
    let workdir = Path::new(&find_agent(&snapshot, "a").workdir);
    s.wait_for("sandboxed raw recovery", || {
        workdir.join("output-done").exists()
    });
    let report: Value =
        serde_json::from_slice(&std::fs::read(workdir.join("view.json")).unwrap()).unwrap();
    assert_eq!(report["kind"], "summary");
    assert_eq!(report["raw_retained"], true);
    assert_eq!(
        std::fs::read(workdir.join("captured.log")).unwrap(),
        std::fs::read(workdir.join("recovered.log")).unwrap()
    );
}
