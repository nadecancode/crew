//! Domain projection inside the same task write transaction and model fence.

use crew_core::rpc::epics::{EpicBoard, EpicBoardChanged};
use crew_core::rpc::features::Id;
use crew_core::rpc::{Push, RpcError, Snapshot};
use crew_core::{Event, Task};
use rusqlite::{Connection, OptionalExtension, Transaction};

use super::super::server::Conn;
use super::super::Daemon;

pub struct TaskView {
    pub board: EpicBoard,
    pub tasks: Vec<Task>,
}

pub struct TaskPublication {
    /// Durable domain outbox sequence, acknowledged only after publication.
    pub sequence: Option<i64>,
    pub delta: EpicBoardChanged,
    pub events: Vec<Event>,
    pub outbox: Vec<crate::message_bus::SendOutcome>,
}

/// Hydrated upserts and an optional grouped publication. A receipt retry or
/// unchanged edit can commit a read without manufacturing a board revision.
pub struct TaskChange {
    pub tasks: Vec<Task>,
    pub publication: Option<TaskPublication>,
}

pub struct CommittedTasks<T> {
    pub result: T,
    pub change: TaskChange,
    pub reopen_session: bool,
}

/// The Epic store implements this port without locking/committing. All task
/// states are ordered and borrowed under the caller's authoritative model fence.
pub trait TaskProjection: Send + Sync {
    fn snapshot(&self, conn: &Connection, run: &Id, tasks: &[Task]) -> Result<TaskView, RpcError>;
    fn apply_delta(
        &self,
        tx: &Transaction<'_>,
        run: &Id,
        before: &[Task],
        after: &[Task],
        changed_ids: &[String],
        now: i64,
    ) -> Result<TaskChange, RpcError>;

    fn acknowledge(
        &self,
        _tx: &Transaction<'_>,
        _run: &Id,
        _sequence: i64,
    ) -> Result<(), RpcError> {
        Err(RpcError::internal(
            "domain publication acknowledgement unavailable",
        ))
    }

    /// Explicit new-run clearing shares the transaction with legacy state.
    /// An old binary's metadata clear deliberately does not invoke this hook.
    fn clear_run(&self, _tx: &Transaction<'_>, _run: &Id) -> Result<(), RpcError> {
        Ok(())
    }
}

impl Daemon {
    pub(crate) fn feature_snapshot(
        &self,
        conn: &Conn,
        messages: usize,
        events: usize,
    ) -> Result<Snapshot, RpcError> {
        let mut snapshot = self.snapshot(messages, events)?;
        let epics = conn.has_feature(crew_core::rpc::features::Feature::Epics);
        if !epics {
            return Ok(snapshot.legacy_projection());
        }
        let projection = self
            .feature_services
            .task_projection()
            .ok_or_else(|| RpcError::new("unsupported_feature", "epic projection unavailable"))?;
        let model = self.model();
        let run = self.run_context()?;
        let view = self
            .feature_db
            .transaction(Some(&run.workspace_run_id), |db| {
                projection.snapshot(db, &run.workspace_run_id, &model.tasks)
            })?;
        validate_view(&run.workspace_run_id, &model.tasks, &view)?;
        snapshot.tasks = view.tasks;
        snapshot.epics = Some(view.board);
        snapshot.session.workspace_run_id = Some(run.workspace_run_id.as_str().to_string());
        Ok(snapshot)
    }

    pub(crate) fn publish_task_publication(&self, publication: TaskPublication) {
        let run = publication.delta.workspace_run_id.clone();
        self.push(Push::EpicBoardChanged(Box::new(publication.delta)));
        for event in publication.events {
            self.push(Push::Event(event));
        }
        // Outbox rows and deliveries are already committed by the callback.
        // Delivery is requested here; never insert or ask a second question.
        for outcome in publication.outbox {
            self.bus.publish_committed(&outcome);
            self.refresh_counts(&outcome.recipients);
            for recipient in &outcome.recipients {
                self.request_delivery(recipient);
            }
        }
        if let (Some(sequence), Some(projection)) = (
            publication.sequence,
            self.feature_services.task_projection(),
        ) {
            // Failure retains the durable outbox for replay; never turn a
            // successfully committed domain mutation into an apparent failure.
            if let Err(error) = self
                .feature_db
                .transaction(Some(&run), |tx| projection.acknowledge(tx, &run, sequence))
            {
                tracing::warn!(code = %error.code, "feature outbox acknowledgement deferred");
            }
        }
    }

    /// The callback owns domain validation and transaction-local writes. It
    /// receives the authoritative model under its fence, and must not acquire
    /// another model/store lock. Publication and model changes follow commit.
    pub(crate) fn commit_tasks_with<T>(
        &self,
        run: &Id,
        write: impl FnOnce(
            &Transaction<'_>,
            &super::super::Model,
        ) -> Result<CommittedTasks<T>, RpcError>,
    ) -> Result<T, RpcError> {
        let (committed, paused) = {
            let mut model = self.model();
            let committed = self.feature_db.transaction(Some(run), |tx| {
                let committed = write(tx, &model)?;
                let mut after = model.tasks.clone();
                install_upserts(&mut after, &committed.change.tasks)?;
                validate_change(tx, run, &after, &[], &committed.change)?;
                Ok(committed)
            })?;
            let mut paused = Vec::new();
            for task in &committed.change.tasks {
                if let Some(before) = model.task(&task.id).cloned() {
                    if let Some(agent) =
                        super::super::overlap::track_pause(&mut model, &before, task)
                    {
                        paused.push(agent);
                    }
                }
            }
            install_upserts(&mut model.tasks, &committed.change.tasks)?;
            if committed.reopen_session {
                model.session.completed_at = None;
            }
            (committed, paused)
        };
        for agent in paused {
            self.save_local(&agent);
        }
        for task in &committed.change.tasks {
            self.push(Push::Task(Box::new(task.clone())));
        }
        if let Some(publication) = committed.change.publication {
            self.publish_task_publication(publication);
        }
        if committed.reopen_session {
            self.save_session();
        }
        Ok(committed.result)
    }

    pub(crate) fn persist_task_projection(
        &self,
        before: &[Task],
        after: &[Task],
        changed_ids: &[String],
    ) -> Result<Option<TaskChange>, RpcError> {
        let Some(projection) = self.feature_services.task_projection() else {
            return Ok(None);
        };
        let run = self.run_context()?.workspace_run_id;
        self.feature_db.transaction(Some(&run), |tx| {
            Ok(Some(project_in(
                projection.as_ref(),
                tx,
                &run,
                before,
                after,
                changed_ids,
            )?))
        })
    }

    pub(crate) fn persist_task_telling_projection(
        &self,
        before: &[Task],
        after: &[Task],
        id: &str,
        new: crate::message_bus::NewMessage,
        recipients: Vec<String>,
    ) -> Result<(Option<TaskChange>, crate::message_bus::SendOutcome), RpcError> {
        if !self.bus.supports_external_commit() {
            return Err(RpcError::new(
                "unsupported_feature",
                "message bus cannot share feature transaction",
            ));
        }
        let projection = self
            .feature_services
            .task_projection()
            .ok_or_else(|| RpcError::new("unsupported_feature", "epic projection unavailable"))?;
        let message = crate::message_bus::prepare_message(new, &recipients, self.bus.max_bytes())?;
        let run = self.run_context()?.workspace_run_id;
        self.feature_db.transaction(Some(&run), |tx| {
            if let Some(original) = crate::persistence::sqlite::message_by_id_in(tx, &message.id)
                .map_err(|_| RpcError::new("internal", "message lookup failed"))?
            {
                if original.from != message.from {
                    return Err(crate::message_bus::BusError::IdConflict(message.id.clone()).into());
                }
                let recipients = crate::persistence::sqlite::recipients_in(tx, &original.id)
                    .map_err(|_| RpcError::new("internal", "message recipient lookup failed"))?;
                return Ok((
                    None,
                    crate::message_bus::SendOutcome {
                        message: original,
                        recipients,
                        duplicate: true,
                    },
                ));
            }
            let inserted = crate::persistence::sqlite::insert_message_in(tx, &message, &recipients)
                .map_err(|_| RpcError::new("internal", "message/task persistence failed"))?;
            if !inserted {
                return Err(RpcError::new(
                    "internal",
                    "message insert changed inside transaction",
                ));
            }
            let publication = project_in(
                projection.as_ref(),
                tx,
                &run,
                before,
                after,
                &[id.to_string()],
            )?;
            Ok((
                Some(publication),
                crate::message_bus::SendOutcome {
                    message,
                    recipients,
                    duplicate: false,
                },
            ))
        })
    }
}

pub(crate) fn validate_view(run: &Id, ordered: &[Task], view: &TaskView) -> Result<(), RpcError> {
    if &view.board.generation != run || view.tasks.len() != ordered.len() {
        return Err(RpcError::new("internal", "invalid epic task projection"));
    }
    for (before, after) in ordered.iter().zip(&view.tasks) {
        let mut before = before.clone();
        let mut after = after.clone();
        before.epic = None;
        after.epic = None;
        if before != after {
            return Err(RpcError::new(
                "internal",
                "epic projection changed task state or order",
            ));
        }
    }
    Ok(())
}

fn project_in(
    projection: &dyn TaskProjection,
    tx: &Transaction<'_>,
    run: &Id,
    before: &[Task],
    after: &[Task],
    changed_ids: &[String],
) -> Result<TaskChange, RpcError> {
    let change = projection.apply_delta(
        tx,
        run,
        before,
        after,
        changed_ids,
        crew_core::util::now_ms(),
    )?;
    validate_change(tx, run, after, changed_ids, &change)?;
    Ok(change)
}

fn validate_change(
    tx: &Transaction<'_>,
    run: &Id,
    after: &[Task],
    changed_ids: &[String],
    change: &TaskChange,
) -> Result<(), RpcError> {
    if let Some(publication) = &change.publication {
        if publication.delta.workspace_run_id != *run
            || publication.delta.board.generation != *run
            || publication.delta.to_revision != publication.delta.board.revision
            || publication.delta.from_revision >= publication.delta.to_revision
        {
            return Err(RpcError::new("internal", "invalid epic task delta"));
        }
        if publication.delta.tasks != change.tasks {
            return Err(RpcError::internal(
                "epic publication and committed upserts differ",
            ));
        }
    }
    if changed_ids
        .iter()
        .any(|id| !change.tasks.iter().any(|task| &task.id == id))
    {
        return Err(RpcError::new(
            "internal",
            "epic delta omitted a changed task",
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for task in &change.tasks {
        if !seen.insert(&task.id) {
            return Err(RpcError::new("internal", "duplicate task in epic delta"));
        }
        let expected = after
            .iter()
            .find(|item| item.id == task.id)
            .ok_or_else(|| RpcError::new("internal", "unexpected task in epic delta"))?;
        let mut actual = task.clone();
        let mut expected = expected.clone();
        actual.epic = None;
        expected.epic = None;
        if actual != expected {
            return Err(RpcError::new("internal", "epic delta changed task state"));
        }
        let stored: Option<String> = tx
            .query_row("SELECT data FROM tasks WHERE id=?1", [&task.id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(crate::persistence::features::storage_error)?;
        let stored: Option<Task> = stored
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(|_| RpcError::internal("invalid committed task"))?;
        if stored.as_ref() != Some(task) {
            return Err(RpcError::internal("domain task upsert was not persisted"));
        }
    }
    Ok(())
}

fn install_upserts(tasks: &mut Vec<Task>, upserts: &[Task]) -> Result<(), RpcError> {
    let mut seen = std::collections::HashSet::new();
    for task in upserts {
        if !seen.insert(&task.id) {
            return Err(RpcError::internal("duplicate committed task"));
        }
        match tasks.iter_mut().find(|old| old.id == task.id) {
            Some(old) => *old = task.clone(),
            None => tasks.push(task.clone()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_core::rpc::epics::EpicCounts;
    use crew_core::rpc::features::Wide;
    use crew_core::{MessageType, TaskStatus};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    struct Projection {
        fail: AtomicBool,
    }

    fn board(run: &Id, tasks: &[Task], revision: u128) -> EpicBoard {
        EpicBoard {
            generation: run.clone(),
            revision: Wide(revision),
            epics: Vec::new(),
            no_epic: EpicCounts {
                total: Wide(tasks.len() as u128),
                ..Default::default()
            },
            focus_epic: None,
        }
    }

    impl TaskProjection for Projection {
        fn snapshot(
            &self,
            conn: &Connection,
            run: &Id,
            tasks: &[Task],
        ) -> Result<TaskView, RpcError> {
            let revision: i64 = conn
                .query_row("SELECT revision FROM projection_fixture", [], |row| {
                    row.get(0)
                })
                .map_err(crate::persistence::features::storage_error)?;
            Ok(TaskView {
                board: board(run, tasks, revision as u128),
                tasks: tasks.to_vec(),
            })
        }
        fn apply_delta(
            &self,
            tx: &Transaction<'_>,
            run: &Id,
            before: &[Task],
            after: &[Task],
            changed: &[String],
            _: i64,
        ) -> Result<TaskChange, RpcError> {
            assert!(before
                .iter()
                .all(|task| after.iter().any(|current| current.id == task.id)));
            let revision: i64 = tx
                .query_row("SELECT revision FROM projection_fixture", [], |row| {
                    row.get(0)
                })
                .map_err(crate::persistence::features::storage_error)?;
            tx.execute("UPDATE projection_fixture SET revision=revision+1", [])
                .map_err(crate::persistence::features::storage_error)?;
            if self.fail.load(Ordering::SeqCst) {
                return Err(RpcError::conflict("projection fixture rollback"));
            }
            let tasks: Vec<_> = after
                .iter()
                .filter(|task| changed.contains(&task.id))
                .cloned()
                .collect();
            for task in &tasks {
                crate::persistence::sqlite::save_task_in(tx, task)
                    .map_err(|_| RpcError::internal("fixture task persistence failed"))?;
            }
            Ok(TaskChange {
                tasks: tasks.clone(),
                publication: Some(TaskPublication {
                    sequence: None,
                    delta: EpicBoardChanged {
                        workspace_run_id: run.clone(),
                        from_revision: Wide(revision as u128),
                        to_revision: Wide(revision as u128 + 1),
                        board: board(run, after, revision as u128 + 1),
                        tasks,
                    },
                    events: Vec::new(),
                    outbox: Vec::new(),
                }),
            })
        }
    }

    async fn fixture() -> (
        tempfile::TempDir,
        super::super::super::Running,
        Arc<Projection>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let running = super::super::super::start(super::super::super::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        running.daemon.feature_db.transaction(None,|tx|tx.execute_batch("CREATE TABLE projection_fixture(revision INTEGER);INSERT INTO projection_fixture VALUES(0);").map_err(crate::persistence::features::storage_error)).unwrap();
        let projection = Arc::new(Projection {
            fail: AtomicBool::new(false),
        });
        running
            .daemon
            .feature_services
            .register_task_projection(projection.clone());
        (temp, running, projection)
    }

    #[tokio::test]
    async fn failed_domain_projection_rolls_back_task_model_sql_and_messages() {
        let (_temp, running, projection) = fixture().await;
        let daemon = &running.daemon;
        let task = daemon
            .create_task(crew_core::config::TaskSpec {
                id: "task".into(),
                ..Default::default()
            })
            .unwrap();
        projection.fail.store(true, Ordering::SeqCst);
        assert!(daemon
            .try_update_task("task", |task| task.status = TaskStatus::Working)
            .is_err());
        assert_eq!(daemon.model().task("task").unwrap(), &task);
        assert_eq!(daemon.store.load_tasks().unwrap(), vec![task.clone()]);
        let message = crate::message_bus::NewMessage::new(
            "operator",
            "worker",
            MessageType::Message,
            "fixture",
        );
        assert!(daemon
            .try_update_task_telling(
                "task",
                |task| {
                    task.status = TaskStatus::Working;
                    Ok(())
                },
                message,
                vec!["worker".into()]
            )
            .is_err());
        assert_eq!(daemon.store.load_tasks().unwrap(), vec![task]);
        assert!(daemon.store.history(None, 10).unwrap().is_empty());
        let revision = daemon
            .feature_db
            .read(|conn| {
                conn.query_row("SELECT revision FROM projection_fixture", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(crate::persistence::features::storage_error)
            })
            .unwrap();
        assert_eq!(revision, 1);
        daemon.shutdown(true).await;
    }

    #[tokio::test]
    async fn committed_message_task_projection_publishes_once_and_duplicate_changes_nothing() {
        let (_temp, running, _) = fixture().await;
        let daemon = &running.daemon;
        daemon
            .create_task(crew_core::config::TaskSpec {
                id: "task".into(),
                ..Default::default()
            })
            .unwrap();
        let mut bus = daemon.bus.subscribe();
        let mut message = crate::message_bus::NewMessage::new(
            "operator",
            "worker",
            MessageType::Message,
            "fixture",
        );
        message.id = Some("projection-message".into());
        let (task, outcome) = daemon
            .try_update_task_telling(
                "task",
                |task| {
                    task.status = TaskStatus::Working;
                    Ok(())
                },
                message.clone(),
                vec!["worker".into()],
            )
            .unwrap()
            .unwrap();
        assert!(!outcome.duplicate);
        assert_eq!(bus.try_recv().unwrap().message.id, "projection-message");
        assert_eq!(daemon.store.load_tasks().unwrap(), vec![task.clone()]);
        let (retried, outcome) = daemon
            .try_update_task_telling(
                "task",
                |task| {
                    task.description = "must not replace".into();
                    Ok(())
                },
                message,
                vec!["worker".into()],
            )
            .unwrap()
            .unwrap();
        assert!(outcome.duplicate);
        assert_eq!(retried, task);
        assert_eq!(daemon.model().task("task").unwrap(), &task);
        assert_eq!(daemon.store.load_tasks().unwrap(), vec![task]);
        assert!(bus.try_recv().is_err());
        daemon.shutdown(true).await;
    }
}
