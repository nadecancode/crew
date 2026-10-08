//! Remote profile input, persistence and copy rules. GTK presentation and
//! the live isolated gateway journey run in `tests/gateway.rs`.

use std::os::unix::fs::PermissionsExt as _;

use crew_client::api::ClientReason;
use crew_protocol::native::{HexBytes, PublicProfile};

use super::copy::{self, Phase, Recovery};
use super::profile::*;

const CERT: &str = include_str!("../../../crew-client-transport/tests/fixtures/gateway/cert.pem");
const PIN: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

fn public() -> PublicProfile {
    PublicProfile {
        endpoint_id: HexBytes([0x11; 16]),
        name: "Fixture host".into(),
        url: "wss://127.0.0.1:8443/native/ws".into(),
        tls_leaf_sha256: HexBytes::from_hex(PIN).unwrap(),
        trust_pem: CERT.into(),
    }
}

fn input(profile: &str, fingerprint: &str) -> ProfileInput {
    ProfileInput {
        label: "Build server".into(),
        profile: profile.into(),
        fingerprint: fingerprint.into(),
    }
}

#[test]
fn fingerprint_normalization_matches_the_shared_rule() {
    let grouped = PIN
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap().to_uppercase())
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(normalize_fingerprint(&grouped).as_deref(), Some(PIN));
    let colons = PIN
        .as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join(":");
    assert_eq!(normalize_fingerprint(&colons).as_deref(), Some(PIN));
    assert_eq!(
        normalize_fingerprint(&format!("{}\n{}\t", &PIN[..32], &PIN[32..])).as_deref(),
        Some(PIN)
    );
    // Not ASCII whitespace, not hex, wrong length: all refused.
    assert_eq!(
        normalize_fingerprint(&format!("{}\u{a0}{}", &PIN[..32], &PIN[32..])),
        None
    );
    assert_eq!(normalize_fingerprint(&PIN.replace('f', "g")), None);
    assert_eq!(normalize_fingerprint(&PIN[..60]), None);
    assert_eq!(normalize_fingerprint(&format!("{PIN}0")), None);
    assert_eq!(fingerprint_characters("0F:1e 2d").chars().count(), 6);
}

#[test]
fn profile_paste_accepts_status_output_or_bare_profile_and_refuses_extras() {
    let bare = serde_json::to_string(&public()).unwrap();
    let status = serde_json::json!({"profile": public(), "endpoint_epoch": "00ff"}).to_string();
    assert_eq!(parse_public_profile(&bare), Some(public()));
    assert_eq!(parse_public_profile(&status), Some(public()));
    let mut extra = serde_json::to_value(public()).unwrap();
    extra["token"] = "secret".into();
    assert_eq!(parse_public_profile(&extra.to_string()), None);
    assert_eq!(
        parse_public_profile("crew web http://127.0.0.1:7000/#code"),
        None
    );
}

#[test]
fn form_errors_follow_reading_order_and_never_trust_the_pasted_pin() {
    let status = serde_json::json!({"profile": public(), "endpoint_epoch": "00ff"}).to_string();
    let mut empty_label = input(&status, PIN);
    empty_label.label = " ".into();
    assert_eq!(
        empty_label.check().unwrap().label,
        "Fixture host",
        "an empty name defaults to the host name"
    );
    let mut long_label = input(&status, PIN);
    long_label.label = "x".repeat(65);
    assert_eq!(long_label.check().unwrap_err().field, Field::Label);
    let profile_error = input("{}", "short").check().unwrap_err();
    assert_eq!(profile_error.field, Field::Profile);
    let mut plaintext = public();
    plaintext.url = "ws://127.0.0.1:8443/native/ws".into();
    assert_eq!(
        input(&serde_json::to_string(&plaintext).unwrap(), PIN)
            .check()
            .unwrap_err()
            .field,
        Field::Profile
    );
    let count = input(&status, &PIN[..60]).check().unwrap_err();
    assert_eq!(count.field, Field::Fingerprint);
    assert_eq!(
        count.message,
        "The fingerprint has 64 hexadecimal characters (0–9, a–f). This one has 60."
    );
    let other = PIN.replace('0', "1");
    let checked = input(&status, &other.to_uppercase()).check().unwrap();
    assert_eq!(checked.entered.to_hex(), other);
    assert_eq!(checked.public, public());
    let profile = RemoteProfile {
        id: "remote-fixture".into(),
        label: checked.label,
        public: checked.public,
        entered_fingerprint: checked.entered,
        device_label: None,
        storage: KeyStorage::SecureStore,
        saved_access: None,
    };
    assert!(
        matches!(
            profile.trusted(),
            Err(crew_client_transport::gateway::GatewayError::Trust)
        ),
        "the connector refuses a fingerprint that differs from the profile pin"
    );
    let matching = RemoteProfile {
        entered_fingerprint: HexBytes::from_hex(PIN).unwrap(),
        ..profile
    };
    assert!(matching.trusted().is_ok());
}

#[test]
fn store_persists_public_fields_only_with_owner_permissions() {
    let root = tempfile::tempdir().unwrap();
    let store = ProfileStore::new(root.path().join("crew-desktop").join("connections.json"));
    assert_eq!(store.load().unwrap(), Vec::new());
    let profile = RemoteProfile {
        id: new_profile_id().unwrap(),
        label: "Build server".into(),
        public: public(),
        entered_fingerprint: HexBytes::from_hex(PIN).unwrap(),
        device_label: Some("Work laptop".into()),
        storage: KeyStorage::Memory,
        saved_access: Some(SavedAccess {
            device_id: "00".repeat(16),
            expires_at_ms: 1_800_000_000_000,
        }),
    };
    store.save(std::slice::from_ref(&profile)).unwrap();
    assert_eq!(store.load().unwrap(), vec![profile]);
    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(store.path()), 0o600);
    assert_eq!(mode(store.path().parent().unwrap()), 0o700);
    let text = std::fs::read_to_string(store.path()).unwrap();
    for forbidden in ["private", "secret", "seed", "signature", "token"] {
        assert!(!text.contains(forbidden), "{forbidden} in saved profile");
    }
    std::fs::write(store.path(), b"{\"version\":2,\"profiles\":[]}").unwrap();
    assert!(
        store.load().is_err(),
        "unknown versions are not silently replaced"
    );
}

#[test]
fn hex_reads_in_lower_case_like_the_host_console() {
    assert_eq!(group_hex("0A1B2C3D", 4), "0a1b 2c3d");
    assert_eq!(groups("E58FC96D", 4), ["e58f", "c96d"]);
}

#[test]
fn device_names_use_the_connector_label_bounds() {
    assert_eq!(valid_device_name("  Work laptop ").unwrap(), "Work laptop");
    assert!(valid_device_name("").is_err());
    assert!(valid_device_name(&"x".repeat(65)).is_err());
    assert!(valid_device_name("line\nbreak").is_err());
}

#[test]
fn failure_copy_uses_safe_codes_and_generic_remote_refusals() {
    let reason = |code: &str| ClientReason {
        details: None,
        code: code.into(),
        message: "raw detail must not be shown".into(),
    };
    let secure = KeyStorage::SecureStore;
    let trust = copy::problem(&reason("tls_trust"), Phase::Connect, secure);
    assert!(trust
        .message
        .starts_with("Host identity could not be verified."));
    assert_eq!(trust.actions, vec![Recovery::ReviewProfile]);
    let unreachable = copy::problem(&reason("unreachable"), Phase::Connect, secure);
    assert_eq!(
        unreachable.actions,
        vec![Recovery::TryAgain, Recovery::EditProfile]
    );
    // Before acceptance a refusal is generic; after a shown code it expired.
    let refused = copy::problem(&reason("authentication"), Phase::Request, secure);
    assert!(refused
        .message
        .starts_with("Access request was not accepted."));
    let expired = copy::problem(&reason("timeout"), Phase::Approval, secure);
    assert!(expired
        .message
        .starts_with("This request is no longer available."));
    let denied = copy::problem(&reason("authentication"), Phase::Connect, secure);
    assert_eq!(
        denied.message,
        "Device access is unavailable. Check with the host operator."
    );
    let ended = copy::problem(&reason("key_retired"), Phase::Connect, KeyStorage::Memory);
    assert!(ended.message.contains("ended when Crew closed"));
    for code in [
        "tls_trust",
        "key_locked",
        "unreachable",
        "authentication",
        "revoked",
    ] {
        for phase in [Phase::Connect, Phase::Request, Phase::Approval, Phase::Live] {
            assert!(!copy::problem(&reason(code), phase, secure)
                .message
                .contains("raw detail"));
        }
    }
    let instructions = copy::revoke_instructions(Some("abcd"));
    assert!(instructions.contains("crew native revoke abcd"));
    assert!(instructions.contains("cannot change or revoke access"));
}
