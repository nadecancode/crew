//! Service-port acceptance. Gateway framing, platform openers and frontend
//! rendering retain separate integration gates; these do not pretend to render.
use super::*;
use crate::docs::config::Layer;
use std::fs;
use tempfile::TempDir;
fn fixture() -> (TempDir, Service, ConnCtx) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("notes")).unwrap();
    fs::write(dir.path().join("notes/a.md"), "# Alpha\n\nHello.\n").unwrap();
    let run = RunCtx {
        workspace_run_id: index::id(),
    };
    let context = ConnCtx::new(index::id(), run.clone(), Caller::Operator);
    let service = Service::new(run, Policy::default());
    service
        .replace_sources(vec![registration(
            "workspace",
            dir.path().into(),
            DocSourceKind::Workspace,
        )])
        .unwrap();
    (dir, service, context)
}
fn registration(key: &str, root: std::path::PathBuf, kind: DocSourceKind) -> Registration {
    Registration {
        key: Id::new(key).unwrap(),
        root,
        kind,
        member_ids: vec![key.into()],
        display_name: key.into(),
        branch: None,
        head_commit: None,
        history: None,
    }
}
async fn complete(service: &Service, context: &ConnCtx) -> DocIndexPage {
    for _ in 0..1000 {
        let page: DocIndexPage = serde_json::from_value(
            service
                .handle(context, Call::DocsIndex(PageRequest::default()))
                .await
                .unwrap(),
        )
        .unwrap();
        if page.phase == DocPhase::Ready {
            return page;
        }
    }
    panic!("index did not finish");
}
fn reference(page: &DocIndexPage, path: &str) -> DocRef {
    let entry = page
        .entries
        .iter()
        .find(|e| &*e.relative_path == path)
        .unwrap();
    let source = page
        .sources
        .iter()
        .find(|s| s.kind == DocSourceKind::Workspace)
        .unwrap();
    let v = entry
        .variants
        .iter()
        .find(|v| v.source_id == source.id)
        .unwrap();
    DocRef {
        doc_id: entry.id.clone(),
        source_id: v.source_id.clone(),
        source_generation: v.source_generation,
    }
}
async fn read(service: &Service, context: &ConnCtx, document: DocRef) -> DocSnapshot {
    let result: DocReadResult = serde_json::from_value(
        service
            .handle(
                context,
                Call::DocsRead(DocsRead {
                    document,
                    version: DocVersionSelector::Current,
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let DocReadResult::Available(value) = result else {
        panic!("read failed: {result:?}");
    };
    *value
}

#[tokio::test]
async fn k01_k02_k03_shared_registered_sources_and_policy_ignore_gitignore() {
    let (dir, service, ctx) = fixture();
    let member = dir.path().join(".crew/worktrees/a");
    fs::create_dir_all(member.join("notes")).unwrap();
    fs::write(member.join("notes/member.md"), "# Member").unwrap();
    fs::write(member.join("notes/a.md"), "# Alpha\n\nHello.\n").unwrap();
    fs::create_dir_all(member.join(".crew")).unwrap();
    fs::write(member.join(".crew/private.md"), "secret").unwrap();
    fs::write(dir.path().join(".gitignore"), "notes\n").unwrap();
    service
        .replace_sources(vec![
            registration("workspace", dir.path().into(), DocSourceKind::Workspace),
            registration("shared", dir.path().into(), DocSourceKind::Shared),
            registration("member", member, DocSourceKind::Member),
        ])
        .unwrap();
    let page = complete(&service, &ctx).await;
    assert_eq!(page.sources.len(), 2);
    assert_eq!(page.entries.len(), 2);
    let a = page
        .entries
        .iter()
        .find(|e| &*e.relative_path == "notes/a.md")
        .unwrap();
    assert!(a
        .variants
        .iter()
        .all(|v| v.comparison == DocComparison::SameMarkdown));
    let before = service.state().index.policy.clone();
    assert!(service
        .configure(["[docs]\nextra_roots=['../private']"])
        .is_err());
    assert_eq!(service.state().index.policy, before);
    service
        .configure(["[docs]\nexclude=['notes/member.md']"])
        .unwrap();
    assert_eq!(complete(&service, &ctx).await.entries.len(), 1);
    service.replace_policy(
        Policy::layered([Layer {
            enabled: Some(false),
            ..Default::default()
        }])
        .unwrap(),
    );
    let page: DocIndexPage = serde_json::from_value(
        service
            .handle(&ctx, Call::DocsIndex(PageRequest::default()))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(page.phase, DocPhase::Disabled);
    assert!(page.entries.is_empty());
    assert_eq!(service.state().cache_bytes, 0);
}

#[tokio::test]
async fn k05_k07_k16_chunks_and_seen_captures_never_substitute_current_bytes() {
    let (dir, service, ctx) = fixture();
    fs::write(
        dir.path().join("notes/a.md"),
        "# Alpha\n![pixel](pixel.png)\n![remote](https://example.invalid/no-fetch)\n",
    )
    .unwrap();
    let image = include_bytes!("fixtures/pixel.png");
    fs::write(dir.path().join("notes/pixel.png"), image).unwrap();
    let page = complete(&service, &ctx).await;
    let doc = reference(&page, "notes/a.md");
    let first = read(&service, &ctx, doc.clone()).await;
    assert_eq!(first.images[0].state, DocImageState::Ready);
    assert_eq!(first.images[1].state, DocImageState::Remote);
    let asset = first.images[0].asset_id.clone().unwrap();
    fs::write(dir.path().join("notes/pixel.png"), b"changed").unwrap();
    let (mut bytes, mut offset) = (Vec::new(), 0);
    loop {
        let result: DocAssetResult = serde_json::from_value(
            service
                .handle(
                    &ctx,
                    Call::DocsAsset(DocsAsset {
                        snapshot_id: first.snapshot_id.clone(),
                        asset_id: asset.clone(),
                        offset,
                        max_bytes: 17,
                    }),
                )
                .await
                .unwrap(),
        )
        .unwrap();
        let DocAssetResult::Available(c) = result else {
            panic!("asset failed");
        };
        bytes.extend(
            base64::engine::general_purpose::STANDARD
                .decode(c.data_base64.as_bytes())
                .unwrap(),
        );
        if let Some(next) = c.next_offset {
            offset = next;
        } else {
            break;
        }
    }
    assert_eq!(bytes, image);
    fs::write(dir.path().join("notes/a.md"), "# Changed\n").unwrap();
    let old: DocReadResult = serde_json::from_value(
        service
            .handle(
                &ctx,
                Call::DocsRead(DocsRead {
                    document: doc.clone(),
                    version: DocVersionSelector::Id(first.version.id.clone()),
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(matches!(old,DocReadResult::Available(s) if s.markdown==first.markdown));
    for n in 0..17 {
        fs::write(dir.path().join("notes/a.md"), format!("# Save {n}\n")).unwrap();
        read(&service, &ctx, doc.clone()).await;
    }
    assert_eq!(
        service.state().connections[&ctx.connection_epoch]
            .versions
            .len(),
        16
    );
    let old: DocReadResult = serde_json::from_value(
        service
            .handle(
                &ctx,
                Call::DocsRead(DocsRead {
                    document: doc,
                    version: DocVersionSelector::Id(first.version.id.clone()),
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(matches!(old,DocReadResult::Unavailable(f) if f.code==DocFailureCode::SnapshotExpired));
}

#[tokio::test]
async fn k12_p08_agent_wrong_run_reconnect_revoke_and_removal_fail_closed() {
    let (dir, service, ctx) = fixture();
    let page = complete(&service, &ctx).await;
    let doc = reference(&page, "notes/a.md");
    let snapshot = read(&service, &ctx, doc.clone()).await;
    for other in [
        ConnCtx::new(
            index::id(),
            ctx.run.clone(),
            Caller::Agent {
                id: "orchestrator".into(),
            },
        ),
        ConnCtx::new(
            index::id(),
            RunCtx {
                workspace_run_id: index::id(),
            },
            Caller::Operator,
        ),
    ] {
        assert!(service
            .handle(&other, Call::DocsIndex(PageRequest::default()))
            .await
            .is_err());
    }
    let next = ConnCtx::new(index::id(), ctx.run.clone(), Caller::Operator);
    let result: DocReadResult = serde_json::from_value(
        service
            .handle(
                &next,
                Call::DocsRead(DocsRead {
                    document: doc.clone(),
                    version: DocVersionSelector::Id(snapshot.version.id.clone()),
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        matches!(result,DocReadResult::Unavailable(f) if f.code==DocFailureCode::SnapshotExpired)
    );
    service.connection_closed(&ctx.connection_epoch);
    assert!(service
        .handle(&ctx, Call::DocsIndex(PageRequest::default()))
        .await
        .is_err());
    service.replace_sources(Vec::new()).unwrap();
    let result: DocReadResult = serde_json::from_value(
        service
            .handle(
                &next,
                Call::DocsRead(DocsRead {
                    document: doc,
                    version: DocVersionSelector::Current,
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(matches!(result, DocReadResult::Unavailable(_)));
    assert_eq!(
        fs::read_to_string(dir.path().join("notes/a.md")).unwrap(),
        "# Alpha\n\nHello.\n"
    );
    assert!(!dir.path().join("crew.db").exists());
    service.run_archived(&next.run);
    let new = ConnCtx::new(index::id(), next.run.clone(), Caller::Operator);
    assert!(service
        .handle(&new, Call::DocsIndex(PageRequest::default()))
        .await
        .is_err());
}

#[tokio::test]
async fn k08_k13_json_expansion_and_worker_pressure_fail_bounded_cursors_are_scoped() {
    let (dir, service, ctx) = fixture();
    fs::write(
        dir.path().join("notes/a.md"),
        format!("```\n{}\n```", "\u{0001}".repeat(100_000)),
    )
    .unwrap();
    fs::write(dir.path().join("notes/b.md"), "# B").unwrap();
    let page = complete(&service, &ctx).await;
    let doc = reference(&page, "notes/a.md");
    let value = service
        .handle(
            &ctx,
            Call::DocsRead(DocsRead {
                document: doc,
                version: DocVersionSelector::Current,
            }),
        )
        .await
        .unwrap();
    assert!(
        matches!(serde_json::from_value::<DocReadResult>(value.clone()).unwrap(),DocReadResult::Unavailable(f) if f.code==DocFailureCode::TooLarge)
    );
    assert!(serde_json::to_vec(&value).unwrap().len() < 1024);
    let page: DocIndexPage = serde_json::from_value(
        service
            .handle(
                &ctx,
                Call::DocsIndex(PageRequest {
                    limit: Some(1),
                    cursor: None,
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let cursor = page.next_cursor.unwrap();
    let other = ConnCtx::new(index::id(), ctx.run.clone(), Caller::Operator);
    assert!(service
        .handle(
            &other,
            Call::DocsIndex(PageRequest {
                cursor: Some(cursor.clone()),
                limit: Some(1)
            })
        )
        .await
        .is_err());
    let rest: DocIndexPage = serde_json::from_value(
        service
            .handle(
                &ctx,
                Call::DocsIndex(PageRequest {
                    cursor: Some(cursor.clone()),
                    limit: Some(1),
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(rest.entries.len(), 1);
    assert!(service
        .handle(
            &ctx,
            Call::DocsIndex(PageRequest {
                cursor: Some(cursor),
                limit: Some(1)
            })
        )
        .await
        .is_err());
    let permit = service
        .inner
        .reads
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    assert!(service
        .handle(&ctx, Call::DocsIndex(PageRequest::default()))
        .await
        .is_err());
    drop(permit);
}

#[tokio::test]
async fn k09_k10_k11_watch_barrier_bursts_trailing_images_and_cancel() {
    let (dir, service, ctx) = fixture();
    fs::write(
        dir.path().join("notes/a.md"),
        "# Alpha\n![pixel](pixel.png)\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("notes/pixel.png"),
        include_bytes!("fixtures/pixel.png"),
    )
    .unwrap();
    let page = complete(&service, &ctx).await;
    let doc = reference(&page, "notes/a.md");
    read(&service, &ctx, doc.clone()).await;
    let result = service
        .state()
        .watch(
            &ctx,
            DocsWatch {
                selected: List::new(vec![doc.clone()]).unwrap(),
            },
        )
        .unwrap();
    let mut stream = service
        .watch_stream(ctx.clone(), result.watch_id.clone())
        .unwrap();
    for n in 0..20 {
        fs::write(
            dir.path().join("notes/a.md"),
            format!("# Save {n}\n![pixel](pixel.png)\n"),
        )
        .unwrap();
        let mut s = service.state();
        s.index.rescan();
        for _ in 0..10 {
            s.index.advance();
        }
    }
    service.state().publish(false);
    let push = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let crew_core::rpc::Push::DocsChanged(changed) = push else {
        panic!("wrong push");
    };
    assert_eq!(changed.from_revision, result.initial_index_page.revision);
    assert!(changed.to_revision.0 > changed.from_revision.0);
    assert!(changed.changed_selected.iter().any(|c| c.document == doc));
    assert!(
        tokio::time::timeout(Duration::from_millis(10), stream.next())
            .await
            .is_err()
    );
    fs::write(
        dir.path().join("notes/pixel.png"),
        include_bytes!("fixtures/pixel.jpg"),
    )
    .unwrap();
    {
        let mut s = service.state();
        s.check_images();
        s.publish(false);
    }
    let push = stream.next().await.unwrap().unwrap();
    assert!(
        matches!(push,crew_core::rpc::Push::DocsChanged(c) if c.changed_selected.iter().any(|s|s.kind==DocChangeKind::Image))
    );
    fs::remove_file(dir.path().join("notes/a.md")).unwrap();
    {
        let mut s = service.state();
        s.index.rescan();
        for _ in 0..10 {
            s.index.advance();
        }
        s.publish(false);
    }
    let push = stream.next().await.unwrap().unwrap();
    assert!(
        matches!(push,crew_core::rpc::Push::DocsChanged(c) if c.upserts.iter().any(|e|e.variants.iter().any(|v|v.state==DocState::Missing)))
    );
    ctx.cancellation.cancel();
    assert!(stream.next().await.unwrap().is_none());
    service.tick();
    assert_eq!(service.state().watch_mode, DocWatchMode::None);
}

#[tokio::test]
async fn k17_k18_diff_and_navigation_use_known_paths_and_immutable_versions() {
    let (dir, service, ctx) = fixture();
    let original =
        "# Alpha\n[b](b.md#beta) [bad](../../private) [js](javascript:alert(1))\none\ntwo\n";
    fs::write(dir.path().join("notes/a.md"), original).unwrap();
    fs::write(dir.path().join("notes/b.md"), "# Beta").unwrap();
    let page = complete(&service, &ctx).await;
    let doc = reference(&page, "notes/a.md");
    let first = read(&service, &ctx, doc.clone()).await;
    let json = serde_json::to_string(&first.blocks).unwrap();
    assert!(json.contains("preferred_source"));
    assert!(json.contains("\"anchor\":\"beta\""));
    assert!(!json.contains("\"href\":\"javascript:"));
    fs::write(
        dir.path().join("notes/a.md"),
        original.replace("two", "three"),
    )
    .unwrap();
    let second = read(&service, &ctx, doc).await;
    let result: DocDiffResult = serde_json::from_value(
        service
            .handle(
                &ctx,
                Call::DocsDiff(DocsDiff {
                    left: first.version.id.clone(),
                    right: second.version.id.clone(),
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let DocDiffResult::Available(diff) = result else {
        panic!("diff unavailable");
    };
    assert_eq!((diff.added_lines, diff.removed_lines), (1, 1));
    assert!(diff.unified_diff.contains("-two\n"));
    assert!(diff.unified_diff.contains("+three\n"));
    assert!(!diff.truncated);
}

#[tokio::test]
async fn k13_cache_pressure_expires_an_old_capture_without_serving_new_content() {
    let (dir, service, ctx) = fixture();
    let page = complete(&service, &ctx).await;
    let doc = reference(&page, "notes/a.md");
    let old = read(&service, &ctx, doc.clone()).await;
    // Account one retained synthetic capture as consuming the budget, exercising
    // the real global eviction path without a test allocating hundreds of MiB.
    {
        let mut state = service.state();
        let retained = state
            .connections
            .get_mut(&ctx.connection_epoch)
            .unwrap()
            .versions
            .get_mut(&old.version.id)
            .unwrap();
        Arc::get_mut(&mut retained.value).unwrap().charge = CACHE_BYTES;
        state.cache_bytes = CACHE_BYTES;
    }
    fs::write(dir.path().join("notes/a.md"), "# New\n").unwrap();
    let latest = read(&service, &ctx, doc.clone()).await;
    assert_ne!(latest.version.id, old.version.id);
    assert!(service.state().cache_bytes < CACHE_BYTES);
    let result: DocReadResult = serde_json::from_value(
        service
            .handle(
                &ctx,
                Call::DocsRead(DocsRead {
                    document: doc,
                    version: DocVersionSelector::Id(old.version.id.clone()),
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        matches!(result,DocReadResult::Unavailable(f) if f.code==DocFailureCode::SnapshotExpired)
    );
}

#[tokio::test]
async fn k09_k10_late_directory_capture_and_trailing_revision_survive_one_pending_push() {
    let (dir, service, ctx) = fixture();
    let page = complete(&service, &ctx).await;
    let doc = reference(&page, "notes/a.md");
    let result = service
        .state()
        .watch(
            &ctx,
            DocsWatch {
                selected: List::new(vec![doc]).unwrap(),
            },
        )
        .unwrap();
    let mut stream = service.watch_stream(ctx.clone(), result.watch_id).unwrap();
    // Edit after initial capture but before any consumer asks for a push.
    fs::create_dir_all(dir.path().join("notes/new/deep")).unwrap();
    fs::write(
        dir.path().join("notes/new/deep/untracked.md"),
        "# Newly created\n",
    )
    .unwrap();
    {
        let mut state = service.state();
        state.index.rescan();
        for _ in 0..20 {
            state.advance();
        }
        state.publish(false);
    }
    fs::write(dir.path().join("notes/a.md"), "# Trailing save\n").unwrap();
    {
        let mut state = service.state();
        state.index.rescan();
        for _ in 0..20 {
            state.advance();
        }
        state.publish(false);
    }
    let push = stream.next().await.unwrap().unwrap();
    let crew_core::rpc::Push::DocsChanged(change) = push else {
        panic!("wrong push");
    };
    assert!(change.resync_required);
    assert_eq!(change.from_revision, result.initial_index_page.revision);
    let page = complete(&service, &ctx).await;
    assert!(page
        .entries
        .iter()
        .any(|e| &*e.relative_path == "notes/new/deep/untracked.md"));
    assert!(page.entries.iter().any(|e| e
        .variants
        .iter()
        .any(|v| v.title.as_ref().is_some_and(|t| &**t == "Trailing save"))));
    assert!(
        tokio::time::timeout(Duration::from_millis(10), stream.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn k10_native_save_burst_publishes_at_most_once_per_second_and_the_final_save() {
    let (dir, service, ctx) = fixture();
    let page = complete(&service, &ctx).await;
    let doc = reference(&page, "notes/a.md");
    let result: DocsWatchResult = serde_json::from_value(
        service
            .handle(
                &ctx,
                Call::DocsWatch(DocsWatch {
                    selected: List::new(vec![doc]).unwrap(),
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let mut stream = service.watch_stream(ctx.clone(), result.watch_id).unwrap();
    let path = dir.path().join("notes/a.md");
    let writer = tokio::spawn(async move {
        for n in 0..20 {
            fs::write(&path, format!("# Burst {n}\n")).unwrap();
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
    });
    let end = tokio::time::Instant::now()
        + if result.initial_index_page.watch_mode == DocWatchMode::Events {
            Duration::from_millis(3400)
        } else {
            Duration::from_millis(6400)
        };
    let mut times = Vec::new();
    while let Ok(Some(_)) = tokio::time::timeout_at(end, stream.next())
        .await
        .unwrap_or(Ok(None))
    {
        times.push(Instant::now());
    }
    writer.await.unwrap();
    assert!(!times.is_empty());
    assert!(
        times
            .windows(2)
            .all(|pair| pair[1].duration_since(pair[0]) >= Duration::from_millis(900)),
        "burst frames were not coalesced: {times:?}"
    );
    let page = complete(&service, &ctx).await;
    assert!(page.entries.iter().any(|e| e
        .variants
        .iter()
        .any(|v| v.title.as_ref().is_some_and(|t| &**t == "Burst 19"))));
    service.connection_closed(&ctx.connection_epoch);
}

#[tokio::test]
async fn p08_k16_coalesced_expiries_never_keep_an_omitted_old_handle_usable() {
    let (dir, service, ctx) = fixture();
    let page = complete(&service, &ctx).await;
    let doc = reference(&page, "notes/a.md");
    let watched = service
        .state()
        .watch(
            &ctx,
            DocsWatch {
                selected: List::new(vec![doc.clone()]).unwrap(),
            },
        )
        .unwrap();
    let mut stream = service.watch_stream(ctx.clone(), watched.watch_id).unwrap();
    // Queue a history invalidation before the long sequence of expiry metadata.
    {
        let mut state = service.state();
        state.index.revision.0 += 1;
        state.publish(true);
    }
    let mut ids = Vec::new();
    for n in 0..240 {
        fs::write(dir.path().join("notes/a.md"), format!("# Version {n}\n")).unwrap();
        ids.push(
            service
                .read(
                    &ctx,
                    DocsRead {
                        document: doc.clone(),
                        version: DocVersionSelector::Current,
                    },
                )
                .unwrap()
                .version
                .id
                .clone(),
        );
    }
    let push = stream.next().await.unwrap().unwrap();
    let crew_core::rpc::Push::DocsChanged(change) = push else {
        panic!("wrong push");
    };
    assert!(change.resync_required);
    assert_eq!(change.expired_seen_versions.len(), 200);
    assert!(!change.history_sources_changed.is_empty());
    let omitted = {
        let state = service.state();
        ids.iter()
            .find(|id| {
                !change.expired_seen_versions.contains(id)
                    && !state.connections[&ctx.connection_epoch]
                        .versions
                        .contains_key(id)
            })
            .unwrap()
            .clone()
    };
    let result: DocReadResult = serde_json::from_value(
        service
            .handle(
                &ctx,
                Call::DocsRead(DocsRead {
                    document: doc,
                    version: DocVersionSelector::Id(omitted),
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        matches!(result,DocReadResult::Unavailable(f) if f.code==DocFailureCode::SnapshotExpired)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p08_cancelled_awaiter_does_not_release_a_blocking_job_permit_early() {
    let (_dir, service, ctx) = fixture();
    let page = complete(&service, &ctx).await;
    let doc = reference(&page, "notes/a.md");
    let owned = service.clone();
    let caller = ctx.clone();
    let guard = service.state();
    let task = tokio::spawn(async move {
        owned
            .handle(
                &caller,
                Call::DocsRead(DocsRead {
                    document: doc,
                    version: DocVersionSelector::Current,
                }),
            )
            .await
    });
    // The queued blocking worker cannot enter admit while this guard is held.
    // The other Tokio worker can still cancel its awaiter independently.
    let deadline = Instant::now() + Duration::from_secs(1);
    while service.inner.reads.available_permits() != 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(service.inner.reads.available_permits(), 1);
    ctx.cancellation.cancel();
    while !task.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(task.is_finished());
    assert_eq!(service.inner.reads.available_permits(), 1);
    drop(guard);
    assert!(task.await.unwrap().is_err());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while service.inner.reads.available_permits() != 2 && tokio::time::Instant::now() < deadline {
        tokio::task::yield_now().await;
    }
    assert_eq!(service.inner.reads.available_permits(), 2);
    assert_eq!(service.state().cache_bytes, 0);
}
