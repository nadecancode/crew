//! Durable run identity and shared transactions for feature service tables.
//! The run marker is separate from legacy session JSON. An older binary's
//! metadata clear removes that marker; reopening then archives the old run
//! rather than attaching its feature rows to reused legacy ids.

use std::sync::{Arc, MutexGuard};

use anyhow::{Context, Result};
use crew_core::rpc::features::{Id, Timestamp};
use crew_core::rpc::RpcError;
use crew_core::util::now_ms;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use super::SqliteStore;

const MARKER: &str = "feature_run_marker";
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS feature_runs (
    id TEXT PRIMARY KEY,
    started_at INTEGER NOT NULL,
    archived_at INTEGER
);
CREATE TABLE IF NOT EXISTS feature_current_run (
    slot INTEGER PRIMARY KEY CHECK (slot = 1),
    run_id TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS feature_members (
    run_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    incarnation TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    retired_at INTEGER,
    PRIMARY KEY (run_id, agent_id, incarnation)
);
CREATE UNIQUE INDEX IF NOT EXISTS feature_member_active
ON feature_members (run_id, agent_id) WHERE retired_at IS NULL;
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRecord {
    pub id: Id,
    pub started_at: Timestamp,
    pub archived_at: Option<Timestamp>,
}

/// All services share the daemon store's connection and writer mutex. Domain
/// callbacks must not recursively call the store or commit their transaction.
#[derive(Clone)]
pub struct FeatureDb {
    store: Arc<SqliteStore>,
}

impl FeatureDb {
    pub fn new(store: Arc<SqliteStore>) -> Self {
        Self { store }
    }

    pub fn read<T>(
        &self,
        read: impl FnOnce(&Connection) -> std::result::Result<T, RpcError>,
    ) -> std::result::Result<T, RpcError> {
        read(&self.store.conn())
    }

    /// The caller holds any required model fence. The expected run check and
    /// every domain/task/message write occur inside this one transaction.
    /// Publishing is the caller's responsibility after a successful return.
    pub fn transaction<T>(
        &self,
        expected_run: Option<&Id>,
        write: impl FnOnce(&Transaction<'_>) -> std::result::Result<T, RpcError>,
    ) -> std::result::Result<T, RpcError> {
        let mut conn = self.store.conn();
        transact(&mut conn, expected_run, write)
    }

    /// Receipt admissions/removals commit with synchronous=FULL before the
    /// caller forks paid work or acknowledges the mutation. The previous
    /// setting is restored under the same writer mutex, including on unwind.
    pub fn durable_transaction<T>(
        &self,
        expected_run: Option<&Id>,
        write: impl FnOnce(&Transaction<'_>) -> std::result::Result<T, RpcError>,
    ) -> std::result::Result<T, RpcError> {
        let conn = self.store.conn();
        let previous = conn
            .pragma_query_value(None, "synchronous", |row| row.get::<_, i64>(0))
            .map_err(storage_error)?;
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(storage_error)?;
        let mut durable = DurableConnection { conn, previous };
        transact(&mut durable.conn, expected_run, write)
    }

    pub fn current_run(&self) -> std::result::Result<RunRecord, RpcError> {
        self.read(|conn| {
            let id = current_id(conn)
                .map_err(storage_error)?
                .ok_or_else(|| RpcError::new("stale_scope", "no workspace run"))?;
            if marker(conn).map_err(storage_error)?.as_deref() != Some(id.as_str()) {
                return Err(RpcError::new("stale_scope", "workspace run changed"));
            }
            let (at, archived): (i64, Option<i64>) = conn
                .query_row(
                    "SELECT started_at, archived_at FROM feature_runs WHERE id = ?1",
                    [&id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(storage_error)?;
            if archived.is_some() {
                return Err(RpcError::new("stale_scope", "workspace run archived"));
            }
            Ok(RunRecord {
                id: Id::new(id).map_err(|_| invalid_storage())?,
                started_at: Timestamp::new(at).map_err(|_| invalid_storage())?,
                archived_at: None,
            })
        })
    }

    /// Archive/reset legacy state and active domain rows in one run-fenced
    /// transaction. Historical domain records remain the callback's choice.
    pub fn clear_session(
        &self,
        expected_run: &Id,
        clear_domain: impl FnOnce(&Transaction<'_>) -> std::result::Result<(), RpcError>,
    ) -> std::result::Result<(), RpcError> {
        self.transaction(Some(expected_run), |tx| {
            clear_domain(tx)?;
            clear_session_in(tx).map_err(|_| RpcError::internal("session clear failed"))
        })
    }

    pub fn incarnation(&self, run: &Id, agent: &str) -> std::result::Result<Option<Id>, RpcError> {
        self.read(|conn| {
            let value: Option<String> = conn
                .query_row(
                    "SELECT incarnation FROM feature_members
                 WHERE run_id = ?1 AND agent_id = ?2 AND retired_at IS NULL",
                    params![run.as_str(), agent],
                    |row| row.get(0),
                )
                .optional()
                .map_err(storage_error)?;
            value
                .map(Id::new)
                .transpose()
                .map_err(|_| invalid_storage())
        })
    }

    /// Historical metrics may name archived runs, but never a run from another
    /// workspace's database. Current membership is not query attribution.
    pub fn has_run(&self, run: &Id) -> std::result::Result<bool, RpcError> {
        self.read(|conn| {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM feature_runs WHERE id=?1)",
                [run.as_str()],
                |row| row.get(0),
            )
            .map_err(storage_error)
        })
    }
}

struct DurableConnection<'a> {
    conn: MutexGuard<'a, Connection>,
    previous: i64,
}

impl Drop for DurableConnection<'_> {
    fn drop(&mut self) {
        // Restore failure leaves FULL in place, which preserves durability.
        // A successful committed receipt must not be reported as rolled back.
        let _ = self.conn.pragma_update(None, "synchronous", self.previous);
    }
}

fn transact<T>(
    conn: &mut Connection,
    expected_run: Option<&Id>,
    write: impl FnOnce(&Transaction<'_>) -> std::result::Result<T, RpcError>,
) -> std::result::Result<T, RpcError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_error)?;
    if let Some(expected) = expected_run {
        let actual = current_id(&tx).map_err(storage_error)?;
        let marker = marker(&tx).map_err(storage_error)?;
        if actual.as_deref() != Some(expected.as_str()) || marker != actual {
            return Err(RpcError::new("stale_scope", "workspace run changed"));
        }
    }
    let result = write(&tx)?;
    tx.commit().map_err(storage_error)?;
    Ok(result)
}

pub fn storage_error(_: rusqlite::Error) -> RpcError {
    RpcError::new("internal", "feature storage failed")
}

fn invalid_storage() -> RpcError {
    RpcError::new("internal", "invalid feature storage state")
}

fn current_id(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT run_id FROM feature_current_run WHERE slot = 1",
        [],
        |row| row.get(0),
    )
    .optional()
}

fn marker(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [MARKER], |row| {
        row.get(0)
    })
    .optional()
}

pub(super) fn ensure_current(conn: &Connection, at: i64) -> Result<RunRecord> {
    if let Some(id) = current_id(conn)? {
        if marker(conn)?.as_deref() == Some(id.as_str()) {
            let (started, archived): (i64, Option<i64>) = conn.query_row(
                "SELECT started_at, archived_at FROM feature_runs WHERE id = ?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            anyhow::ensure!(archived.is_none(), "current feature run is archived");
            return Ok(RunRecord {
                id: Id::new(id).map_err(anyhow::Error::msg)?,
                started_at: Timestamp::new(started).map_err(anyhow::Error::msg)?,
                archived_at: None,
            });
        }
        conn.execute(
            "UPDATE feature_runs SET archived_at = COALESCE(archived_at, ?1) WHERE id = ?2",
            params![at, id],
        )?;
        conn.execute(
            "UPDATE feature_members SET retired_at = COALESCE(retired_at, ?1) WHERE run_id = ?2",
            params![at, id],
        )?;
    }
    let id = Id::new(uuid::Uuid::new_v4().to_string()).map_err(anyhow::Error::msg)?;
    conn.execute(
        "INSERT INTO feature_runs(id, started_at) VALUES (?1, ?2)",
        params![id.as_str(), at],
    )?;
    conn.execute("INSERT INTO feature_current_run(slot, run_id) VALUES (1, ?1) ON CONFLICT(slot) DO UPDATE SET run_id = excluded.run_id", [id.as_str()])?;
    conn.execute("INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![MARKER, id.as_str()])?;
    Ok(RunRecord {
        id,
        started_at: Timestamp::new(at).map_err(anyhow::Error::msg)?,
        archived_at: None,
    })
}

pub(super) fn ensure_member(conn: &Connection, run: &Id, agent: &str, at: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO feature_members(run_id, agent_id, incarnation, created_at)
         SELECT ?1, ?2, ?3, ?4 WHERE NOT EXISTS
         (SELECT 1 FROM feature_members WHERE run_id = ?1 AND agent_id = ?2 AND retired_at IS NULL)",
        params![run.as_str(), agent, uuid::Uuid::new_v4().to_string(), at],
    )?;
    Ok(())
}

pub(super) fn initialize(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)
        .context("creating feature identity schema")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let run = ensure_current(&tx, now_ms())?;
    let agents = {
        let mut stmt = tx.prepare("SELECT id FROM agents")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for agent in agents {
        ensure_member(&tx, &run.id, &agent, now_ms())?;
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn clear_session(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    clear_session_in(&tx)?;
    tx.commit()?;
    Ok(())
}

fn clear_session_in(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("DELETE FROM agents; DELETE FROM tokens; DELETE FROM tasks; DELETE FROM messages; DELETE FROM deliveries; DELETE FROM meta;")?;
    ensure_current(tx, now_ms())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::{AgentStore, SessionStore};
    use crew_core::Agent;

    fn agent() -> Agent {
        serde_json::from_value(serde_json::json!({
            "id":"worker", "name":"worker", "role":"work", "runtime":"shell",
            "status":"waiting", "workdir":"/tmp", "backend":"pty", "last_activity_at":0
        }))
        .unwrap()
    }

    #[test]
    fn process_and_store_restarts_preserve_run_and_member_incarnation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("crew.db");
        let store = Arc::new(SqliteStore::open(&path).unwrap());
        let db = FeatureDb::new(store.clone());
        store.save_agent(&agent()).unwrap();
        let run = db.current_run().unwrap();
        let incarnation = db.incarnation(&run.id, "worker").unwrap().unwrap();
        store.save_agent(&agent()).unwrap();
        assert_eq!(
            db.incarnation(&run.id, "worker").unwrap(),
            Some(incarnation.clone())
        );
        drop(db);
        drop(store);
        let db = FeatureDb::new(Arc::new(SqliteStore::open(&path).unwrap()));
        assert_eq!(db.current_run().unwrap(), run);
        assert_eq!(
            db.incarnation(&run.id, "worker").unwrap(),
            Some(incarnation)
        );
    }

    #[test]
    fn removing_and_reusing_an_id_allocates_a_new_incarnation() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let db = FeatureDb::new(store.clone());
        store.save_agent(&agent()).unwrap();
        let run = db.current_run().unwrap();
        let old = db.incarnation(&run.id, "worker").unwrap().unwrap();
        store.delete_agent("worker").unwrap();
        assert_eq!(db.incarnation(&run.id, "worker").unwrap(), None);
        store.save_agent(&agent()).unwrap();
        let new = db.incarnation(&run.id, "worker").unwrap().unwrap();
        assert_ne!(old, new);
        assert_eq!(db.current_run().unwrap(), run);
        assert_eq!(
            db.read(|conn| conn
                .query_row("SELECT COUNT(*) FROM feature_members", [], |row| row
                    .get::<_, i64>(0))
                .map_err(storage_error))
                .unwrap(),
            2
        );
    }

    #[test]
    fn session_clear_archives_the_run_and_rejects_stale_writes_before_callbacks() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let db = FeatureDb::new(store.clone());
        let old = db.current_run().unwrap();
        store.clear_session().unwrap();
        let new = db.current_run().unwrap();
        assert_ne!(old.id, new.id);
        let mut called = false;
        let error = db
            .transaction(Some(&old.id), |_| {
                called = true;
                Ok(())
            })
            .unwrap_err();
        assert_eq!(error.code, "stale_scope");
        assert!(!called);
        assert!(db
            .read(|conn| conn
                .query_row(
                    "SELECT archived_at IS NOT NULL FROM feature_runs WHERE id = ?1",
                    [old.id.as_str()],
                    |row| row.get::<_, bool>(0)
                )
                .map_err(storage_error))
            .unwrap());
    }

    #[test]
    fn an_old_binary_clear_is_detected_on_reopen_without_joining_historical_members() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("crew.db");
        let store = Arc::new(SqliteStore::open(&path).unwrap());
        let db = FeatureDb::new(store.clone());
        store.save_agent(&agent()).unwrap();
        let old = db.current_run().unwrap();
        let incarnation = db.incarnation(&old.id, "worker").unwrap().unwrap();
        drop(db);
        drop(store);
        // The historical clear knows only the legacy tables, not feature_runs.
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "DELETE FROM agents; DELETE FROM tokens; DELETE FROM tasks;
             DELETE FROM messages; DELETE FROM deliveries; DELETE FROM meta;",
            )
            .unwrap();
        let store = Arc::new(SqliteStore::open(&path).unwrap());
        let db = FeatureDb::new(store.clone());
        let new = db.current_run().unwrap();
        assert_ne!(old.id, new.id);
        store.save_agent(&agent()).unwrap();
        assert_ne!(
            db.incarnation(&new.id, "worker").unwrap().unwrap(),
            incarnation
        );
        assert_eq!(db.incarnation(&old.id, "worker").unwrap(), None);
    }

    #[test]
    fn domain_callback_error_rolls_back_all_writes() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let db = FeatureDb::new(store);
        let run = db.current_run().unwrap();
        db.transaction(None, |tx| {
            tx.execute_batch("CREATE TABLE domain_fixture (value INTEGER NOT NULL);")
                .map_err(storage_error)
        })
        .unwrap();
        let result: std::result::Result<(), RpcError> = db.transaction(Some(&run.id), |tx| {
            tx.execute("INSERT INTO domain_fixture VALUES (1)", [])
                .map_err(storage_error)?;
            Err(RpcError::conflict("fixture rollback"))
        });
        assert_eq!(result.unwrap_err().code, "conflict");
        assert_eq!(
            db.read(|conn| conn
                .query_row("SELECT COUNT(*) FROM domain_fixture", [], |row| row
                    .get::<_, i64>(0))
                .map_err(storage_error))
                .unwrap(),
            0
        );
    }

    #[test]
    fn durable_receipts_restore_sync_setting_after_commit_error_and_unwind() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let db = FeatureDb::new(store);
        let run = db.current_run().unwrap();
        let setting = || {
            db.read(|conn| {
                conn.pragma_query_value(None, "synchronous", |row| row.get::<_, i64>(0))
                    .map_err(storage_error)
            })
            .unwrap()
        };
        let before = setting();
        db.durable_transaction(Some(&run.id), |tx| {
            assert_eq!(
                tx.pragma_query_value(None, "synchronous", |row| row.get::<_, i64>(0))
                    .map_err(storage_error)?,
                2
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(setting(), before);
        let failure: Result<(), RpcError> =
            db.durable_transaction(Some(&run.id), |_| Err(RpcError::conflict("rollback")));
        assert_eq!(failure.unwrap_err().code, "conflict");
        assert_eq!(setting(), before);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), RpcError> =
                db.durable_transaction(Some(&run.id), |_| panic!("fixture unwind"));
        }));
        assert!(panic.is_err());
        assert_eq!(setting(), before);
    }
}
