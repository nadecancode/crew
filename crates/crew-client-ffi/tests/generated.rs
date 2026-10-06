#![cfg(feature = "bindgen")]

use std::fs;
use std::path::Path;
use std::process::Command;

#[test]
fn committed_swift_is_exact_unformatted_generator_output() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repository = manifest.join("../..");
    let executable = Path::new(env!("CARGO_BIN_EXE_crew-client-bindgen"));
    let profile_dir = executable.parent().unwrap();
    // cargo test builds the rlib and generator, but need not build the cdylib.
    // Build it explicitly in the same artifact directory even when the parent
    // command used --target-dir instead of CARGO_TARGET_DIR.
    let target_dir = profile_dir.parent().unwrap();
    let profile = profile_dir.file_name().unwrap().to_str().unwrap();
    let profile = if profile == "debug" { "dev" } else { profile };
    let library = profile_dir.join(format!(
        "{}crew_client_ffi{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX,
    ));
    let scratch = repository.join("target/native-validation");
    fs::create_dir_all(&scratch).unwrap();
    let committed = repository.join("apps/apple/CrewClient/Sources/Generated");
    // Local IO is an optional implementation detail. Both feature selections
    // must generate exactly the same public API, including the CrewClient
    // object. This catches an accidental local-only cfg on exported methods.
    for features in ["bindings", "bindings,local"] {
        let build = Command::new(env!("CARGO"))
            .current_dir(manifest)
            .env("CARGO_TARGET_DIR", target_dir)
            .env_remove("CARGO_BUILD_TARGET")
            .args([
                "build",
                "--locked",
                "-p",
                "crew-client-ffi",
                "--no-default-features",
                "--features",
                features,
                "--profile",
                profile,
            ])
            .status()
            .expect("build the actual host binding library for freshness verification");
        assert!(build.success());
        assert!(
            library.is_file(),
            "Cargo must produce the host binding library"
        );
        let output = tempfile::Builder::new()
            .prefix("ffi-freshness-")
            .tempdir_in(&scratch)
            .unwrap();
        let status = Command::new(executable)
            .current_dir(manifest)
            .args(["generate", "--library"])
            .arg(&library)
            .args(["--language", "swift", "--no-format", "--out-dir"])
            .arg(output.path())
            .status()
            .expect("run the pinned in-workspace generator");
        assert!(status.success());

        for (name, folder) in [
            ("CrewBindings.swift", "Swift"),
            ("CrewClientFFI.h", "Headers"),
            ("CrewClientFFI.modulemap", "Headers"),
        ] {
            assert_eq!(
                fs::read(output.path().join(name)).unwrap(),
                fs::read(committed.join(folder).join(name)).unwrap(),
                "stale generated file {name} with features {features}; regenerate with --no-format",
            );
        }
    }
}

#[test]
fn swift_identity_and_private_boundary_are_explicit() {
    let swift =
        include_str!("../../../apps/apple/CrewClient/Sources/Generated/Swift/CrewBindings.swift");
    for declaration in [
        "public struct ClientScope: Equatable, Hashable",
        "public struct EntityKey: Equatable, Hashable",
        "public enum Entity: Equatable, Hashable",
        "public let generation: UInt64?",
        "public let daemonPid: UInt32?",
        "public let daemonStartedAtMs: Int64?",
        "public let attempt: UInt64",
        "public let attachmentId: UInt64?",
        "public let metadataJson: String?",
        "public let dataJson: String?",
        "public let seq: Int64",
        "public let filesChanged: UInt64",
        "case sentUnacknowledged",
        "public let askedBy: String?",
        "public protocol ClientObserver: AnyObject, Sendable",
        "func changed(notice: ClientNotice)",
        "func terminal(chunk: TerminalChunk)",
        "public protocol CrewClientProtocol: AnyObject, Sendable",
        "func connect(profileId: String) throws",
        "func connectionContext() throws  -> ConnectionContext?",
        "func workspaces() throws  -> Workspaces?",
        "public static func local()throws  -> CrewClient",
        "func localProfiles() throws  -> [LocalProfileInfo]",
        "public struct LocalProfileInfo: Equatable, Hashable",
        "func query(query: ViewQuery) throws  -> ViewData",
        "func plan(intent: ActionIntent) throws  -> ActionPlan",
        "func observe(observer: ClientObserver) throws  -> UInt64",
        "func removeObserver(subscriptionId: UInt64) throws",
    ] {
        assert!(
            swift.contains(declaration),
            "missing native contract: {declaration}"
        );
    }
    for private in [
        "ClientInput",
        "ClientUpdate",
        "ClientEffect",
        "Command",
        "CommandResult",
        "HostEvent",
        "HostEffect",
        "Hello",
        "HelloInfo",
        "WebPaired",
        "SendParams",
        "ServerFrame",
        "Push",
        "Snapshot",
        "ProfileSelection",
    ] {
        for kind in ["struct", "enum", "class", "protocol"] {
            for suffix in [":", " "] {
                assert!(
                    !swift.contains(&format!("public {kind} {private}{suffix}")),
                    "private Rust type {private} must not cross the Swift interface"
                );
            }
        }
    }
    assert!(!swift.contains("webProof"));
    assert!(!swift.contains("operatorToken"));
}
