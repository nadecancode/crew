use super::*;
use std::os::unix::fs::symlink;

const STATUS: CapturedStatus = CapturedStatus {
    exit_code: Some(101),
    signal: None,
    timed_out: false,
};

fn fixture() -> (tempfile::TempDir, File, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = File::open(dir.path()).unwrap();
    (dir, root, "a".repeat(64))
}

fn capture() -> Capture {
    Capture {
        status: STATUS,
        stdout: b"exact\0\xff\r\n[redacted $TOKEN]\n".to_vec(),
        stderr: b"warning: keep this\n".to_vec(),
    }
}

#[test]
fn exact_streams_status_and_reference_survive_store_restart() {
    let (_dir, root, scope) = fixture();
    let expected = capture();
    let reference = {
        let store = Store::open(&root, scope.clone()).unwrap();
        let reference = store.put(&expected).unwrap();
        assert_eq!(store.put(&expected).unwrap(), reference);
        reference
    };
    let store = Store::open(&root, scope).unwrap();
    let actual = store.get(&reference).unwrap();
    assert_eq!(actual.stdout, expected.stdout);
    assert_eq!(actual.stderr, expected.stderr);
    assert_eq!(actual.status, expected.status);
    assert_eq!(names(&store.root).unwrap().len(), 2);
}

#[test]
fn public_reference_never_selects_another_credential_scope_or_path() {
    let (_dir, root, scope) = fixture();
    let reference = Store::open(&root, scope).unwrap().put(&capture()).unwrap();
    let other = Store::open(&root, "b".repeat(64)).unwrap();
    assert!(other.get(&reference).is_err());
    for input in [
        "../identity.json",
        "/etc/passwd",
        "sha256:../raw",
        "sha256:",
        "SHA256:abcd",
    ] {
        assert!(other.get(input).is_err());
    }
    let mut identity = paths::Identity {
        socket: "/socket".into(),
        agent_id: "a".into(),
        token: "old".into(),
        orchestrator: false,
    };
    let old = namespace(&identity);
    identity.token = "new".into();
    assert_ne!(namespace(&identity), old);
    identity.token = "old".into();
    identity.agent_id = "b".into();
    assert_ne!(namespace(&identity), old);
}

#[test]
fn changed_truncated_and_oversized_artifacts_fail_closed() {
    let (dir, root, scope) = fixture();
    let store = Store::open(&root, scope).unwrap();
    let reference = store.put(&capture()).unwrap();
    let path = dir
        .path()
        .join("rw/output-v1")
        .join(store.name(&reference).unwrap());
    let original = std::fs::read(&path).unwrap();
    for length in [0, MAGIC.len(), original.len() - 1] {
        std::fs::write(&path, &original[..length]).unwrap();
        assert!(store.get(&reference).is_err());
    }
    let mut altered = original.clone();
    *altered.last_mut().unwrap() ^= 1;
    std::fs::write(&path, altered).unwrap();
    assert!(store.get(&reference).is_err());
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(FILE_LIMIT as u64 + 1)
        .unwrap();
    assert!(store.get(&reference).is_err());
}

#[test]
fn symlinks_hard_links_and_permissive_files_are_not_raw_authority() {
    let (dir, root, scope) = fixture();
    let store = Store::open(&root, scope).unwrap();
    let reference = store.put(&capture()).unwrap();
    let path = dir
        .path()
        .join("rw/output-v1")
        .join(store.name(&reference).unwrap());
    let outside = dir.path().join("sentinel");
    std::fs::write(&outside, b"do not change").unwrap();
    std::fs::remove_file(&path).unwrap();
    symlink(&outside, &path).unwrap();
    assert!(store.get(&reference).is_err());
    assert!(store.put(&capture()).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"do not change");
    std::fs::remove_file(&path).unwrap();
    store.put(&capture()).unwrap();
    std::fs::hard_link(&path, dir.path().join("linked")).unwrap();
    assert!(store.get(&reference).is_err());
    std::fs::remove_file(dir.path().join("linked")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.get(&reference).is_err());
    drop(store);
    let output = dir.path().join("rw/output-v1");
    std::fs::rename(&output, dir.path().join("moved")).unwrap();
    symlink(dir.path().join("moved"), &output).unwrap();
    assert!(Store::open(&root, "a".repeat(64)).is_err());
}

#[test]
fn descriptor_anchor_survives_path_replacement_without_writing_to_the_replacement() {
    let (dir, root, scope) = fixture();
    let store = Store::open(&root, scope).unwrap();
    let output = dir.path().join("rw/output-v1");
    let moved = dir.path().join("original");
    let replacement = dir.path().join("replacement");
    std::fs::create_dir(&replacement).unwrap();
    std::fs::rename(&output, &moved).unwrap();
    symlink(&replacement, &output).unwrap();
    let reference = store.put(&capture()).unwrap();
    assert_eq!(store.get(&reference).unwrap().stdout, capture().stdout);
    assert_eq!(std::fs::read_dir(&replacement).unwrap().count(), 0);
}

#[test]
fn count_byte_and_time_budgets_prune_raw_without_rerunning_anything() {
    let (_dir, root, scope) = fixture();
    let store = Store::open(&root, scope).unwrap();
    let first = store.put(&capture()).unwrap();
    for i in 0..MAX_FILES {
        store
            .put(&Capture {
                stdout: i.to_string().into_bytes(),
                ..capture()
            })
            .unwrap();
    }
    assert!(store.get(&first).is_err());
    assert_eq!(store.prune(SystemTime::now()).unwrap().len(), MAX_FILES);
    for i in 0..10 {
        store
            .put(&Capture {
                status: STATUS,
                stdout: vec![i; MAX_CAPTURE_BYTES],
                stderr: vec![],
            })
            .unwrap();
    }
    let entries = store.prune(SystemTime::now()).unwrap();
    assert!(entries.iter().map(|entry| entry.bytes).sum::<u64>() <= MAX_BYTES);
    assert!(entries.len() <= MAX_FILES);
    let remaining = store
        .prune(SystemTime::now() + RETENTION + Duration::from_secs(1))
        .unwrap();
    assert!(remaining.is_empty());
}

#[test]
fn busy_store_is_bounded_and_interrupted_publication_recovers() {
    let (dir, root, scope) = fixture();
    let store = Store::open(&root, scope.clone()).unwrap();
    assert!(Store::open(&root, scope.clone()).is_err());
    let reference = store.put(&capture()).unwrap();
    let artifact = dir
        .path()
        .join("rw/output-v1")
        .join(store.name(&reference).unwrap());
    let pending = dir
        .path()
        .join("rw/output-v1/.pending-0123456789abcdef0123456789abcdef");
    std::fs::hard_link(&artifact, &pending).unwrap();
    drop(store);
    let store = Store::open(&root, scope).unwrap();
    assert_eq!(store.get(&reference).unwrap().stdout, capture().stdout);
    assert!(!pending.exists());
}

#[test]
fn completed_operation_unlocks_even_if_an_inherited_description_still_exists() {
    let (_dir, root, scope) = fixture();
    let store = Store::open(&root, scope.clone()).unwrap();
    let reference = store.put(&capture()).unwrap();
    // try_clone and fork both retain the same open file description. This
    // exercises its lifetime deterministically without forking a test process.
    let inherited = store._lock.try_clone().unwrap();
    drop(store);
    let next = Store::open(&root, scope).unwrap();
    assert_eq!(next.get(&reference).unwrap().stdout, capture().stdout);
    drop(inherited);
}
