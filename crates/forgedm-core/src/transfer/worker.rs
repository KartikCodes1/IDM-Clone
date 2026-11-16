//! One connection's work in a segmented transfer (ARCHITECTURE 4.6 and 4.8, ADR-010).
//!
//! A worker is assigned a segment, requests `bytes=claim-(end-1)` with `If-Range`, and runs
//! the response through `decide` before anything is read from the body. Each chunk is
//! *claimed* under the table lock before it is buffered, so a worker can never take a byte
//! that a split moved to another segment; at the end of the segment (or on a shrink) it
//! flushes its buffer and drops the response, which closes the connection.
//!
//! Workers share a [`Shared`] context. The first failure of any worker is recorded there and
//! stops every other worker (`range_protocol_violation` and `remote_changed` included).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use forgedm_protocol::DownloadErrorCode;
use reqwest::Response;
use tokio::time::{Instant, sleep, timeout};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::resilience::{Resilience, Verdict};
use super::segment_table::{SegmentTable, TableError, WorkerId};
use crate::client::{
    ClientError, FetchSpec, HttpClient, classify_reqwest, response_meta, retry_after_of,
};
use crate::engine::LiveDownload;
use crate::error::{Failure, Fault, TaskStop};
use crate::http::content_range::RangeRequest;
use crate::http::range_decision::{RangeDecision, RangedRequest, decide};
use crate::http::validators::ResumeIdentity;
use crate::progress::SpeedMeter;
use crate::retry::{Phase, TransferErrorKind};
use crate::transfer::single::writer_stop;
use crate::writer::{Coalescer, WRITE_COALESCE, WriteHandle, WritePermit};

/// The table shared by the workers, the writer's tracker and the checkpointer.
pub type SharedTable = Arc<Mutex<SegmentTable>>;

/// Most requests one worker makes for one segment: a server answering a few bytes at a time
/// must not keep a download alive forever.
const MAX_REQUESTS_PER_SEGMENT: u32 = 4096;
/// How long a stopping worker waits for room in the writer queue to hand over its buffer.
const HANDOVER_GRACE: Duration = Duration::from_secs(2);
/// How often a worker publishes its speed to the table.
const SPEED_PUBLISH: Duration = Duration::from_millis(250);

/// Why a worker ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerEnd {
    /// Nothing was left to assign.
    NoWork,
    /// Asked to stop (retired, paused, cancelled, or another worker failed).
    Stopped,
    /// Failed; the cause is in [`Shared::take_fatal`].
    Failed,
}

/// What every worker of one download shares.
pub struct Shared {
    /// The segment table.
    pub table: SharedTable,
    /// Where pieces are queued for the writer thread.
    pub writer: WriteHandle,
    /// Progress counters (accepted bytes are published from here).
    pub live: Arc<LiveDownload>,
    /// The HTTP client.
    pub client: HttpClient,
    /// The original URL (redirects are re-followed per request).
    pub url: Url,
    /// The remote identity every response is judged against.
    pub identity: ResumeIdentity,
    /// `If-Range` value, when the identity has a strong validator.
    pub if_range: Option<String>,
    /// Retry decisions, budgets and the host cooldown of this download.
    pub res: Arc<Resilience>,
    /// Fires when every worker must stop (a user stop or a failure).
    pub halt: CancellationToken,
    fatal: Mutex<Option<TaskStop>>,
    final_url: Mutex<Option<Url>>,
}

/// What [`Shared::new`] is built from.
pub struct SharedInit {
    /// The segment table.
    pub table: SharedTable,
    /// Where pieces are queued for the writer thread.
    pub writer: WriteHandle,
    /// Progress counters.
    pub live: Arc<LiveDownload>,
    /// The HTTP client.
    pub client: HttpClient,
    /// The original URL.
    pub url: Url,
    /// The remote identity every response is judged against.
    pub identity: ResumeIdentity,
    /// Retry decisions of this download.
    pub res: Arc<Resilience>,
    /// Should be a child of the download's stop token.
    pub halt: CancellationToken,
}

impl Shared {
    /// Builds the shared context.
    #[must_use]
    pub fn new(init: SharedInit) -> Self {
        let if_range = init.identity.if_range();
        Self {
            table: init.table,
            writer: init.writer,
            live: init.live,
            client: init.client,
            url: init.url,
            identity: init.identity,
            if_range,
            res: init.res,
            halt: init.halt,
            fatal: Mutex::new(None),
            final_url: Mutex::new(None),
        }
    }

    /// Locks the table. Never hold the guard across an `.await`.
    pub fn lock(&self) -> MutexGuard<'_, SegmentTable> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records the first failure and stops every worker.
    pub fn fail(&self, stop: TaskStop) {
        let mut slot = self.fatal.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(stop);
        }
        drop(slot);
        self.halt.cancel();
    }

    /// The first recorded failure, if any.
    pub fn take_fatal(&self) -> Option<TaskStop> {
        self.fatal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// The URL of the first accepted response, once there is one.
    pub fn final_url(&self) -> Option<Url> {
        self.final_url
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn note_final_url(&self, url: Url) {
        let mut slot = self
            .final_url
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(url);
        }
    }
}

fn table_bug(err: &TableError) -> TaskStop {
    tracing::error!(error = %err, "a worker broke the segment table protocol");
    TaskStop::Fail(Failure::new(DownloadErrorCode::Internal))
}

fn client_failure(err: ClientError) -> TaskStop {
    match err {
        ClientError::Redirect(redirect) => TaskStop::Fail(Failure::new(redirect.code())),
        ClientError::Transport(kind) => TaskStop::Retry(Fault::Transport(kind)),
        ClientError::Cancelled | ClientError::Build(_) => {
            TaskStop::Fail(Failure::new(DownloadErrorCode::Internal))
        }
    }
}

/// What the accepted response allows a worker to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accepted {
    /// One past the last byte the body may carry for this request.
    pub end_excl: u64,
    /// A 206: bytes beyond `end_excl` are a violation. (A full 200 is simply cut off.)
    pub strict: bool,
}

/// Turns the range decision into what may be read, or the failure it stands for. A server
/// that stops honouring ranges mid-download is a violation here (the probe proved ranges),
/// never a reason to write at another offset.
///
/// # Errors
/// The [`TaskStop`] for every decision that is not an accept.
pub fn judge_segment(decision: RangeDecision) -> Result<Accepted, TaskStop> {
    match decision {
        RangeDecision::Accept { end_excl, .. } => Ok(Accepted {
            end_excl,
            strict: true,
        }),
        RangeDecision::Accept200Full { end_excl } => Ok(Accepted {
            end_excl,
            strict: false,
        }),
        RangeDecision::SegmentComplete => {
            Err(Failure::protocol_violation("unexpected_unsatisfied").into())
        }
        RangeDecision::RemoteChanged => Err(Failure::new(DownloadErrorCode::RemoteChanged).into()),
        RangeDecision::UnsupportedContentEncoding => {
            Err(Failure::new(DownloadErrorCode::UnsupportedContentEncoding).into())
        }
        RangeDecision::RangeUnsupported => Err(TaskStop::Reprobe("ranges_no_longer_supported")),
        RangeDecision::Reprobe => Err(TaskStop::Reprobe("reprobe_needed")),
        RangeDecision::Violation(violation) => {
            Err(Failure::protocol_violation(violation.reason()).into())
        }
        RangeDecision::OtherStatus { status: 401 | 407 } => Err(TaskStop::NeedsCredentials),
        RangeDecision::OtherStatus { status } => Err(TaskStop::Retry(Fault::Status {
            status,
            retry_after: None,
        })),
    }
}

/// Runs one worker: assign, fetch, repeat, until nothing is left or it is stopped. After
/// `stagger` (connections are not opened in a burst) it takes work from the table.
pub async fn run_worker(
    shared: Arc<Shared>,
    id: WorkerId,
    cancel: CancellationToken,
    stagger: Duration,
) -> WorkerEnd {
    if !stagger.is_zero() {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return WorkerEnd::Stopped,
            () = sleep(stagger) => {}
        }
    }
    loop {
        if cancel.is_cancelled() {
            return WorkerEnd::Stopped;
        }
        let assignment = shared.lock().assign(id);
        let Some(assignment) = assignment else {
            return WorkerEnd::NoWork;
        };
        let outcome = run_segment(&shared, id, assignment.seg_no, &cancel).await;
        shared.lock().release(assignment.seg_no, id);
        match outcome {
            Ok(SegmentEnd::Done) => {}
            Ok(SegmentEnd::Cancelled) => return WorkerEnd::Stopped,
            Err(stop) => {
                shared.fail(stop);
                return WorkerEnd::Failed;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentEnd {
    /// The segment has no unclaimed bytes left.
    Done,
    /// The worker was asked to stop.
    Cancelled,
}

/// Fetches segment `no` until its claim reaches its end, then hands over the buffer.
async fn run_segment(
    shared: &Shared,
    id: WorkerId,
    no: u32,
    cancel: &CancellationToken,
) -> Result<SegmentEnd, TaskStop> {
    let mut buffer = Coalescer::new();
    let outcome = fetch_segment(shared, id, no, cancel, &mut buffer).await;
    // Bytes that passed the range decision are valid whatever happened next: hand them over
    // so progress is not lost (best effort, bounded).
    if let Some((at, bytes)) = buffer.take_rest() {
        match timeout(HANDOVER_GRACE, send_piece(shared, no, at, bytes)).await {
            Ok(Ok(())) => {}
            Ok(Err(stop)) => {
                tracing::warn!(?stop, "could not hand over a worker buffer");
            }
            Err(_elapsed) => tracing::warn!("timed out handing over a worker buffer"),
        }
    }
    outcome
}

async fn fetch_segment(
    shared: &Shared,
    id: WorkerId,
    no: u32,
    cancel: &CancellationToken,
    buffer: &mut Coalescer,
) -> Result<SegmentEnd, TaskStop> {
    for _ in 0..MAX_REQUESTS_PER_SEGMENT {
        let window = shared.lock().segment(no).map(|s| (s.claim, s.end));
        let Some((claim, end)) = window else {
            return Err(table_bug(&TableError::NoSuchSegment(no)));
        };
        if claim >= end {
            return Ok(SegmentEnd::Done);
        }
        if shared.res.gate(cancel).await.is_err() {
            return Ok(SegmentEnd::Cancelled);
        }
        let ranged = (claim, end);
        match attempt(shared, id, no, ranged, cancel, buffer).await {
            Ok(SegmentEnd::Done) => {}
            Ok(SegmentEnd::Cancelled) => return Ok(SegmentEnd::Cancelled),
            Err(TaskStop::Retry(fault)) => {
                let phase = Phase::Transfer;
                match shared.res.on_fault(phase, no, fault, cancel).await {
                    Verdict::Retry => {}
                    Verdict::Cancelled => return Ok(SegmentEnd::Cancelled),
                    Verdict::Stop(stop) => return Err(stop),
                }
            }
            Err(other) => return Err(other),
        }
    }
    Err(Failure::protocol_violation("too_many_partial_responses").into())
}

/// One request for `[claim, end)` and the reading of its body.
async fn attempt(
    shared: &Shared,
    id: WorkerId,
    no: u32,
    (claim, end): (u64, u64),
    cancel: &CancellationToken,
    buffer: &mut Coalescer,
) -> Result<SegmentEnd, TaskStop> {
    let Some((response, accepted)) = open(shared, no, claim, end, cancel).await? else {
        return Ok(SegmentEnd::Cancelled);
    };
    let body = Body {
        shared,
        id,
        no,
        accepted,
        cancel,
    };
    body.stream(response, claim, buffer).await
}

/// Sends the request for `[claim, end)` and judges the response. `None` when cancelled.
async fn open(
    shared: &Shared,
    no: u32,
    claim: u64,
    end: u64,
    cancel: &CancellationToken,
) -> Result<Option<(Response, Accepted)>, TaskStop> {
    let window = RangeRequest {
        start: claim,
        end_excl: end,
    };
    let spec = FetchSpec::ranged(window, shared.if_range.clone());
    let fetched = match shared.client.fetch(&shared.url, &spec, cancel).await {
        Ok(fetched) => fetched,
        Err(ClientError::Cancelled) => return Ok(None),
        Err(other) => return Err(client_failure(other)),
    };
    let request = RangedRequest {
        range: window,
        if_range_sent: shared.if_range.is_some(),
        segment_no: no,
        has_committed_data: shared.lock().accepted_bytes() > 0,
    };
    let decision = decide(
        &request,
        &shared.identity,
        &response_meta(&fetched.response),
    );
    if let RangeDecision::OtherStatus { status } = decision
        && !matches!(status, 401 | 407)
    {
        let retry_after = retry_after_of(&fetched.response);
        return Err(TaskStop::Retry(Fault::Status {
            status,
            retry_after,
        }));
    }
    let accepted = judge_segment(decision)?;
    shared.note_final_url(fetched.final_url);
    Ok(Some((fetched.response, accepted)))
}

/// Queues one piece for the writer, waiting for a queue slot.
async fn send_piece(shared: &Shared, no: u32, at: u64, bytes: Bytes) -> Result<(), TaskStop> {
    let permit = shared.writer.reserve().await.map_err(|e| writer_stop(&e))?;
    queue_piece(shared, no, permit, at, bytes)
}

/// Records the piece as sent and queues it in the same step (no await in between), so
/// `sent` never runs ahead of the queue.
fn queue_piece(
    shared: &Shared,
    no: u32,
    permit: WritePermit<'_>,
    at: u64,
    bytes: Bytes,
) -> Result<(), TaskStop> {
    let upto = at + bytes.len() as u64;
    {
        let mut table = shared.lock();
        let accepted = table.mark_sent(no, upto).map_err(|e| table_bug(&e))?;
        shared.live.set_accepted(accepted);
    }
    permit.send(at, bytes);
    Ok(())
}

/// The reading side of one accepted response.
struct Body<'a> {
    shared: &'a Shared,
    id: WorkerId,
    no: u32,
    accepted: Accepted,
    cancel: &'a CancellationToken,
}

impl Body<'_> {
    /// Reads the body from stream position `start`, claiming every chunk before it is
    /// buffered. Returns when the accepted range is read, the segment ended or shrank
    /// (the response is dropped), or the worker was cancelled.
    async fn stream(
        &self,
        mut response: Response,
        start: u64,
        buffer: &mut Coalescer,
    ) -> Result<SegmentEnd, TaskStop> {
        let mut pos = start;
        let mut received = 0_u64;
        let mut meter = SpeedMeter::worker(Instant::now(), 0);
        let mut published = Instant::now();
        while pos < self.accepted.end_excl {
            let chunk = tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Ok(SegmentEnd::Cancelled),
                next = response.chunk() => next,
            };
            let bytes = match chunk {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    let eof = TransferErrorKind::PrematureEof;
                    return Err(TaskStop::Retry(Fault::Transport(eof)));
                }
                Err(err) => {
                    let kind = classify_reqwest(&err);
                    return Err(TaskStop::Retry(Fault::Transport(kind)));
                }
            };
            let (taken, allowed) = self.claim(pos, bytes.len())?;
            let kept = usize::try_from(allowed).unwrap_or(0);
            buffer
                .push(pos, &bytes[..kept])
                .map_err(|_| table_bug(&TableError::Order(self.no)))?;
            pos += allowed;
            received += allowed;
            self.shared.res.on_progress(self.no, allowed);
            if published.elapsed() >= SPEED_PUBLISH {
                meter.sample(Instant::now(), received);
                self.shared.lock().set_speed(self.no, meter.speed());
                published = Instant::now();
            }
            self.hand_over_full_pieces(buffer).await?;
            if self.cancel.is_cancelled() {
                return Ok(SegmentEnd::Cancelled);
            }
            if allowed < taken {
                // The segment ended or shrank: nothing more may be taken from this response.
                return Ok(SegmentEnd::Done);
            }
            if self.accepted.strict && (bytes.len() as u64) > taken {
                return Err(Failure::protocol_violation("body_beyond_content_range").into());
            }
        }
        Ok(SegmentEnd::Done)
    }

    /// Claims as much of a chunk of `len` bytes as the response and the segment allow.
    /// Returns `(bytes this response may still supply, bytes claimed)`.
    fn claim(&self, pos: u64, len: usize) -> Result<(u64, u64), TaskStop> {
        let room = self.accepted.end_excl - pos;
        let taken = (len as u64).min(room);
        let allowed = self
            .shared
            .lock()
            .claim(self.no, self.id, pos, taken)
            .map_err(|e| table_bug(&e))?;
        Ok((taken, allowed))
    }

    /// Queues every complete piece. A slot is reserved before the piece leaves the buffer,
    /// so a cancel while waiting loses nothing: the buffer stays contiguous with `sent` and
    /// the caller hands the rest over in order.
    async fn hand_over_full_pieces(&self, buffer: &mut Coalescer) -> Result<(), TaskStop> {
        while buffer.len() >= WRITE_COALESCE {
            let permit = tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Ok(()),
                reserved = self.shared.writer.reserve() => reserved.map_err(|e| writer_stop(&e))?,
            };
            let Some((at, piece)) = buffer.next_piece() else {
                return Ok(());
            };
            queue_piece(self.shared, self.no, permit, at, piece)?;
        }
        Ok(())
    }
}
