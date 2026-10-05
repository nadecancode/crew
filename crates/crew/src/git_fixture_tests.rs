//! Regressions run real fixtures against an injected disposable repository.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::git_test_support::{clear_repository_environment, git};

fn checked_git(dir: &Path, args: &[&str]) {
    let output = git(dir).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, at: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(dir, dir, &mut files);
    files
}

#[test]
fn inherited_git_overrides_cannot_mutate_a_foreign_fixture_repository() {
    for fixture in [
        "daemon::sandbox::tests::a_new_worktree_is_filled_through_the_helper_and_made_without_it",
        "endpoints::tests::files_others_can_write_and_files_from_the_repository_are_rejected",
    ] {
        let root = tempfile::tempdir().unwrap();
        let foreign = root.path().join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        checked_git(&foreign, &["init", "-q", "-b", "main"]);
        std::fs::write(foreign.join("sentinel.txt"), "foreign repository\n").unwrap();
        checked_git(&foreign, &["add", "sentinel.txt"]);
        checked_git(&foreign, &["commit", "-q", "-m", "sentinel"]);
        checked_git(&foreign, &["config", "fixture.sentinel", "unchanged"]);
        let git_dir = foreign.join(".git");
        let before = snapshot(&git_dir);
        assert!(before.contains_key(Path::new("HEAD")));
        assert!(before.contains_key(Path::new("index")));
        assert!(before.contains_key(Path::new("config")));
        let path = |p: &Path| p.to_string_lossy().into_owned();
        let cases = [
            vec![("GIT_DIR", path(&git_dir))],
            vec![("GIT_INDEX_FILE", path(&git_dir.join("index")))],
            vec![
                ("GIT_DIR", path(&git_dir)),
                ("GIT_WORK_TREE", path(&foreign)),
                ("GIT_COMMON_DIR", path(&git_dir)),
                ("GIT_INDEX_FILE", path(&git_dir.join("index"))),
                ("GIT_OBJECT_DIRECTORY", path(&git_dir.join("objects"))),
                (
                    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                    path(&git_dir.join("objects")),
                ),
                ("GIT_NAMESPACE", "foreign".to_string()),
                ("GIT_CEILING_DIRECTORIES", path(root.path())),
                ("GIT_CONFIG_COUNT", "1".to_string()),
                ("GIT_CONFIG_KEY_0", "core.worktree".to_string()),
                ("GIT_CONFIG_VALUE_0", path(&foreign)),
            ],
        ];
        for overrides in cases {
            let mut child = Command::new(std::env::current_exe().unwrap());
            clear_repository_environment(&mut child);
            let output = child
                .args(["--exact", fixture, "--nocapture"])
                .envs(overrides.iter().map(|(k, v)| (*k, v)))
                .output()
                .unwrap();
            let after = snapshot(&git_dir);
            let changed = before
                .keys()
                .chain(after.keys())
                .filter(|p| before.get(*p) != after.get(*p))
                .collect::<std::collections::BTreeSet<_>>();
            assert!(
                changed.is_empty(),
                "{fixture}: foreign files changed: {changed:?}"
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success()
                    && stdout.lines().any(|line| line == "running 1 test")
                    && stdout.contains("1 passed; 0 failed"),
                "{fixture} did not run and pass: {output:?}"
            );
        }
    }
}
