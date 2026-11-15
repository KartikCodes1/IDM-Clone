//! Startup reconcile and shutdown settling of persisted download states
//! (ARCHITECTURE 2.6 and 6.3 step 1).
//!
//! Both directions use the pure mapping [`forgedm_core::state::on_startup`]:
//! `probing`, `downloading`, `verifying` become `interrupted`; `pausing` becomes `paused`;
//! `finalizing` is left for the part-file reconcile of S1.14; everything else is kept.
//! The only difference is the recorded reason: `crash` at startup (the previous agent did not
//! settle its downloads), `agent_shutdown` at a graceful shutdown. Each change is a
//! compare-and-set on the stored state with a `download_events` row.

use std::time::{SystemTime, UNIX_EPOCH};

use forgedm_core::state::on_startup;
use forgedm_protocol::DownloadState;
use forgedm_storage::{NewEvent, Storage, StorageError};

/// `state_reason` recorded when the agent found work it never settled.
pub const REASON_CRASH: &str = "crash";
/// `state_reason` recorded when the agent stopped them deliberately.
pub const REASON_SHUTDOWN: &str = "agent_shutdown";
/// `state_reason` of a download that was `pausing` when the agent went away.
const REASON_USER: &str = "user";
/// States that [`on_startup`] changes.
const UNSETTLED: [DownloadState; 4] = [
    DownloadState::Probing,
    DownloadState::Downloading,
    DownloadState::Verifying,
    DownloadState::Pausing,
];
/// Rows fetched per round; changed rows leave the filter, so each round starts at offset 0.
const PAGE: u32 = 500;
/// Event `kind` of a reconcile transition.
const EVENT_KIND: &str = "reconcile";

/// What a pass changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settled {
    /// Downloads moved to `interrupted`.
    pub interrupted: u32,
    /// Downloads moved from `pausing` to `paused`.
    pub paused: u32,
}

/// Settles downloads left in an active state by a previous agent (reason `crash`).
///
/// # Errors
/// [`StorageError`] when the database cannot be read or written.
pub async fn on_agent_start(storage: &Storage) -> Result<Settled, StorageError> {
    settle(storage, REASON_CRASH, PAGE).await
}

/// Settles active downloads at a graceful shutdown (reason `agent_shutdown`).
///
/// # Errors
/// [`StorageError`] when the database cannot be read or written.
pub async fn on_agent_shutdown(storage: &Storage) -> Result<Settled, StorageError> {
    settle(storage, REASON_SHUTDOWN, PAGE).await
}

async fn settle(
    storage: &Storage,
    interrupt_reason: &str,
    page: u32,
) -> Result<Settled, StorageError> {
    let downloads = storage.downloads();
    let mut settled = Settled::default();
    loop {
        let rows = downloads.list(Some(&UNSETTLED), page, 0).await?;
        if rows.is_empty() {
            return Ok(settled);
        }
        let mut progressed = false;
        for row in rows {
            let to = on_startup(row.state);
            if to == row.state {
                continue;
            }
            let reason = match to {
                DownloadState::Interrupted => interrupt_reason,
                DownloadState::Paused => REASON_USER,
                other => {
                    tracing::error!(state = ?other, "unexpected startup target state");
                    return Err(StorageError::InvalidValue { field: "state" });
                }
            };
            let mut event = NewEvent::new(now_ms(), EVENT_KIND);
            event.code = Some(reason.to_owned());
            match downloads
                .transition(row.id, row.state, to, Some(reason), &event)
                .await
            {
                Ok(()) => {
                    progressed = true;
                    if to == DownloadState::Interrupted {
                        settled.interrupted += 1;
                    } else {
                        settled.paused += 1;
                    }
                }
                // Another writer moved it first; it is no longer ours to settle.
                Err(StorageError::Conflict { .. }) => {}
                Err(other) => return Err(other),
            }
        }
        if !progressed {
            return Ok(settled);
        }
    }
}

/// Wall-clock milliseconds since the Unix epoch (0 if the clock is before it).
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use forgedm_protocol::{ClientKind, Priority};
    use forgedm_storage::NewDownload;
    use uuid::Uuid;

    use super::*;
    use crate::sealer::DpapiSealer;

    async fn storage() -> (tempfile::TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("t.db"), Arc::new(DpapiSealer))
            .await
            .unwrap();
        (dir, storage)
    }

    async fn seed(storage: &Storage, state: DownloadState) -> Uuid {
        let id = Uuid::now_v7();
        storage
            .downloads()
            .insert(&NewDownload {
                id,
                source: ClientKind::Cli,
                priority: Priority::Normal,
                queue_position: 0,
                original_url: "https://example.com/f?token=CANARY".to_owned(),
                url_display: "https://example.com/f".to_owned(),
                target_dir: r"C:\Downloads".to_owned(),
                requested_name: None,
                display_name: "f.bin".to_owned(),
                collision_policy: "rename".to_owned(),
                max_connections: 4,
                created_at_ms: 1,
            })
            .await
            .unwrap();
        if state != DownloadState::Queued {
            storage
                .downloads()
                .transition(
                    id,
                    DownloadState::Queued,
                    state,
                    None,
                    &NewEvent::new(2, "seed"),
                )
                .await
                .unwrap();
        }
        id
    }

    async fn state_of(storage: &Storage, id: Uuid) -> (DownloadState, Option<String>) {
        let row = storage.downloads().get(id).await.unwrap().unwrap();
        (row.state, row.state_reason)
    }

    #[tokio::test]
    async fn startup_maps_active_states_like_core_on_startup() {
        let (_dir, storage) = storage().await;
        let probing = seed(&storage, DownloadState::Probing).await;
        let downloading = seed(&storage, DownloadState::Downloading).await;
        let verifying = seed(&storage, DownloadState::Verifying).await;
        let pausing = seed(&storage, DownloadState::Pausing).await;
        let finalizing = seed(&storage, DownloadState::Finalizing).await;
        let queued = seed(&storage, DownloadState::Queued).await;
        let retry = seed(&storage, DownloadState::RetryWaiting).await;
        let paused = seed(&storage, DownloadState::Paused).await;

        let settled = on_agent_start(&storage).await.unwrap();

        assert_eq!(
            settled,
            Settled {
                interrupted: 3,
                paused: 1
            }
        );
        for id in [probing, downloading, verifying] {
            assert_eq!(
                state_of(&storage, id).await,
                (DownloadState::Interrupted, Some("crash".to_owned()))
            );
        }
        assert_eq!(state_of(&storage, pausing).await.0, DownloadState::Paused);
        assert_eq!(
            state_of(&storage, finalizing).await.0,
            DownloadState::Finalizing
        );
        assert_eq!(state_of(&storage, queued).await.0, DownloadState::Queued);
        assert_eq!(
            state_of(&storage, retry).await.0,
            DownloadState::RetryWaiting
        );
        assert_eq!(state_of(&storage, paused).await.0, DownloadState::Paused);
    }

    #[tokio::test]
    async fn shutdown_records_agent_shutdown_and_each_change_has_an_event() {
        let (_dir, storage) = storage().await;
        let downloading = seed(&storage, DownloadState::Downloading).await;

        on_agent_shutdown(&storage).await.unwrap();

        assert_eq!(
            state_of(&storage, downloading).await,
            (
                DownloadState::Interrupted,
                Some("agent_shutdown".to_owned())
            )
        );
        let events = storage.events().list(downloading, 10).await.unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.kind == "reconcile" && e.to_state == Some(DownloadState::Interrupted))
        );
    }

    #[tokio::test]
    async fn settling_is_idempotent() {
        let (_dir, storage) = storage().await;
        seed(&storage, DownloadState::Probing).await;
        on_agent_start(&storage).await.unwrap();
        assert_eq!(on_agent_start(&storage).await.unwrap(), Settled::default());
    }

    #[tokio::test]
    async fn more_rows_than_one_page_are_all_settled() {
        let (_dir, storage) = storage().await;
        for _ in 0..12 {
            seed(&storage, DownloadState::Downloading).await;
        }
        let settled = settle(&storage, REASON_CRASH, 5).await.unwrap();
        assert_eq!(settled.interrupted, 12);
    }
}
