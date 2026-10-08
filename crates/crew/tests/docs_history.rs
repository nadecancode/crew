//! Real confined Git plumbing. Require mode refuses to turn a missing sandbox
//! into passing evidence. No provider/runtime processes or operator repository.
#![cfg(target_os = "linux")]
use crew::daemon::features::{Caller, ConnCtx, FeatureService, RunCtx};
use crew::docs::{
    config::{Policy, Relative},
    history::{Reader, Tools, DEADLINE},
    index::Registration,
    Service,
};
use crew_core::rpc::{
    docs::*,
    features::{Id, PageRequest},
    Call,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Instant,
};
const CREW: &str = env!("CARGO_BIN_EXE_crew");
fn tools() -> Option<Tools> {
    let crew = PathBuf::from(CREW).canonicalize().unwrap();
    let probe = match crew::sandbox::probe::probe(&crew, std::env::var_os("PATH").as_ref()) {
        Ok(p) => p,
        Err(e) => {
            assert_ne!(
                std::env::var("CREW_TEST_SANDBOX").as_deref(),
                Ok("require"),
                "sandbox required: {e}"
            );
            eprintln!("skipped Docs history sandbox: {e}");
            return None;
        }
    };
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join("git"))
        .find(|p| p.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    Some(Tools {
        git,
        crew,
        bwrap: probe.bwrap,
        landlock_abi: probe.landlock_abi,
    })
}
fn git(tools: &Tools, dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new(&tools.git);
    for (name, _) in std::env::vars_os() {
        let s = name.to_string_lossy();
        if s.starts_with("GIT_") || s.starts_with("CREW_") {
            cmd.env_remove(name);
        }
    }
    let out = cmd
        .args(["-C"])
        .arg(dir)
        .args([
            "-c",
            "user.name=Docs Fixture",
            "-c",
            "user.email=docs@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fixture git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn repository(tools: &Tools) -> (tempfile::TempDir, Id, Id) {
    let dir = tempfile::tempdir().unwrap();
    git(tools, dir.path(), &["init", "-q"]);
    fs::create_dir(dir.path().join("notes")).unwrap();
    fs::write(
        dir.path().join("notes/a.md"),
        "# First\n![pixel](pixel.png)\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("notes/pixel.png"),
        include_bytes!("../src/docs/fixtures/pixel.png"),
    )
    .unwrap();
    git(tools, dir.path(), &["add", "."]);
    git(tools, dir.path(), &["commit", "-qm", "first"]);
    let first = Id::new(git(tools, dir.path(), &["rev-parse", "HEAD"])).unwrap();
    fs::write(
        dir.path().join("notes/a.md"),
        "# Second\n![pixel](pixel.png)\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("notes/pixel.png"),
        include_bytes!("../src/docs/fixtures/pixel.jpg"),
    )
    .unwrap();
    git(tools, dir.path(), &["commit", "-qam", "second"]);
    let second = Id::new(git(tools, dir.path(), &["rev-parse", "HEAD"])).unwrap();
    (dir, first, second)
}

#[test]
fn k14_k15_git_and_published_jj_use_fixed_config_raw_blobs_and_no_alternates() {
    let Some(tools) = tools() else {
        return;
    };
    let (dir, first, second) = repository(&tools);
    let git_dir = dir.path().join(".git");
    let marker = dir.path().join("helper-ran");
    let mut config = fs::read_to_string(git_dir.join("config")).unwrap();
    config.push_str(&format!("\n[core]\n  hooksPath = /unmounted/hooks\n  fsmonitor = touch {}\n[diff]\n  external = touch {}\n[filter \"evil\"]\n  smudge = touch {}\n[include]\n  path = /unmounted/private-config\n",marker.display(),marker.display(),marker.display()));
    fs::write(git_dir.join("config"), config).unwrap();
    let before = fs::read(git_dir.join("index")).unwrap();
    for store in [
        crew::vcs::Store::Git {
            git_dir: git_dir.clone(),
        },
        crew::vcs::Store::Jj {
            git_dir: git_dir.clone(),
            repo_dir: dir.path().join("unused-jj-admin"),
        },
    ] {
        let reader = Reader::from_store(&store, tools.clone()).unwrap();
        let path = Relative::new("notes/a.md").unwrap();
        let records = reader
            .records(&second, &path, 0, 50, Instant::now() + DEADLINE)
            .unwrap();
        assert_eq!(records.rows.len(), 2);
        assert_eq!(records.rows[0].commit, second);
        assert_eq!(records.rows[1].commit, first);
        assert_eq!(
            reader
                .blob(&first, &path, 256 * 1024, Instant::now() + DEADLINE)
                .unwrap(),
            b"# First\n![pixel](pixel.png)\n"
        );
        assert!(reader
            .ancestor(&first, &second, Instant::now() + DEADLINE)
            .unwrap());
        assert!(!reader
            .ancestor(&second, &first, Instant::now() + DEADLINE)
            .unwrap());
        // Replacing source configuration after construction is harmless: Git never
        // mounts or reopens it. This is the hostile-config race's negative control.
        fs::write(git_dir.join("config"), b"not even a valid Git config\0").unwrap();
        assert!(reader
            .blob(&first, &path, 256 * 1024, Instant::now() + DEADLINE)
            .is_ok());
    }
    assert!(!marker.exists());
    assert_eq!(before, fs::read(git_dir.join("index")).unwrap());
    fs::write(
        git_dir.join("objects/info/alternates"),
        "/outside/object-store\n",
    )
    .unwrap();
    assert!(matches!(
        Reader::from_store(
            &crew::vcs::Store::Git {
                git_dir: git_dir.clone()
            },
            tools.clone()
        ),
        Err(DocFailureCode::HistoryUnavailable)
    ));
    fs::remove_file(git_dir.join("objects/info/alternates")).unwrap();
    std::os::unix::fs::symlink(
        dir.path().join("outside"),
        git_dir.join("objects/info/alternates"),
    )
    .unwrap();
    assert!(Reader::from_store(&crew::vcs::Store::Git { git_dir }, tools).is_err());
}

#[tokio::test]
async fn k15_k16_recorded_cursor_and_assets_remain_at_the_captured_commit() {
    let Some(tools) = tools() else {
        return;
    };
    let (dir, first, second) = repository(&tools);
    let reader = Arc::new(
        Reader::from_store(
            &crew::vcs::Store::Git {
                git_dir: dir.path().join(".git"),
            },
            tools.clone(),
        )
        .unwrap(),
    );
    let run = RunCtx {
        workspace_run_id: Id::new("docs-run").unwrap(),
    };
    let ctx = ConnCtx::new(Id::new("docs-conn").unwrap(), run.clone(), Caller::Operator);
    let service = Service::new(run, Policy::default());
    let mut registration = Registration {
        key: Id::new("workspace").unwrap(),
        root: dir.path().into(),
        kind: DocSourceKind::Workspace,
        member_ids: vec![],
        display_name: "Workspace (working copy)".into(),
        branch: None,
        head_commit: Some(second.clone()),
        history: Some(reader),
    };
    service.replace_sources(vec![registration.clone()]).unwrap();
    let page = loop {
        let p: DocIndexPage = serde_json::from_value(
            service
                .handle(&ctx, Call::DocsIndex(PageRequest::default()))
                .await
                .unwrap(),
        )
        .unwrap();
        if p.phase == DocPhase::Ready {
            break p;
        }
    };
    let doc = &page.entries[0];
    let request = DocsHistory {
        doc_id: doc.id.clone(),
        source_ids: None,
        page: PageRequest {
            limit: Some(1),
            cursor: None,
        },
    };
    let result: DocHistoryResult = serde_json::from_value(
        service
            .handle(&ctx, Call::DocsHistory(request.clone()))
            .await
            .unwrap(),
    )
    .unwrap();
    let DocHistoryResult::Available(page) = result else {
        panic!("history failed: {result:?}");
    };
    assert_eq!(page.recorded_versions[0].version.commit_id, Some(second));
    let newer_version = page.recorded_versions[0].version.id.clone();
    fs::write(dir.path().join("notes/a.md"), "# Third\n").unwrap();
    git(&tools, dir.path(), &["commit", "-qam", "third"]);
    registration.head_commit =
        Some(Id::new(git(&tools, dir.path(), &["rev-parse", "HEAD"])).unwrap());
    service.replace_sources(vec![registration]).unwrap();
    let result: DocHistoryResult = serde_json::from_value(
        service
            .handle(
                &ctx,
                Call::DocsHistory(DocsHistory {
                    page: PageRequest {
                        limit: Some(1),
                        cursor: page.next_cursor.clone(),
                    },
                    ..request
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let DocHistoryResult::Available(page) = result else {
        panic!("next history failed");
    };
    let version = &page.recorded_versions[0].version;
    assert_eq!(version.commit_id, Some(first));
    assert_eq!(page.recorded_versions[0].sources.len(), 1);
    let difference: DocDiffResult = serde_json::from_value(
        service
            .handle(
                &ctx,
                Call::DocsDiff(DocsDiff {
                    left: newer_version,
                    right: version.id.clone(),
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        matches!(difference,DocDiffResult::Available(diff) if diff.added_lines==1 && diff.removed_lines==1)
    );
    let result: DocReadResult = serde_json::from_value(
        service
            .handle(
                &ctx,
                Call::DocsRead(DocsRead {
                    document: version.document.clone(),
                    version: DocVersionSelector::Id(version.id.clone()),
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let DocReadResult::Available(snapshot) = result else {
        panic!("recorded read failed: {result:?}");
    };
    assert!(snapshot.markdown.starts_with("# First"));
    assert_eq!(snapshot.images[0].media_type, Some(RasterType::Png));
    let result: DocAssetResult = serde_json::from_value(
        service
            .handle(
                &ctx,
                Call::DocsAsset(DocsAsset {
                    snapshot_id: snapshot.snapshot_id.clone(),
                    asset_id: snapshot.images[0].asset_id.clone().unwrap(),
                    offset: 0,
                    max_bytes: 192 * 1024,
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let DocAssetResult::Available(chunk) = result else {
        panic!("recorded asset failed");
    };
    use base64::Engine;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(chunk.data_base64.as_bytes())
            .unwrap(),
        include_bytes!("../src/docs/fixtures/pixel.png")
    );
}

#[test]
fn k04_k15_recorded_symlinks_gitlinks_and_object_store_escape_are_refused() {
    let Some(tools) = tools() else {
        return;
    };
    let (dir, first, _) = repository(&tools);
    let git_dir = dir.path().join(".git");
    let store = crew::vcs::Store::Git {
        git_dir: git_dir.clone(),
    };
    let path = Relative::new("notes/a.md").unwrap();
    // A normal blob is a positive control before every namespace negative.
    let reader = Reader::from_store(&store, tools.clone()).unwrap();
    let expected = reader
        .blob(&first, &path, 256 * 1024, Instant::now() + DEADLINE)
        .unwrap();
    fs::remove_file(dir.path().join("notes/a.md")).unwrap();
    std::os::unix::fs::symlink("/outside/private.md", dir.path().join("notes/a.md")).unwrap();
    git(&tools, dir.path(), &["add", "notes/a.md"]);
    git(&tools, dir.path(), &["commit", "-qm", "symlink"]);
    let link = Id::new(git(&tools, dir.path(), &["rev-parse", "HEAD"])).unwrap();
    assert!(matches!(
        reader.blob(&link, &path, 256 * 1024, Instant::now() + DEADLINE),
        Err(DocFailureCode::ForbiddenPath)
    ));
    git(
        &tools,
        dir.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},notes/submodule", first.as_str()),
        ],
    );
    git(&tools, dir.path(), &["commit", "-qm", "gitlink"]);
    let submodule = Id::new(git(&tools, dir.path(), &["rev-parse", "HEAD"])).unwrap();
    assert!(reader
        .blob(
            &submodule,
            &Relative::new("notes/submodule").unwrap(),
            256 * 1024,
            Instant::now() + DEADLINE
        )
        .is_err());
    let blob = git(
        &tools,
        dir.path(),
        &["rev-parse", &format!("{}:notes/a.md", first.as_str())],
    );
    let object = git_dir.join("objects").join(&blob[..2]).join(&blob[2..]);
    let outside = dir.path().join("outside-object");
    fs::rename(&object, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &object).unwrap();
    assert!(reader
        .blob(&first, &path, 256 * 1024, Instant::now() + DEADLINE)
        .is_err());
    fs::remove_file(&object).unwrap();
    fs::rename(&outside, &object).unwrap();
    assert_eq!(
        reader
            .blob(&first, &path, 256 * 1024, Instant::now() + DEADLINE)
            .unwrap(),
        expected
    );
    // A directory swapped after Reader construction cannot become its object store.
    fs::rename(git_dir.join("objects"), git_dir.join("old-objects")).unwrap();
    fs::create_dir(git_dir.join("objects")).unwrap();
    assert!(matches!(
        reader.blob(&first, &path, 256 * 1024, Instant::now() + DEADLINE),
        Err(DocFailureCode::HistoryUnavailable)
    ));
}

#[tokio::test]
async fn k13_k15_full_history_pages_batch_three_heads_over_320_commits() {
    use std::io::Write;
    use std::process::Stdio;
    let Some(tools) = tools() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    git(&tools, dir.path(), &["init", "-q", "-b", "main"]);
    let mut input = String::new();
    for n in 1..=320 {
        let subject = format!("note {n}");
        let body = format!("# Note {n}\n");
        input.push_str(&format!("commit refs/heads/main\ncommitter Docs Fixture <docs@example.invalid> {} +0000\ndata {}\n{}\nM 100644 inline notes/a.md\ndata {}\n{}\n",1700000000+n,subject.len(),subject,body.len(),body));
    }
    let mut command = Command::new(&tools.git);
    for (name, _) in std::env::vars_os() {
        let s = name.to_string_lossy();
        if s.starts_with("GIT_") || s.starts_with("CREW_") {
            command.env_remove(name);
        }
    }
    let mut child = command
        .arg("-C")
        .arg(dir.path())
        .args(["fast-import", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::create_dir(dir.path().join("notes")).unwrap();
    fs::write(dir.path().join("notes/a.md"), "# Note 320\n").unwrap();
    let reader = Arc::new(
        Reader::from_store(
            &crew::vcs::Store::Git {
                git_dir: dir.path().join(".git"),
            },
            tools.clone(),
        )
        .unwrap(),
    );
    let run = RunCtx {
        workspace_run_id: Id::new("history-many").unwrap(),
    };
    let ctx = ConnCtx::new(
        Id::new("history-many-conn").unwrap(),
        run.clone(),
        Caller::Operator,
    );
    let service = Service::new(run, Policy::default());
    let mut registrations = Vec::new();
    for (i, behind) in [0, 30, 50].into_iter().enumerate() {
        let root = if i == 0 {
            dir.path().to_owned()
        } else {
            let root = dir.path().join(format!(".crew/worktrees/member{i}"));
            fs::create_dir_all(root.join("notes")).unwrap();
            fs::write(
                root.join("notes/a.md"),
                format!("# Note {}\n", 320 - behind),
            )
            .unwrap();
            root
        };
        let commit = Id::new(git(
            &tools,
            dir.path(),
            &["rev-parse", &format!("main~{behind}")],
        ))
        .unwrap();
        registrations.push(Registration {
            key: Id::new(format!("source{i}")).unwrap(),
            root,
            kind: if i == 0 {
                DocSourceKind::Workspace
            } else {
                DocSourceKind::Member
            },
            member_ids: vec![],
            display_name: format!("source{i}"),
            branch: None,
            head_commit: Some(commit),
            history: Some(reader.clone()),
        });
    }
    service.replace_sources(registrations).unwrap();
    let page = loop {
        let p: DocIndexPage = serde_json::from_value(
            service
                .handle(&ctx, Call::DocsIndex(PageRequest::default()))
                .await
                .unwrap(),
        )
        .unwrap();
        if p.phase == DocPhase::Ready {
            break p;
        }
    };
    let doc_id = page.entries[0].id.clone();
    let mut cursor = None;
    let mut seen = std::collections::BTreeSet::new();
    for page_number in 0..2 {
        let start = Instant::now();
        let result: DocHistoryResult = serde_json::from_value(
            service
                .handle(
                    &ctx,
                    Call::DocsHistory(DocsHistory {
                        doc_id: doc_id.clone(),
                        source_ids: None,
                        page: PageRequest {
                            limit: Some(50),
                            cursor,
                        },
                    }),
                )
                .await
                .unwrap(),
        )
        .unwrap();
        let DocHistoryResult::Available(page) = result else {
            panic!("full page failed: {result:?}");
        };
        eprintln!(
            "320-commit, three-head page {page_number}: {} rows, {} ms",
            page.recorded_versions.len(),
            start.elapsed().as_millis()
        );
        assert_eq!(page.recorded_versions.len(), 50);
        assert!(start.elapsed() < DEADLINE);
        assert_eq!(page.captured_heads.len(), 3);
        for (i, row) in page.recorded_versions.iter().enumerate() {
            assert!(seen.insert(row.version.commit_id.clone().unwrap()));
            let number = 320 - page_number * 50 - i;
            assert_eq!(
                row.version.subject.as_ref().map(|s| s.to_string()),
                Some(format!("note {number}"))
            );
            assert_eq!(
                row.sources.len(),
                1 + usize::from(number <= 290) + usize::from(number <= 270)
            );
        }
        cursor = page.next_cursor;
    }
}

#[test]
fn k15_missing_object_info_is_an_explicit_unavailable_store() {
    let Some(tools) = tools() else {
        return;
    };
    let (dir, _, _) = repository(&tools);
    fs::remove_dir(dir.path().join(".git/objects/info")).unwrap();
    assert!(matches!(
        Reader::from_store(
            &crew::vcs::Store::Git {
                git_dir: dir.path().join(".git")
            },
            tools
        ),
        Err(DocFailureCode::HistoryUnavailable)
    ));
    assert!(!dir.path().join(".git/objects/info").exists());
}
