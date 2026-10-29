//! 수신 업무(Task) 영속 저장소 (Durable Inbox)
//!
//! - 중앙 서버로부터 수신한 `TASK_CREATED` 업무를 로컬 파일(`.devlink/inbox.json`)에 안전하게 영속 보관합니다.
//! - 디스크에 원자적(Atomic write)으로 저장이 완료된 경우에만 중앙 WebSocket으로 `ACK`를 회신합니다.
//! - 동일 `task_id`의 재전송 요청에 대해 중복 저장을 방지하고 멱등적(Idempotent)으로 ACK를 반환합니다.
//! - I/O 작업은 비동기 소켓 루프를 차단하지 않도록 별도의 블로킹 스레드풀에서 실행됩니다.

use crate::{
    central::endpoint_url,
    config, credentials,
    protocol::{TaskAckPayload, TaskCreatedMessage},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// 수신함 최대 허용 파일 크기 (64 MiB)
const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// 수신함 최대 보관 가능 업무 개수 (1024개)
const MAX_TASKS: usize = 1024;

/// 수신함에 영속 보관되는 단일 업무 데이터 구조
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredTask {
    /// 업무를 수신한 중앙 서버 URL 출처
    pub server_url: String,
    /// 수신한 업무 원문 메시지 (event_id, task_id, prompt 등)
    pub message: TaskCreatedMessage,
}

/// 디스크 스냅샷 직렬화용 루트 구조체
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema_version: u32,
    tasks: Vec<StoredTask>,
}

/// 인메모리 저장소 캐시 및 장애 상태 관리
struct Store {
    path: PathBuf,
    tasks: Vec<StoredTask>,
    failed: bool,
}

/// 수신함 접근 관리 핸들러 (Thread-safe, 동시 쓰기 세마포어 적용)
#[derive(Clone)]
pub struct Inbox(Arc<Mutex<Store>>, Arc<tokio::sync::Semaphore>);

impl Inbox {
    #[cfg(test)]
    pub(crate) fn has_pending_write(&self) -> bool {
        self.1.available_permits() == 0
    }
    /// 수신함 파일 경로 반환 (`.devlink/inbox.json`)
    pub fn path(config_path: &Path) -> PathBuf {
        config::state_directory(config_path).join("inbox.json")
    }

    /// 수신함 초기화 및 기존 파일 검증/로드
    pub fn open(config_path: &Path) -> Result<Self> {
        let path = Self::path(config_path);
        let parent = path.parent().context("Inbox has no parent")?;
        credentials::ensure_safe_parent_directory(parent)?;
        let exists = credentials::ensure_safe_target_if_exists(&path)?;
        if exists {
            credentials::verify_private_permissions(&path)?;
        }
        credentials::set_private_permissions(parent)?;
        let tasks = if exists {
            let mut bytes = Vec::new();
            File::open(&path)?
                .take(MAX_BYTES + 1)
                .read_to_end(&mut bytes)?;
            ensure!(bytes.len() as u64 <= MAX_BYTES, "Inbox exceeds size limit");
            let snapshot: Snapshot = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("Invalid inbox JSON"))?;
            ensure!(snapshot.schema_version == 1, "Unsupported inbox schema");
            ensure!(
                snapshot.tasks.len() <= MAX_TASKS,
                "Inbox exceeds task limit"
            );
            let mut seen = std::collections::HashSet::new();
            for task in &snapshot.tasks {
                ensure!(task.message.is_valid(), "Invalid stored task");
                ensure!(
                    endpoint_url(&task.server_url)?.as_str() == task.server_url,
                    "Invalid inbox origin"
                );
                ensure!(
                    seen.insert((
                        task.server_url.as_str(),
                        task.message.payload.receiver_agent_id,
                        task.message.payload.task_id
                    )),
                    "Duplicate stored task"
                );
            }
            snapshot.tasks
        } else {
            Vec::new()
        };
        Ok(Self(
            Arc::new(Mutex::new(Store {
                path,
                tasks,
                failed: false,
            })),
            Arc::new(tokio::sync::Semaphore::new(1)),
        ))
    }

    /// 업무를 영속 저장하고 ACK 페이로드를 생성 (비동기 블로킹 태스크 위임)
    pub async fn record(
        &self,
        server_url: String,
        agent_id: u64,
        message: TaskCreatedMessage,
    ) -> Result<TaskAckPayload> {
        let permit = self
            .1
            .clone()
            .acquire_owned()
            .await
            .context("Inbox worker closed")?;
        let inbox = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            inbox.record_sync(&server_url, agent_id, message)
        })
        .await
        .context("Inbox worker failed")?
    }

    /// 동기식 디스크 저장 및 중복 제거 검증 로직
    fn record_sync(
        &self,
        server_url: &str,
        agent_id: u64,
        message: TaskCreatedMessage,
    ) -> Result<TaskAckPayload> {
        ensure!(
            message.is_valid() && message.payload.receiver_agent_id == agent_id,
            "Invalid task recipient or payload"
        );
        let server_url = endpoint_url(server_url)?.to_string();
        let mut store = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Inbox lock failed"))?;
        ensure!(
            !store.failed,
            "Inbox requires restart after storage failure"
        );
        let parent = store.path.parent().context("Inbox has no parent")?;
        credentials::verify_parent_safety(parent)?;
        let exists = credentials::ensure_safe_target_if_exists(&store.path)?;
        ensure!(
            exists || store.tasks.is_empty(),
            "Inbox snapshot is missing"
        );
        // 이미 저장된 동일 task_id가 있는 경우: 중복 저장하지 않고 즉시 ACK 반환 (멱등성 보장)
        if let Some(existing) = store.tasks.iter().find(|task| {
            task.server_url == server_url
                && task.message.payload.receiver_agent_id == agent_id
                && task.message.payload.task_id == message.payload.task_id
        }) {
            ensure!(
                existing.message.payload == message.payload,
                "Conflicting duplicate task"
            );
            return Ok(message.ack());
        }
        ensure!(store.tasks.len() < MAX_TASKS, "Inbox is full");
        let ack = message.ack();
        let mut tasks = store.tasks.clone();
        tasks.push(StoredTask {
            server_url,
            message,
        });
        let snapshot = Snapshot {
            schema_version: 1,
            tasks,
        };
        // 임시 파일 생성 후 원자적 교체(Atomic replace)로 디스크에 완전 기록
        if write_snapshot(&store.path, &snapshot).is_err() {
            store.failed = true;
            anyhow::bail!("Cannot persist inbox snapshot");
        }
        store.tasks = snapshot.tasks;
        Ok(ack)
    }

    #[cfg(test)]
    pub(crate) fn pause_writes(
        &self,
    ) -> (
        std::sync::mpsc::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    ) {
        let store = self.0.clone();
        let (release, wait) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _guard = store.lock().unwrap();
            let _ = started.send(());
            let _ = wait.recv();
        });
        (release, ready)
    }

    /// 특정 서버 및 수신 Agent에 보관된 업무 목록 조회
    pub fn list_for(&self, server_url: &str, agent_id: u64) -> Result<Vec<StoredTask>> {
        let server_url = endpoint_url(server_url)?.to_string();
        let store = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Inbox lock failed"))?;
        Ok(store
            .tasks
            .iter()
            .filter(|task| {
                task.server_url == server_url && task.message.payload.receiver_agent_id == agent_id
            })
            .cloned()
            .collect())
    }
}

/// 디스크에 원자적으로 임시 파일을 기록하고 기존 수신함 파일을 교체(persist)
fn write_snapshot(path: &Path, snapshot: &Snapshot) -> Result<()> {
    let parent = path.parent().context("Inbox has no parent")?;
    let bytes = serde_json::to_vec(snapshot).context("Cannot encode inbox")?;
    ensure!(bytes.len() as u64 <= MAX_BYTES, "Inbox exceeds size limit");
    let mut temp = tempfile::Builder::new()
        .prefix(".inbox-")
        .tempfile_in(parent)?;
    credentials::set_private_permissions(temp.path())?;
    temp.write_all(&bytes)?;
    temp.as_file().sync_all()?;
    credentials::verify_private_permissions(temp.path())?;
    temp.persist(path)
        .map_err(|_| anyhow::anyhow!("Cannot replace inbox"))?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TaskCreatedPayload;

    fn task(event: &str, receiver: u64) -> TaskCreatedMessage {
        TaskCreatedMessage {
            event_id: event.into(),
            payload: TaskCreatedPayload {
                task_id: 101,
                team_id: 1,
                requester_agent_id: 10,
                receiver_agent_id: receiver,
                prompt: "테스트 작업".into(),
                created_at: "2026-10-29T09:00:00".into(),
            },
        }
    }

    #[test]
    fn restart_deduplicates_new_events_and_isolates_origin_and_agent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let inbox = Inbox::open(&path).unwrap();
        inbox
            .record_sync("ws://localhost:8080", 12, task("old", 12))
            .unwrap();
        let before = fs_read(&Inbox::path(&path));
        drop(inbox);
        let inbox = Inbox::open(&path).unwrap();
        let ack = inbox
            .record_sync("ws://localhost:8080/ws/broker", 12, task("new", 12))
            .unwrap();
        assert_eq!(ack.event_id, "new");
        assert_eq!(fs_read(&Inbox::path(&path)), before);
        inbox
            .record_sync("ws://localhost:8081", 12, task("other-server", 12))
            .unwrap();
        inbox
            .record_sync("ws://localhost:8080", 13, task("other-agent", 13))
            .unwrap();
        assert_eq!(inbox.list_for("ws://localhost:8080", 12).unwrap().len(), 1);
        assert_eq!(inbox.list_for("ws://localhost:8081", 12).unwrap().len(), 1);
        assert_eq!(inbox.list_for("ws://localhost:8080", 13).unwrap().len(), 1);
        assert_eq!(inbox.0.lock().unwrap().tasks.len(), 3);
        credentials::verify_private_permissions(&Inbox::path(&path)).unwrap();
    }

    #[test]
    fn invalid_recipient_and_conflicting_duplicates_preserve_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let inbox = Inbox::open(&path).unwrap();
        assert!(
            inbox
                .record_sync("ws://localhost:8080", 12, task("bad-recipient", 13))
                .is_err()
        );
        assert!(!Inbox::path(&path).exists());
        inbox
            .record_sync("ws://localhost:8080", 12, task("first", 12))
            .unwrap();
        let before = fs_read(&Inbox::path(&path));
        let mut conflict = task("conflict", 12);
        conflict.payload.prompt = "different".into();
        assert!(
            inbox
                .record_sync("ws://localhost:8080", 12, conflict)
                .is_err()
        );
        assert_eq!(fs_read(&Inbox::path(&path)), before);
    }

    #[test]
    fn corrupt_or_missing_snapshot_is_not_silently_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let inbox = Inbox::open(&path).unwrap();
        inbox
            .record_sync("ws://localhost:8080", 12, task("first", 12))
            .unwrap();
        std::fs::remove_file(Inbox::path(&path)).unwrap();
        assert!(
            inbox
                .record_sync("ws://localhost:8080", 12, task("duplicate", 12))
                .is_err()
        );
        std::fs::write(Inbox::path(&path), b"partial-json").unwrap();
        assert!(Inbox::open(&path).is_err());
        assert_eq!(fs_read(&Inbox::path(&path)), b"partial-json");
    }

    #[test]
    fn write_failure_does_not_publish_in_memory_and_limits_do_not_evict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let inbox = Inbox::open(&path).unwrap();
        let mut store = inbox.0.lock().unwrap();
        store.tasks = (1..=MAX_TASKS as u64)
            .map(|id| {
                let mut message = task("existing", 12);
                message.payload.task_id = id;
                StoredTask {
                    server_url: "ws://localhost:8080/ws/broker".into(),
                    message,
                }
            })
            .collect();
        write_snapshot(
            &store.path,
            &Snapshot {
                schema_version: 1,
                tasks: store.tasks.clone(),
            },
        )
        .unwrap();
        drop(store);
        let mut extra = task("full", 12);
        extra.payload.task_id = MAX_TASKS as u64 + 1;
        assert!(inbox.record_sync("ws://localhost:8080", 12, extra).is_err());
        assert_eq!(inbox.0.lock().unwrap().tasks.len(), MAX_TASKS);
        assert_eq!(
            Inbox::open(&path)
                .unwrap()
                .list_for("ws://localhost:8080", 12)
                .unwrap()
                .len(),
            MAX_TASKS
        );
        // A directory at the target path is rejected before any ACK can be built.
        std::fs::remove_file(Inbox::path(&path)).unwrap();
        std::fs::create_dir(Inbox::path(&path)).unwrap();
        assert!(
            inbox
                .record_sync("ws://localhost:8080", 12, task("blocked", 12))
                .is_err()
        );
    }

    fn fs_read(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap()
    }
}
