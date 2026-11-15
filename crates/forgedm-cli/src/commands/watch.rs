//! `forgedm watch <id>` and the follow loop behind `add --watch`.
//!
//! Follows one download over the documented client sequence: `GetSnapshot`, then
//! `Subscribe(after_seq = snapshot.seq)`, so no change between the two is lost. Bytes, speed
//! and ETA come from the agent's `ProgressSnapshot` events (true accepted bytes), never from
//! a client-side estimate. The follow ends when the download does: success returns `Ok`;
//! `failed`, `cancelled` and the states that need the user (`needs_credentials`,
//! `needs_url_refresh`) end with exit 13.

use std::time::{Duration, Instant};

use forgedm_ipc::Client;
use forgedm_protocol::{
    DownloadDetail, DownloadIdRequest, DownloadState, DownloadSummary, Empty, EventBody,
    HandshakeResult, ProgressItem, RequestBody, ResultBody, SnapshotResult, SubscribeRequest,
};
use uuid::Uuid;

use crate::client::Target;
use crate::error::CliError;
use crate::output;

/// Progress lines are printed at most this often, whatever the agent's rate.
const PROGRESS_PRINT_INTERVAL: Duration = Duration::from_millis(500);
/// `state_reason` the agent sets on a download that restarts from zero after a crash.
const REASON_RESTART_FROM_ZERO: &str = "restart_from_zero";
/// Durable event kind recorded when a download restarts from zero.
const EVENT_RESTART_FROM_ZERO: &str = "restart_from_zero";
/// `paused` reasons that nothing resumes by itself; the follow ends instead of hanging.
const STUCK_PAUSE_REASONS: [&str; 2] = ["disk_full", "destination_unavailable"];

/// `forgedm watch <id>`: connects (never starting the agent) and follows the download.
pub async fn run(target: &Target, id: Uuid) -> Result<(), CliError> {
    let (mut client, hello) = target.connect().await?;
    follow(&mut client, &hello, id).await
}

/// Whether `state` ends the follow.
fn ends_follow(state: DownloadState, reason: Option<&str>) -> bool {
    if state == DownloadState::Paused {
        return reason.is_some_and(|r| STUCK_PAUSE_REASONS.contains(&r));
    }
    matches!(
        state,
        DownloadState::Completed
            | DownloadState::Failed
            | DownloadState::Cancelled
            | DownloadState::NeedsCredentials
            | DownloadState::NeedsUrlRefresh
    )
}

/// Follows download `id` until it ends.
///
/// # Errors
/// [`CliError::DownloadEnded`] (exit 13) when it ended `failed`, `cancelled` or needs the
/// user; `not_found` and transport failures keep their own mapping.
pub async fn follow(
    client: &mut Client,
    hello: &HandshakeResult,
    id: Uuid,
) -> Result<(), CliError> {
    let mut view = View::new(id);
    loop {
        let snapshot = snapshot(client).await?;
        subscribe(client, hello, snapshot.seq).await?;
        let initial = match snapshot.downloads.iter().find(|d| d.id == id) {
            Some(summary) => summary.clone(),
            None => detail(client, id).await?.summary,
        };
        if !view.noted_restart {
            let known = detail(client, id).await?;
            if known
                .recent_events
                .iter()
                .any(|e| e.kind == EVENT_RESTART_FROM_ZERO)
            {
                view.noted_restart = true;
                output::restarting_from_zero();
            }
        }
        if view.apply(&initial) {
            return finish(client, id).await;
        }
        if follow_events(client, &mut view).await? {
            return finish(client, id).await;
        }
        // The agent dropped the subscription (ResyncRequired): start over from a snapshot.
    }
}

/// Reads events until the download ends (`true`) or a resync is requested (`false`).
async fn follow_events(client: &mut Client, view: &mut View) -> Result<bool, CliError> {
    loop {
        match client.next_event().await?.body {
            EventBody::DownloadChanged(summary) if summary.id == view.id => {
                if view.apply(&summary) {
                    return Ok(true);
                }
            }
            EventBody::ProgressSnapshot(progress) => {
                if let Some(item) = progress.items.iter().find(|i| i.id == view.id) {
                    view.progress(item);
                }
            }
            EventBody::ResyncRequired(_) => return Ok(false),
            _ => {}
        }
    }
}

/// What the follow has shown so far.
struct View {
    id: Uuid,
    state: Option<DownloadState>,
    last_progress: Option<Instant>,
    noted_restart: bool,
}

impl View {
    fn new(id: Uuid) -> Self {
        Self {
            id,
            state: None,
            last_progress: None,
            noted_restart: false,
        }
    }

    /// Shows a state change; returns whether the download has ended.
    fn apply(&mut self, summary: &DownloadSummary) -> bool {
        if summary.state_reason.as_deref() == Some(REASON_RESTART_FROM_ZERO) && !self.noted_restart
        {
            self.noted_restart = true;
            output::restarting_from_zero();
        }
        if self.state != Some(summary.state) {
            self.state = Some(summary.state);
            output::state_change(summary.state);
        }
        ends_follow(summary.state, summary.state_reason.as_deref())
    }

    /// Shows a progress line while downloading, at most twice a second.
    fn progress(&mut self, item: &ProgressItem) {
        if self.state != Some(DownloadState::Downloading) {
            return;
        }
        let due = self
            .last_progress
            .is_none_or(|at| at.elapsed() >= PROGRESS_PRINT_INTERVAL);
        if due {
            self.last_progress = Some(Instant::now());
            output::progress(item);
        }
    }
}

async fn snapshot(client: &mut Client) -> Result<SnapshotResult, CliError> {
    match client.call(RequestBody::GetSnapshot(Empty {})).await? {
        ResultBody::GetSnapshot(snapshot) => Ok(*snapshot),
        _ => Err(CliError::Unexpected),
    }
}

async fn subscribe(
    client: &mut Client,
    hello: &HandshakeResult,
    after_seq: u64,
) -> Result<(), CliError> {
    let request = SubscribeRequest {
        agent_instance_id: hello.agent_instance_id,
        after_seq,
    };
    match client.call(RequestBody::Subscribe(request)).await? {
        ResultBody::Subscribe(_) => Ok(()),
        _ => Err(CliError::Unexpected),
    }
}

async fn detail(client: &mut Client, id: Uuid) -> Result<DownloadDetail, CliError> {
    let request = RequestBody::GetDownload(DownloadIdRequest { download_id: id });
    match client.call(request).await? {
        ResultBody::GetDownload(detail) => Ok(*detail),
        _ => Err(CliError::Unexpected),
    }
}

/// Prints the result and maps the end state to success or exit 13.
async fn finish(client: &mut Client, id: Uuid) -> Result<(), CliError> {
    let detail = detail(client, id).await?;
    if detail.summary.state == DownloadState::Completed {
        output::completed(&detail);
        Ok(())
    } else {
        Err(CliError::DownloadEnded(output::ending_text(
            &detail.summary,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_final_and_user_blocked_states_end_the_follow() {
        for state in [
            DownloadState::Completed,
            DownloadState::Failed,
            DownloadState::Cancelled,
            DownloadState::NeedsCredentials,
            DownloadState::NeedsUrlRefresh,
        ] {
            assert!(ends_follow(state, None), "{state:?}");
        }
        for state in [
            DownloadState::Queued,
            DownloadState::Probing,
            DownloadState::Downloading,
            DownloadState::Verifying,
            DownloadState::Finalizing,
            DownloadState::Paused,
            DownloadState::Interrupted,
            DownloadState::RetryWaiting,
        ] {
            assert!(!ends_follow(state, None), "{state:?}");
        }
    }

    #[test]
    fn a_pause_nothing_resumes_ends_the_follow() {
        for reason in ["disk_full", "destination_unavailable"] {
            assert!(ends_follow(DownloadState::Paused, Some(reason)));
        }
        assert!(!ends_follow(DownloadState::Paused, Some("user")));
    }
}
