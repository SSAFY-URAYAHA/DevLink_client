//! Private TASK_CREATED journal. A successful record is the only source of a positive ACK.
//! No task transition, replay, automatic acceptance, or execution occurs here.

use crate::{
    config, credentials,
    protocol::{TaskAckPayload, TaskCreatedEvent},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Semaphore;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InboxFailure {
    StoreFailed,
    QueueFull,
    InvalidEvent,
}

/// Content-free, process-lifetime diagnostics; failures stay visible until broker restart.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboxDiagnostics {
    pub rejected_events: u64,
    pub last_error: Option<InboxFailure>,
}

pub const MAX_EVENTS: usize = 1000;
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Snapshot {
    schema_version: u32,
    events: Vec<TaskCreatedEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{TaskCreatedKind, TaskCreatedPayload};

    #[tokio::test]
    async fn batch_commits_good_entries_without_acknowledging_conflicting_entries() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = Inbox::for_config(&directory.path().join("config.json"));
        let first = event("event-1", 1);
        let mut conflict = first.clone();
        conflict.payload.prompt = "conflicting input".into();
        let second = event("event-2", 2);
        let acknowledgements = inbox
            .record_batch(vec![first.clone(), conflict, second.clone()], 22)
            .await
            .unwrap();
        assert_eq!(
            acknowledgements,
            vec![first.acknowledgement(), second.acknowledgement()]
        );
        assert_eq!(inbox.list(20).await.unwrap().len(), 2);
        assert_eq!(inbox.diagnostics().rejected_events, 1);
    }

    #[tokio::test]
    async fn global_writer_limit_and_shutdown_withhold_ack_without_rewriting_data() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = Inbox::for_config(&directory.path().join("config.json"));
        inbox.record(event("event-1", 1), 22).await.unwrap();
        let original = fs::read(inbox.path()).unwrap();
        let permits = inbox.permits.clone().acquire_many_owned(16).await.unwrap();
        assert!(inbox.record(event("event-2", 2), 22).await.is_err());
        drop(permits);
        inbox.stop_accepting();
        assert!(inbox.record(event("event-2", 2), 22).await.is_err());
        assert_eq!(fs::read(inbox.path()).unwrap(), original);
        assert_eq!(inbox.diagnostics().rejected_events, 2);
    }

    fn event(id: &str, task_id: u64) -> TaskCreatedEvent {
        TaskCreatedEvent {
            kind: TaskCreatedKind::TaskCreated,
            event_id: id.into(),
            payload: TaskCreatedPayload {
                task_id,
                team_id: 3,
                requester_agent_id: 11,
                receiver_agent_id: 22,
                prompt: "사용자 수락 후에만 수정하세요\n\u{1b}[31m".into(),
                created_at: "2026-10-29T12:00:00".into(),
            },
        }
    }

    #[tokio::test]
    async fn persisted_events_survive_restart_and_duplicate_conflicts_withhold_ack() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.json");
        let inbox = Inbox::for_config(&config);
        let first = event("event-1", 100);
        assert_eq!(
            inbox.record(first.clone(), 22).await.unwrap(),
            first.acknowledgement()
        );
        let restarted = Inbox::for_config(&config);
        restarted.record(first.clone(), 22).await.unwrap();
        assert_eq!(restarted.load().unwrap().events.len(), 1);
        let mut conflicting = first.clone();
        conflicting.payload.prompt = "Changed input".into();
        assert!(restarted.record(conflicting, 22).await.is_err());
        assert!(restarted.record(first.clone(), 23).await.is_err());
        let mut new_event = first.clone();
        new_event.event_id = "event-2".into();
        restarted.record(new_event, 22).await.unwrap();
        assert_eq!(restarted.load().unwrap().events.len(), 2);
        assert_eq!(restarted.list(20).await.unwrap().len(), 1);
        assert_eq!(restarted.list(20).await.unwrap()[0].payload, first.payload);
        credentials::verify_private_permissions(&restarted.path()).unwrap();
        let abandoned = restarted.directory.join(".inbox-crash-fixture");
        fs::write(&abandoned, b"private interrupted snapshot").unwrap();
        credentials::set_private_permissions(&abandoned).unwrap();
        restarted.clear().await.unwrap();
        assert!(!abandoned.exists());
        assert!(restarted.list(20).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn capacity_does_not_evict_retained_events_or_ack_new_work() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = Inbox::for_config(&directory.path().join("config.json"));
        inbox.prepare().unwrap();
        let events = (1..=MAX_EVENTS)
            .map(|id| event(&format!("event-{id}"), id as u64))
            .collect();
        inbox
            .persist(&Snapshot {
                schema_version: 1,
                events,
            })
            .unwrap();
        assert!(inbox.record(event("event-extra", 1001), 22).await.is_err());
        assert_eq!(inbox.load().unwrap().events.len(), MAX_EVENTS);
        inbox.record(event("event-1", 1), 22).await.unwrap();
        assert_eq!(inbox.list(20).await.unwrap().len(), 20);
    }

    #[tokio::test]
    async fn corrupt_oversized_and_non_file_snapshots_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = Inbox::for_config(&directory.path().join("config.json"));
        inbox.record(event("event-1", 100), 22).await.unwrap();
        fs::write(inbox.path(), b"not-json").unwrap();
        assert!(inbox.record(event("event-2", 101), 22).await.is_err());
        assert!(inbox.list(20).await.is_err());
        assert!(inbox.clear().await.is_err());
        OpenOptions::new()
            .write(true)
            .open(inbox.path())
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert!(inbox.record(event("event-2", 101), 22).await.is_err());
        fs::remove_file(inbox.path()).unwrap();
        fs::create_dir(inbox.path()).unwrap();
        assert!(inbox.record(event("event-2", 101), 22).await.is_err());
    }

    #[test]
    fn invalid_wire_and_cross_receiver_or_overflow_ids_are_rejected() {
        let valid = event("event-1", 100);
        let json = serde_json::to_value(&valid).unwrap();
        assert_eq!(json["event_id"], "event-1");
        assert_eq!(json["type"], "TASK_CREATED");
        assert!(json["payload"].get("event_id").is_none());
        assert_eq!(
            serde_json::to_value(crate::protocol::ClientMessage::Ack(valid.acknowledgement()))
                .unwrap(),
            serde_json::json!({"type":"ACK","payload":{"event_id":"event-1","task_id":100}})
        );
        for bad in [0, i64::MAX as u64 + 1] {
            let mut invalid = valid.clone();
            invalid.payload.task_id = bad;
            assert!(!invalid.is_valid_for(22));
        }
        assert!(!valid.is_valid_for(23));
        let mut invalid = valid.clone();
        invalid.event_id = "\n".into();
        assert!(!invalid.is_valid_for(22));
        let mut invalid_json = json;
        invalid_json["payload"]["task_id"] = serde_json::json!("100");
        assert!(serde_json::from_value::<TaskCreatedEvent>(invalid_json).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_and_public_snapshots_are_rejected_without_repairing_them() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = tempfile::tempdir().unwrap();
        let inbox = Inbox::for_config(&directory.path().join("config.json"));
        inbox.record(event("event-1", 100), 22).await.unwrap();
        fs::set_permissions(inbox.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(inbox.record(event("event-2", 101), 22).await.is_err());
        assert_eq!(
            fs::metadata(inbox.path()).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fs::remove_file(inbox.path()).unwrap();
        let target = directory.path().join("outside.json");
        fs::write(&target, b"outside").unwrap();
        symlink(&target, inbox.path()).unwrap();
        assert!(inbox.record(event("event-2", 101), 22).await.is_err());
        assert_eq!(fs::read(target).unwrap(), b"outside");
    }
}

#[derive(Clone)]
pub struct Inbox {
    directory: PathBuf,
    gate: Arc<Mutex<()>>,
    diagnostics: Arc<Mutex<InboxDiagnostics>>,
    permits: Arc<Semaphore>,
    stopping: Arc<AtomicBool>,
}

impl Inbox {
    pub fn for_config(config_path: &Path) -> Self {
        Self {
            directory: config::state_directory(config_path).join("inbox"),
            gate: Arc::new(Mutex::new(())),
            diagnostics: Arc::new(Mutex::new(InboxDiagnostics::default())),
            permits: Arc::new(Semaphore::new(16)),
            stopping: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn diagnostics(&self) -> InboxDiagnostics {
        self.diagnostics
            .lock()
            .map(|value| *value)
            .unwrap_or_default()
    }

    pub fn rejected(&self, error: InboxFailure) {
        if let Ok(mut value) = self.diagnostics.lock() {
            value.rejected_events = value.rejected_events.saturating_add(1);
            value.last_error = Some(error);
        }
    }

    pub fn stop_accepting(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }

    fn path(&self) -> PathBuf {
        self.directory.join("tasks.json")
    }

    fn inspect_directories(&self) -> Result<bool> {
        let parent = self
            .directory
            .parent()
            .context("Inbox directory has no parent")?;
        match fs::symlink_metadata(parent) {
            Ok(_) => credentials::verify_parent_safety(parent)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => anyhow::bail!("Cannot inspect inbox state directory"),
        }
        match fs::symlink_metadata(&self.directory) {
            Ok(_) => credentials::verify_parent_safety(&self.directory)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => anyhow::bail!("Cannot inspect inbox directory"),
        }
        Ok(true)
    }

    fn prepare(&self) -> Result<()> {
        // Inspect before mutation; never follow a state/inbox symlink or junction.
        let already_exists = self.inspect_directories()?;
        let parent = self
            .directory
            .parent()
            .context("Inbox directory has no parent")?;
        credentials::ensure_safe_parent_directory(parent)?;
        credentials::ensure_safe_parent_directory(&self.directory)?;
        if !already_exists {
            credentials::set_private_permissions(&self.directory)?;
        }
        Ok(())
    }

    fn load(&self) -> Result<Snapshot> {
        let empty = || Snapshot {
            schema_version: 1,
            events: vec![],
        };
        if !self.inspect_directories()? || !credentials::ensure_safe_target_if_exists(&self.path())?
        {
            return Ok(empty());
        }
        credentials::verify_private_permissions(&self.path())?;
        let mut bytes = Vec::new();
        File::open(self.path())
            .context("Cannot open inbox snapshot")?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("Cannot read inbox snapshot")?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "Inbox snapshot exceeds size limit"
        );
        let snapshot: Snapshot = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("Invalid inbox snapshot; no events acknowledged"))?;
        ensure!(
            snapshot.schema_version == 1 && snapshot.events.len() <= MAX_EVENTS,
            "Unsupported inbox snapshot or capacity"
        );
        let mut ids = HashSet::new();
        let mut tasks = HashMap::new();
        let agent = snapshot
            .events
            .first()
            .map(|event| event.payload.receiver_agent_id);
        for event in &snapshot.events {
            ensure!(
                Some(event.payload.receiver_agent_id) == agent
                    && event.is_valid_for(event.payload.receiver_agent_id),
                "Invalid inbox event identity or payload"
            );
            ensure!(ids.insert(&event.event_id), "Duplicate inbox journal event");
            if let Some(previous) = tasks.insert(event.payload.task_id, &event.payload) {
                ensure!(previous == &event.payload, "Conflicting inbox task payload");
            }
        }
        Ok(snapshot)
    }

    fn persist(&self, snapshot: &Snapshot) -> Result<()> {
        let bytes = serde_json::to_vec(snapshot).context("Cannot serialize inbox snapshot")?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "Inbox capacity exceeded; stop broker and archive or clear inbox"
        );
        let mut temporary = tempfile::Builder::new()
            .prefix(".inbox-")
            .tempfile_in(&self.directory)
            .context("Cannot create inbox snapshot")?;
        credentials::set_private_permissions(temporary.path())?;
        credentials::verify_private_permissions(temporary.path())?;
        temporary
            .write_all(&bytes)
            .context("Cannot write inbox snapshot")?;
        temporary
            .as_file()
            .sync_all()
            .context("Cannot flush inbox snapshot")?;
        temporary
            .persist(self.path())
            .map_err(|_| anyhow::anyhow!("Cannot publish inbox snapshot"))?;
        // A post-publication failure must not result in an ACK. A later identical event is safe.
        credentials::verify_private_permissions(&self.path())?;
        self.flush_published()
    }

    fn flush_published(&self) -> Result<()> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.path())?
            .sync_all()
            .context("Cannot flush published inbox")?;
        #[cfg(unix)]
        File::open(&self.directory)?
            .sync_all()
            .context("Cannot flush inbox directory")?;
        Ok(())
    }

    fn record_batch_sync(
        &self,
        events: Vec<TaskCreatedEvent>,
        authenticated_agent: u64,
    ) -> Result<Vec<TaskAckPayload>> {
        ensure!(
            events
                .iter()
                .all(|event| event.is_valid_for(authenticated_agent)),
            "Invalid TASK_CREATED or receiver mismatch"
        );
        let _gate = self
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("Inbox writer unavailable"))?;
        ensure!(
            !self.stopping.load(Ordering::SeqCst),
            "Inbox is stopping; ACK withheld"
        );
        self.prepare()?;
        let _lock = config::lock(&self.directory.join("inbox.lock"))?;
        ensure!(
            !self.stopping.load(Ordering::SeqCst),
            "Inbox is stopping; ACK withheld"
        );
        let mut snapshot = self.load()?;
        let mut acknowledgements = Vec::new();
        let mut changed = false;
        let mut bytes = serde_json::to_vec(&snapshot)?.len() as u64;
        for event in events {
            match Self::append(&mut snapshot, &event, authenticated_agent, &mut bytes) {
                Ok(added) => {
                    changed |= added;
                    acknowledgements.push(event.acknowledgement());
                }
                Err(_) => self.rejected(InboxFailure::StoreFailed),
            }
        }
        if changed {
            self.persist(&snapshot)?;
        } else if !acknowledgements.is_empty() {
            self.flush_published()?;
        }
        Ok(acknowledgements)
    }

    fn append(
        snapshot: &mut Snapshot,
        event: &TaskCreatedEvent,
        agent: u64,
        bytes: &mut u64,
    ) -> Result<bool> {
        if let Some(previous) = snapshot
            .events
            .iter()
            .find(|old| old.event_id == event.event_id)
        {
            ensure!(
                previous == event,
                "Conflicting duplicate event; ACK withheld"
            );
            return Ok(false);
        }
        for old in &snapshot.events {
            ensure!(
                old.payload.receiver_agent_id == agent,
                "Inbox belongs to another Agent"
            );
            if old.payload.task_id == event.payload.task_id {
                ensure!(
                    old.payload == event.payload,
                    "Conflicting creation payload; ACK withheld"
                );
            }
        }
        ensure!(
            snapshot.events.len() < MAX_EVENTS,
            "Inbox event capacity exceeded"
        );
        let extra =
            serde_json::to_vec(event)?.len() as u64 + u64::from(!snapshot.events.is_empty());
        ensure!(*bytes + extra <= MAX_BYTES, "Inbox byte capacity exceeded");
        *bytes += extra;
        snapshot.events.push(event.clone());
        Ok(true)
    }

    pub async fn record(
        &self,
        event: TaskCreatedEvent,
        authenticated_agent: u64,
    ) -> Result<TaskAckPayload> {
        self.record_batch(vec![event], authenticated_agent)
            .await?
            .pop()
            .context("Inbox event rejected; ACK withheld")
    }

    /// Coalesced socket records share one atomic flush without acknowledging rejected entries.
    pub async fn record_batch(
        &self,
        events: Vec<TaskCreatedEvent>,
        authenticated_agent: u64,
    ) -> Result<Vec<TaskAckPayload>> {
        ensure!(
            !events.is_empty() && events.len() <= 16,
            "Invalid inbox batch size"
        );
        let count = events.len();
        let permit = match self.permits.clone().try_acquire_many_owned(count as u32) {
            Ok(permit) => permit,
            Err(_) => {
                for _ in 0..count {
                    self.rejected(InboxFailure::QueueFull);
                }
                anyhow::bail!("Inbox writer queue is full");
            }
        };
        let inbox = self.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit; // retained even if the connection drops its awaiting future
            let result = inbox.record_batch_sync(events, authenticated_agent);
            if result.is_err() {
                for _ in 0..count {
                    inbox.rejected(InboxFailure::StoreFailed);
                }
            }
            result
        })
        .await;
        match result {
            Ok(result) => result,
            Err(_) => {
                self.rejected(InboxFailure::StoreFailed);
                anyhow::bail!("Inbox writer failed");
            }
        }
    }

    /// Read-only, newest received first, one item per task even if multiple event IDs were recorded.
    pub async fn list(&self, limit: usize) -> Result<Vec<TaskCreatedEvent>> {
        ensure!(
            limit > 0 && limit <= MAX_EVENTS,
            "Inbox limit must be between 1 and 1000"
        );
        let inbox = self.clone();
        tokio::task::spawn_blocking(move || {
            let snapshot = inbox.load()?;
            let mut tasks = HashSet::new();
            Ok(snapshot
                .events
                .into_iter()
                .rev()
                .filter(|event| tasks.insert(event.payload.task_id))
                .take(limit)
                .collect())
        })
        .await
        .context("Inbox reader failed")?
    }

    /// Ends the retention window and dedup history. CLI requires the broker to be stopped.
    pub async fn clear(&self) -> Result<()> {
        let inbox = self.clone();
        tokio::task::spawn_blocking(move || {
            let _gate = inbox
                .gate
                .lock()
                .map_err(|_| anyhow::anyhow!("Inbox writer unavailable"))?;
            if !inbox.inspect_directories()? {
                return Ok(());
            }
            let _lock = config::lock(&inbox.directory.join("inbox.lock"))?;
            // Check existing data/permissions before deleting. Corruption is not silently discarded.
            inbox.load()?;
            if credentials::ensure_safe_target_if_exists(&inbox.path())? {
                fs::remove_file(inbox.path()).context("Cannot clear inbox")?;
            }
            // Reserved private temporary snapshots may remain after an abrupt process exit.
            for entry in
                fs::read_dir(&inbox.directory).context("Cannot inspect inbox temporary files")?
            {
                let entry = entry.context("Cannot inspect inbox temporary file")?;
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(".inbox-"))
                {
                    credentials::ensure_safe_target_if_exists(&entry.path())?;
                    credentials::verify_private_permissions(&entry.path())?;
                    fs::remove_file(entry.path())
                        .context("Cannot clear private inbox temporary file")?;
                }
            }
            #[cfg(unix)]
            File::open(&inbox.directory)?.sync_all()?;
            Ok(())
        })
        .await
        .context("Inbox cleanup failed")?
    }
}
