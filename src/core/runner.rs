use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Runner {
    pub id: String,
    #[serde(rename = "type")]
    pub runner_type: String,
    pub tmux_session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgid: Option<i32>,
    pub status: String,
    pub current_task: Option<String>,
    pub lease_expires_at: Option<String>,
    pub last_heartbeat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_since: Option<String>,
    pub registered_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerRegistry {
    pub runners: BTreeMap<String, Runner>,
}

#[derive(Debug, Error)]
pub enum RunnerStoreError {
    #[error("runner not found")]
    NotFound,

    #[error("failed to write or read {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to serialize runners")]
    Serialize(#[source] serde_json::Error),

    #[error("failed to deserialize runners")]
    Deserialize(#[source] serde_json::Error),
}

pub struct RunnerStore {
    runners_path: PathBuf,
    registry: RunnerRegistry,
}

impl RunnerStore {
    pub fn new(state_dir: impl AsRef<Path>) -> Result<Self, RunnerStoreError> {
        let runner_dir = state_dir.as_ref().join("runner");
        fs::create_dir_all(&runner_dir).map_err(|source| RunnerStoreError::Io {
            path: runner_dir.clone(),
            source,
        })?;

        let runners_path = runner_dir.join("runners.json");
        let registry = Self::load_from_file(&runners_path)?;

        Ok(Self {
            runners_path,
            registry,
        })
    }

    fn load_from_file(path: &Path) -> Result<RunnerRegistry, RunnerStoreError> {
        match fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).map_err(RunnerStoreError::Deserialize),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                Ok(RunnerRegistry::default())
            }
            Err(source) => Err(RunnerStoreError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    pub fn save(&mut self, registry: RunnerRegistry) -> Result<(), RunnerStoreError> {
        let bytes = serde_json::to_vec_pretty(&registry).map_err(RunnerStoreError::Serialize)?;
        let temporary_path = self.runners_path.with_extension("json.tmp");

        fs::write(&temporary_path, bytes).map_err(|source| RunnerStoreError::Io {
            path: self.runners_path.clone(),
            source,
        })?;
        fs::rename(&temporary_path, &self.runners_path).map_err(|source| RunnerStoreError::Io {
            path: self.runners_path.clone(),
            source,
        })?;

        self.registry = registry;
        Ok(())
    }

    pub fn registry(&self) -> &RunnerRegistry {
        &self.registry
    }

    pub fn get_runner(&self, id: &str) -> Option<&Runner> {
        self.registry.runners.get(id)
    }

    pub fn register(
        &mut self,
        id: &str,
        runner_type: &str,
        tmux_session: &str,
    ) -> Result<Runner, RunnerStoreError> {
        self.register_at(id, runner_type, tmux_session, Utc::now())
    }

    pub fn register_at(
        &mut self,
        id: &str,
        runner_type: &str,
        tmux_session: &str,
        now: DateTime<Utc>,
    ) -> Result<Runner, RunnerStoreError> {
        let runner = Runner {
            id: id.to_owned(),
            runner_type: runner_type.to_owned(),
            tmux_session: tmux_session.to_owned(),
            pgid: None,
            status: "running".to_owned(),
            current_task: None,
            lease_expires_at: None,
            last_heartbeat: None,
            idle_since: None,
            registered_at: now.to_rfc3339(),
        };

        let mut registry = self.registry.clone();
        registry.runners.insert(id.to_owned(), runner.clone());
        self.save(registry)?;
        Ok(runner)
    }

    pub fn deregister(&mut self, id: &str) -> Result<Runner, RunnerStoreError> {
        let mut registry = self.registry.clone();
        let runner = registry
            .runners
            .remove(id)
            .ok_or(RunnerStoreError::NotFound)?;
        self.save(registry)?;
        Ok(runner)
    }

    pub fn acquire_lease(
        &mut self,
        runner_id: &str,
        task_id: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Result<Runner, RunnerStoreError> {
        let mut registry = self.registry.clone();
        let runner = registry
            .runners
            .get_mut(runner_id)
            .ok_or(RunnerStoreError::NotFound)?;

        runner.current_task = Some(task_id.to_owned());
        runner.lease_expires_at = Some((now + ttl).to_rfc3339());
        runner.last_heartbeat = Some(now.to_rfc3339());
        runner.idle_since = None;

        let runner = runner.clone();
        self.save(registry)?;
        Ok(runner)
    }

    pub fn renew_lease(
        &mut self,
        runner_id: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Result<Runner, RunnerStoreError> {
        let mut registry = self.registry.clone();
        let runner = registry
            .runners
            .get_mut(runner_id)
            .ok_or(RunnerStoreError::NotFound)?;

        runner.lease_expires_at = Some((now + ttl).to_rfc3339());
        runner.last_heartbeat = Some(now.to_rfc3339());
        runner.idle_since = None;

        let runner = runner.clone();
        self.save(registry)?;
        Ok(runner)
    }

    pub fn release_lease(&mut self, runner_id: &str) -> Result<Runner, RunnerStoreError> {
        let mut registry = self.registry.clone();
        let runner = registry
            .runners
            .get_mut(runner_id)
            .ok_or(RunnerStoreError::NotFound)?;

        runner.current_task = None;
        runner.lease_expires_at = None;
        runner.last_heartbeat = None;

        let runner = runner.clone();
        self.save(registry)?;
        Ok(runner)
    }

    pub fn record_process_group(
        &mut self,
        runner_id: &str,
        pgid: Option<i32>,
    ) -> Result<Runner, RunnerStoreError> {
        let mut registry = self.registry.clone();
        let runner = registry
            .runners
            .get_mut(runner_id)
            .ok_or(RunnerStoreError::NotFound)?;

        runner.pgid = pgid;

        let runner = runner.clone();
        self.save(registry)?;
        Ok(runner)
    }

    pub fn record_ready(
        &mut self,
        runner_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Runner>, RunnerStoreError> {
        self.record_activity(runner_id, now)
    }

    pub fn record_heartbeat(
        &mut self,
        runner_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Runner>, RunnerStoreError> {
        self.record_activity(runner_id, now)
    }

    pub fn record_idle(
        &mut self,
        runner_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Runner>, RunnerStoreError> {
        let mut registry = self.registry.clone();
        let Some(runner) = registry.runners.get_mut(runner_id) else {
            return Ok(None);
        };

        runner.idle_since = Some(now.to_rfc3339());

        let runner = runner.clone();
        self.save(registry)?;
        Ok(Some(runner))
    }

    fn record_activity(
        &mut self,
        runner_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Runner>, RunnerStoreError> {
        let mut registry = self.registry.clone();
        let Some(runner) = registry.runners.get_mut(runner_id) else {
            return Ok(None);
        };

        runner.last_heartbeat = Some(now.to_rfc3339());
        runner.idle_since = None;

        let runner = runner.clone();
        self.save(registry)?;
        Ok(Some(runner))
    }
}

/// Fail-safe: an unparseable timestamp is treated as live, so this predicate
/// returns false and does not yank a task from a possibly active runner.
pub fn is_lease_expired(lease_expires_at: Option<&str>, now: DateTime<Utc>) -> bool {
    match lease_expires_at {
        None => true,
        Some(ts) => match DateTime::parse_from_rfc3339(ts) {
            Err(_) => false,
            Ok(expires_at) => now >= expires_at.with_timezone(&Utc),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> (tempfile::TempDir, RunnerStore) {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let store = RunnerStore::new(dir.path()).expect("create runner store");
        (dir, store)
    }

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-11T12:00:00Z")
            .expect("parse fixed time")
            .with_timezone(&Utc)
    }

    #[test]
    fn missing_file_loads_empty_registry() {
        let (dir, store) = test_store();

        assert!(dir.path().join("runner").exists());
        assert!(store.registry().runners.is_empty());
    }

    #[test]
    fn atomic_round_trip_persists_and_reloads_runner_fields() {
        let (dir, mut store) = test_store();

        let runner = store
            .register("runner-a", "claude-tmux", "agira-test")
            .expect("register runner");

        let fresh_store = RunnerStore::new(dir.path()).expect("reload runner store");
        let reloaded = fresh_store
            .get_runner("runner-a")
            .expect("reloaded runner exists");

        assert_eq!(reloaded, &runner);
        assert!(dir.path().join("runner").join("runners.json").exists());
    }

    #[test]
    fn register_is_idempotent_and_refreshes_existing_runner() {
        let (_dir, mut store) = test_store();

        store
            .register("runner-a", "claude-tmux", "old-session")
            .expect("register runner");
        let refreshed = store
            .register("runner-a", "claude-tmux", "new-session")
            .expect("refresh runner");

        assert_eq!(store.registry().runners.len(), 1);
        assert_eq!(refreshed.status, "running");
        assert_eq!(refreshed.tmux_session, "new-session");
        assert!(refreshed.current_task.is_none());
        assert!(refreshed.lease_expires_at.is_none());
        assert!(refreshed.last_heartbeat.is_none());
        assert!(refreshed.idle_since.is_none());
    }

    #[test]
    fn deregister_removes_present_runner_and_persists() {
        let (dir, mut store) = test_store();

        store
            .register("runner-a", "claude-tmux", "agira-test")
            .expect("register runner");
        let removed = store.deregister("runner-a").expect("deregister runner");

        assert_eq!(removed.id, "runner-a");
        assert!(store.get_runner("runner-a").is_none());

        let fresh_store = RunnerStore::new(dir.path()).expect("reload runner store");
        assert!(fresh_store.get_runner("runner-a").is_none());
    }

    #[test]
    fn deregister_absent_runner_returns_not_found() {
        let (_dir, mut store) = test_store();

        let result = store.deregister("missing-runner");

        assert!(matches!(result, Err(RunnerStoreError::NotFound)));
    }

    #[test]
    fn acquire_sets_task_lease_and_heartbeat_with_fixed_clock() {
        let (_dir, mut store) = test_store();
        let now = fixed_now();

        store
            .register("runner-a", "claude-tmux", "agira-test")
            .expect("register runner");
        let runner = store
            .acquire_lease("runner-a", "task-118", Duration::minutes(5), now)
            .expect("acquire lease");

        assert_eq!(runner.current_task.as_deref(), Some("task-118"));
        assert_eq!(
            runner.last_heartbeat.as_deref(),
            Some("2026-06-11T12:00:00+00:00")
        );
        assert!(runner.idle_since.is_none());
        assert_eq!(
            runner.lease_expires_at.as_deref(),
            Some("2026-06-11T12:05:00+00:00")
        );
    }

    #[test]
    fn renew_extends_expiry_and_refreshes_heartbeat() {
        let (_dir, mut store) = test_store();
        let now = fixed_now();

        store
            .register("runner-a", "claude-tmux", "agira-test")
            .expect("register runner");
        store
            .acquire_lease("runner-a", "task-118", Duration::minutes(5), now)
            .expect("acquire lease");
        let renewed_at = now + Duration::minutes(3);
        let runner = store
            .renew_lease("runner-a", Duration::minutes(5), renewed_at)
            .expect("renew lease");

        assert_eq!(runner.current_task.as_deref(), Some("task-118"));
        assert_eq!(
            runner.last_heartbeat.as_deref(),
            Some("2026-06-11T12:03:00+00:00")
        );
        assert!(runner.idle_since.is_none());
        assert_eq!(
            runner.lease_expires_at.as_deref(),
            Some("2026-06-11T12:08:00+00:00")
        );
    }

    #[test]
    fn expiry_predicate_reports_boundary_with_fixed_clock() {
        let now = fixed_now();
        let expires_at = (now + Duration::minutes(5)).to_rfc3339();

        assert!(!is_lease_expired(
            Some(&expires_at),
            now + Duration::minutes(4)
        ));
        assert!(is_lease_expired(
            Some(&expires_at),
            now + Duration::minutes(5)
        ));
        assert!(is_lease_expired(
            Some(&expires_at),
            now + Duration::minutes(6)
        ));
        assert!(is_lease_expired(None, now));
    }

    #[test]
    fn unparseable_timestamp_is_treated_as_live_fail_safe() {
        assert!(!is_lease_expired(Some("not-a-timestamp"), fixed_now()));
    }

    #[test]
    fn release_clears_task_lease_and_heartbeat() {
        let (_dir, mut store) = test_store();
        let now = fixed_now();

        store
            .register("runner-a", "claude-tmux", "agira-test")
            .expect("register runner");
        store
            .acquire_lease("runner-a", "task-118", Duration::minutes(5), now)
            .expect("acquire lease");
        let runner = store.release_lease("runner-a").expect("release lease");

        assert!(runner.current_task.is_none());
        assert!(runner.lease_expires_at.is_none());
        assert!(runner.last_heartbeat.is_none());
    }

    #[test]
    fn runner_events_record_idle_ready_and_heartbeat() {
        let (_dir, mut store) = test_store();
        let now = fixed_now();

        store
            .register("runner-a", "claude-tmux", "agira-test")
            .expect("register runner");
        let idle = store
            .record_idle("runner-a", now)
            .expect("record idle")
            .expect("runner updated");

        assert_eq!(
            idle.idle_since.as_deref(),
            Some("2026-06-11T12:00:00+00:00")
        );

        let ready = store
            .record_ready("runner-a", now + Duration::seconds(1))
            .expect("record ready")
            .expect("runner updated");
        assert!(ready.idle_since.is_none());
        assert_eq!(
            ready.last_heartbeat.as_deref(),
            Some("2026-06-11T12:00:01+00:00")
        );

        store
            .record_idle("runner-a", now + Duration::seconds(2))
            .expect("record idle")
            .expect("runner updated");
        let heartbeat = store
            .record_heartbeat("runner-a", now + Duration::seconds(3))
            .expect("record heartbeat")
            .expect("runner updated");
        assert!(heartbeat.idle_since.is_none());
        assert_eq!(
            heartbeat.last_heartbeat.as_deref(),
            Some("2026-06-11T12:00:03+00:00")
        );
    }

    #[test]
    fn runner_event_for_unknown_runner_is_noop() {
        let (_dir, mut store) = test_store();

        assert!(
            store
                .record_ready("missing", fixed_now())
                .expect("ready no-op")
                .is_none()
        );
        assert!(
            store
                .record_idle("missing", fixed_now())
                .expect("idle no-op")
                .is_none()
        );
        assert!(
            store
                .record_heartbeat("missing", fixed_now())
                .expect("heartbeat no-op")
                .is_none()
        );
        assert!(store.registry().runners.is_empty());
    }

    #[test]
    fn two_distinct_runner_ids_coexist_without_collision() {
        let (_dir, mut store) = test_store();

        store
            .register("runner-a", "claude-tmux", "agira-a")
            .expect("register runner a");
        store
            .register("runner-b", "claude-tmux", "agira-b")
            .expect("register runner b");

        assert_eq!(store.registry().runners.len(), 2);
        assert_eq!(
            store
                .get_runner("runner-a")
                .expect("runner a exists")
                .tmux_session,
            "agira-a"
        );
        assert_eq!(
            store
                .get_runner("runner-b")
                .expect("runner b exists")
                .tmux_session,
            "agira-b"
        );
    }
}
