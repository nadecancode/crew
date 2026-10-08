use crew_core::output_view::{self, CapturedStatus, InputFormat, RawReason, View};

const SUCCESS: &str = include_str!("fixtures/output-view/cargo-success.txt");
const FAILURE: &str = include_str!("fixtures/output-view/cargo-failure.txt");
const ZERO: &str = include_str!("fixtures/output-view/cargo-zero.txt");
const UNKNOWN: &str = include_str!("fixtures/output-view/cargo-unknown.txt");
const MACHINE: &str = include_str!("fixtures/output-view/cargo-json.txt");
const STATUS: CapturedStatus = CapturedStatus {
    exit_code: Some(0),
    signal: None,
    timed_out: false,
};

fn view(stdout: &str, stderr: &str) -> output_view::OutputView {
    output_view::view(
        stdout.as_bytes(),
        stderr.as_bytes(),
        STATUS,
        InputFormat::CargoTest,
    )
}

fn raw(stdout: &str, stderr: &str) -> output_view::OutputView {
    let got = view(stdout, stderr);
    assert!(matches!(got.view, View::Raw { .. }), "{got:?}");
    assert_eq!(got.original_status, STATUS);
    assert_eq!(got.source.stdout.end, stdout.len() as u64);
    assert_eq!(got.source.stderr.end, stderr.len() as u64);
    got
}

#[test]
fn captured_corpus_counts_all_five_suites_including_empty_doc_tests() {
    let got = view(SUCCESS, "");
    let View::Summary {
        counts, omitted, ..
    } = got.view
    else {
        panic!("real passing corpus was not recognized: {got:?}");
    };
    assert_eq!(counts.suites, 5);
    assert_eq!(counts.passed, 198);
    assert_eq!(counts.failed, 0);
    assert_eq!(counts.ignored, 2);
    assert_eq!(counts.filtered_out, 0);
    assert!(omitted.contains("ignored reasons"));
    assert!(!serde_json::to_string(&counts).unwrap().contains("/home/"));
}

#[test]
fn warnings_failures_zero_execution_unknown_and_machine_corpus_stay_raw() {
    // Exactly cargo-warning.txt from corpus.json.gz, without another copy of
    // the 15 KiB passing log in the source tree.
    raw(
        &format!(
            "warning: fixture unexpected cfg condition at crates/crew/src/lib.rs:1:1\n{SUCCESS}"
        ),
        "",
    );
    for text in [FAILURE, ZERO, UNKNOWN, MACHINE] {
        raw(text, "");
    }
    for warning in [
        "warning: unused variable\n",
        "error: failure\n",
        "CUSTOM diagnostic\n",
    ] {
        raw(SUCCESS, warning);
        raw(&format!("{SUCCESS}{warning}"), "");
    }
}

#[test]
fn original_signal_exit_and_timeout_are_data_and_never_become_success() {
    for status in [
        CapturedStatus {
            exit_code: Some(101),
            ..STATUS
        },
        CapturedStatus {
            exit_code: None,
            signal: Some(9),
            timed_out: false,
        },
        CapturedStatus {
            timed_out: true,
            ..STATUS
        },
        CapturedStatus {
            exit_code: None,
            signal: None,
            timed_out: true,
        },
        CapturedStatus {
            exit_code: None,
            signal: None,
            timed_out: false,
        },
    ] {
        let got = output_view::view(SUCCESS.as_bytes(), b"", status, InputFormat::CargoTest);
        assert_eq!(got.original_status, status);
        assert_eq!(
            got.view,
            View::Raw {
                reason: RawReason::OriginalStatus
            }
        );
        assert_ne!(got.source.reference, view(SUCCESS, "").source.reference);
    }
}

#[test]
fn incomplete_duplicate_inconsistent_and_interleaved_suites_stay_raw() {
    raw(SUCCESS.trim_end(), "");
    raw(&SUCCESS.replace("135 passed", "136 passed"), "");
    raw(
        &SUCCESS.replace("running 137 tests", "running 136 tests"),
        "",
    );
    raw(
        &SUCCESS.replace("running 137 tests", "running 18446744073709551616 tests"),
        "",
    );
    let (before, after) = SUCCESS.split_once("test result:").unwrap();
    raw(before, &format!("test result:{after}"));
    let one = "running 1 test\ntest a ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.00s\n";
    assert!(matches!(view(one, "").view, View::Summary { counts, .. } if counts.filtered_out == 4));
    raw(
        &one.replace("test a ... ok\n", "test a ... ok\ntest a ... ok\n"),
        "",
    );
    raw(
        &one.replace("test a ... ok\n", "CUSTOM\ntest a ... ok\n"),
        "",
    );
    raw(&one.replace("test a ... ok\n", ""), "");
    raw(&one.replace("test a ... ok", "test a ... FAILED"), "");
    raw(&one.replace("0.00s", "NaNs"), "");
    raw(&one.replace("0 measured", "1 measured"), "");
}

#[test]
fn raw_and_machine_opt_out_even_when_the_text_looks_like_cargo() {
    for (format, reason) in [
        (InputFormat::Raw, RawReason::Requested),
        (InputFormat::Machine, RawReason::MachineFormat),
    ] {
        assert_eq!(
            output_view::view(SUCCESS.as_bytes(), b"", STATUS, format).view,
            View::Raw { reason }
        );
    }
    for bytes in [b"\xff".as_slice(), b"\0", b"\x1b[31mwarning\x1b[0m\n"] {
        let got = output_view::view(SUCCESS.as_bytes(), bytes, STATUS, InputFormat::CargoTest);
        assert_eq!(
            got.view,
            View::Raw {
                reason: RawReason::NonTextOrControl
            }
        );
    }
}

#[test]
fn a_complete_stderr_suite_is_independent_but_build_metadata_is_not_a_suite() {
    let split = SUCCESS.find("running 137 tests").unwrap();
    let got = view(&SUCCESS[split..], &SUCCESS[..split]);
    assert!(
        matches!(got.view, View::Summary { counts, .. } if counts.passed == 198 && counts.suites == 5)
    );
    assert!(matches!(view("", SUCCESS).view, View::Summary { counts, .. } if counts.passed == 198));
    raw(&SUCCESS[..split], "");
    let crlf = SUCCESS.replace('\n', "\r\n");
    assert!(matches!(view(&crlf, "").view, View::Summary { .. }));
    assert_ne!(
        view(&crlf, "").source.reference,
        view(SUCCESS, "").source.reference
    );
}

#[test]
fn content_references_bind_exact_streams_and_status_without_time_or_path() {
    let first = view(SUCCESS, "");
    assert_eq!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&view(SUCCESS, "")).unwrap()
    );
    assert_ne!(first.source.reference, view("", SUCCESS).source.reference);
    assert_ne!(
        output_view::reference(b"ab", b"c", STATUS),
        output_view::reference(b"a", b"bc", STATUS)
    );
    assert_eq!(first.format_version, 1);
    assert_eq!(first.source.stdout.start, 0);
    assert!(!serde_json::to_string(&first)
        .unwrap()
        .contains("/home/nade"));
    let all_ignored = "running 1 test\ntest a ... ignored, needs network\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
    assert_eq!(
        raw(all_ignored, "").view,
        View::Raw {
            reason: RawReason::NoTestsExecuted
        }
    );
}
