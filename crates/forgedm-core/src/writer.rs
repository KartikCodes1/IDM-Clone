//! The writer thread (ARCHITECTURE 5.3, PLAN A.9 `WRITE_COALESCE`, `WRITER_QUEUE`).
//!
//! One `std::thread` per active download owns the file and reads a bounded channel of
//! [`WRITER_QUEUE`] messages. Producers coalesce contiguous data into pieces of at most
//! [`WRITE_COALESCE`] bytes before sending. The thread loops positional writes until a piece
//! is fully written (short writes are normal), writes at explicit offsets (the cursor position is never relied on), and never panics:
//! an I/O error is recorded and the thread exits, so the next producer call reports it.
//!
//! `Flush` runs `sync_data` and answers with the written frontier; `Close` runs `sync_all`
//! and drops the file, so no disk I/O ever happens on an async thread.
//!
//! A segmented download (S1.11) writes at many offsets from many workers: each worker owns a
//! [`Coalescer`] and sends its pieces through a cloned [`WriteHandle`]. A [`WriteTracker`]
//! lets the segment table follow every completed write and every `sync_data`.

use std::fs::File;
use std::io;
use std::os::windows::fs::FileExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use bytes::{Bytes, BytesMut};
use forgedm_platform::fs::{IoClass, classify_io};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

/// Largest single write the writer is handed (A.9).
pub const WRITE_COALESCE: usize = 256 * 1024;
/// Messages the writer queue holds (A.9).
pub const WRITER_QUEUE: usize = 16;

/// Positional file operations. The real implementation is [`File`]; tests substitute fakes
/// that write short or record the calling thread.
pub trait Sink: Send + 'static {
    /// Writes at most `buf.len()` bytes at `offset` and returns how many were written.
    ///
    /// # Errors
    /// Any I/O failure.
    fn write_at(&mut self, buf: &[u8], offset: u64) -> io::Result<usize>;
    /// Flushes written data to disk (`FlushFileBuffers`).
    ///
    /// # Errors
    /// Any I/O failure.
    fn sync_data(&mut self) -> io::Result<()>;
    /// Flushes data and metadata to disk.
    ///
    /// # Errors
    /// Any I/O failure.
    fn sync_all(&mut self) -> io::Result<()>;
}

impl Sink for File {
    fn write_at(&mut self, buf: &[u8], offset: u64) -> io::Result<usize> {
        self.seek_write(buf, offset)
    }

    fn sync_data(&mut self) -> io::Result<()> {
        File::sync_data(self)
    }

    fn sync_all(&mut self) -> io::Result<()> {
        File::sync_all(self)
    }
}

/// A recorded writer failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFailure {
    /// OS-code classification (disk full, device gone, ...).
    pub class: IoClass,
    /// The I/O error kind.
    pub kind: io::ErrorKind,
    /// The OS error text (never contains URLs; may contain a path fragment from the OS).
    pub message: String,
}

impl WriteFailure {
    fn from_io(err: &io::Error) -> Self {
        Self {
            class: classify_io(err),
            kind: err.kind(),
            message: err.to_string(),
        }
    }
}

/// Writer errors seen by the producer.
#[derive(Debug, Error)]
pub enum WriterError {
    /// The writer thread hit an I/O error and stopped.
    #[error("write failed: {}", .0.message)]
    Failed(WriteFailure),
    /// A write did not continue the previous one (single-stream writers only append).
    #[error("non-contiguous write at {got}, expected {expected}")]
    NotContiguous {
        /// Offset the producer supplied.
        got: u64,
        /// Offset that continues the buffered data.
        expected: u64,
    },
    /// The thread could not be started.
    #[error("cannot start the writer thread")]
    Spawn(#[source] io::Error),
}

type Reply = oneshot::Sender<Result<u64, WriteFailure>>;

/// Per-segment `(segment number, offset)` pairs.
pub type Frontier = Vec<(u32, u64)>;

/// Follows the writer thread's progress. Called on the writer thread, so implementations
/// must only take short locks and never block.
pub trait WriteTracker: Send + Sync + 'static {
    /// A piece `[offset, offset + len)` was written completely.
    fn wrote(&self, offset: u64, len: u64);
    /// A sync is about to run: the frontier it will make durable (taken before the sync so
    /// it never overstates what is on disk).
    fn before_sync(&self) -> Frontier;
    /// The sync that followed [`before_sync`](Self::before_sync) succeeded.
    fn synced(&self, frontier: &Frontier);
}

enum Msg {
    Data { offset: u64, bytes: Bytes },
    Flush(Reply),
    Close(Reply),
}

type SharedFailure = Arc<Mutex<Option<WriteFailure>>>;

/// The error for a writer that is gone: its recorded failure, or a generic one.
fn stopped(failure: &SharedFailure) -> WriterError {
    let recorded = failure
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    WriterError::Failed(recorded.unwrap_or(WriteFailure {
        class: IoClass::Other,
        kind: io::ErrorKind::BrokenPipe,
        message: "the writer thread stopped".to_owned(),
    }))
}

/// Gathers contiguous data into pieces of at most [`WRITE_COALESCE`] bytes.
#[derive(Debug, Default)]
pub struct Coalescer {
    pending: BytesMut,
    pending_offset: u64,
}

impl Coalescer {
    /// An empty coalescer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: BytesMut::with_capacity(WRITE_COALESCE),
            pending_offset: 0,
        }
    }

    /// Buffers `data` that starts at `offset`.
    ///
    /// # Errors
    /// [`WriterError::NotContiguous`] when `offset` does not continue the buffered data.
    pub fn push(&mut self, offset: u64, data: &[u8]) -> Result<(), WriterError> {
        let expected = self.pending_offset + self.pending.len() as u64;
        if self.pending.is_empty() {
            self.pending_offset = offset;
        } else if offset != expected {
            return Err(WriterError::NotContiguous {
                got: offset,
                expected,
            });
        }
        self.pending.extend_from_slice(data);
        Ok(())
    }

    /// The next complete piece, if a full [`WRITE_COALESCE`] bytes are buffered.
    pub fn next_piece(&mut self) -> Option<(u64, Bytes)> {
        if self.pending.len() < WRITE_COALESCE {
            return None;
        }
        let piece = self.pending.split_to(WRITE_COALESCE).freeze();
        let at = self.pending_offset;
        self.pending_offset += WRITE_COALESCE as u64;
        Some((at, piece))
    }

    /// Everything still buffered (a partial piece), emptying the buffer.
    pub fn take_rest(&mut self) -> Option<(u64, Bytes)> {
        if self.pending.is_empty() {
            return None;
        }
        let at = self.pending_offset;
        self.pending_offset += self.pending.len() as u64;
        Some((at, self.pending.split().freeze()))
    }

    /// Bytes buffered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether nothing is buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// A cloneable sender of positioned pieces. The queue is bounded ([`WRITER_QUEUE`]), so a
/// slow disk makes producers wait instead of buffering without limit.
#[derive(Debug, Clone)]
pub struct WriteHandle {
    tx: mpsc::Sender<Msg>,
    failure: SharedFailure,
}

/// A reserved queue slot: sending through it cannot wait or fail.
pub struct WritePermit<'a>(mpsc::Permit<'a, Msg>);

impl WritePermit<'_> {
    /// Queues `bytes` for a positional write at `offset`.
    pub fn send(self, offset: u64, bytes: Bytes) {
        self.0.send(Msg::Data { offset, bytes });
    }
}

impl WriteHandle {
    /// Waits for a free queue slot. Cancel-safe: dropping the future queues nothing.
    ///
    /// # Errors
    /// [`WriterError::Failed`] when the writer thread has stopped.
    pub async fn reserve(&self) -> Result<WritePermit<'_>, WriterError> {
        self.tx
            .reserve()
            .await
            .map(WritePermit)
            .map_err(|_| stopped(&self.failure))
    }

    /// Queues one piece (waiting for a slot).
    ///
    /// # Errors
    /// [`WriterError::Failed`] when the writer thread has stopped.
    pub async fn send(&self, offset: u64, bytes: Bytes) -> Result<(), WriterError> {
        self.reserve().await?.send(offset, bytes);
        Ok(())
    }

    /// Whether the writer thread is gone.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// The producer's end: coalesces data and talks to the writer thread.
#[derive(Debug)]
pub struct Writer {
    handle: WriteHandle,
    thread: Option<JoinHandle<()>>,
    coalescer: Coalescer,
}

impl Writer {
    /// Starts the writer thread `name` over `sink`. `written` is advanced after every
    /// complete positional write.
    ///
    /// # Errors
    /// [`WriterError::Spawn`] when the OS refuses a new thread.
    pub fn spawn(
        name: String,
        sink: impl Sink,
        written: Arc<AtomicU64>,
    ) -> Result<Self, WriterError> {
        Self::start(name, sink, written, None)
    }

    /// As [`spawn`](Self::spawn), with a [`WriteTracker`] that sees every completed write
    /// and every successful sync.
    ///
    /// # Errors
    /// [`WriterError::Spawn`] when the OS refuses a new thread.
    pub fn spawn_tracked(
        name: String,
        sink: impl Sink,
        written: Arc<AtomicU64>,
        tracker: Arc<dyn WriteTracker>,
    ) -> Result<Self, WriterError> {
        Self::start(name, sink, written, Some(tracker))
    }

    fn start(
        name: String,
        sink: impl Sink,
        written: Arc<AtomicU64>,
        tracker: Option<Arc<dyn WriteTracker>>,
    ) -> Result<Self, WriterError> {
        let (tx, rx) = mpsc::channel(WRITER_QUEUE);
        let failure: SharedFailure = Arc::new(Mutex::new(None));
        let thread_failure = Arc::clone(&failure);
        let thread = std::thread::Builder::new()
            .name(name)
            .spawn(move || run(sink, rx, &written, &thread_failure, tracker.as_deref()))
            .map_err(WriterError::Spawn)?;
        Ok(Self {
            handle: WriteHandle { tx, failure },
            thread: Some(thread),
            coalescer: Coalescer::new(),
        })
    }

    /// A cloneable handle for producers that coalesce on their own.
    #[must_use]
    pub fn handle(&self) -> WriteHandle {
        self.handle.clone()
    }

    /// Appends `data` at `offset`. Contiguous data is coalesced; a full piece is sent to
    /// the thread (waiting when the queue holds [`WRITER_QUEUE`] messages).
    ///
    /// # Errors
    /// [`WriterError::Failed`] when the thread stopped, [`WriterError::NotContiguous`] when
    /// `offset` does not continue the buffered data.
    pub async fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), WriterError> {
        self.coalescer.push(offset, data)?;
        while let Some((at, piece)) = self.coalescer.next_piece() {
            self.handle.send(at, piece).await?;
        }
        Ok(())
    }

    /// Bytes accepted but not yet handed to the thread.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.coalescer.len()
    }

    /// Sends whatever is buffered, then `sync_data`; returns the written frontier, which
    /// covers every byte given to [`write`](Self::write) so far.
    ///
    /// # Errors
    /// [`WriterError::Failed`] on any I/O error.
    pub async fn flush(&mut self) -> Result<u64, WriterError> {
        self.send_pending().await?;
        self.round_trip(Msg::Flush).await
    }

    /// Sends the rest, runs `sync_all`, closes the file and joins the thread. Returns the
    /// final written frontier.
    ///
    /// # Errors
    /// [`WriterError::Failed`] on any I/O error.
    pub async fn close(mut self) -> Result<u64, WriterError> {
        self.send_pending().await?;
        let frontier = self.round_trip(Msg::Close).await;
        if let Some(thread) = self.thread.take() {
            // The thread has already answered Close (or died); joining cannot block for long.
            if thread.join().is_err() {
                tracing::error!("the writer thread panicked");
            }
        }
        frontier
    }

    async fn send_pending(&mut self) -> Result<(), WriterError> {
        match self.coalescer.take_rest() {
            Some((offset, bytes)) => self.handle.send(offset, bytes).await,
            None => Ok(()),
        }
    }

    async fn round_trip(&self, make: impl FnOnce(Reply) -> Msg) -> Result<u64, WriterError> {
        let (reply, answer) = oneshot::channel();
        self.handle
            .tx
            .send(make(reply))
            .await
            .map_err(|_| stopped(&self.handle.failure))?;
        match answer.await {
            Ok(Ok(frontier)) => Ok(frontier),
            Ok(Err(failure)) => Err(WriterError::Failed(failure)),
            Err(_) => Err(stopped(&self.handle.failure)),
        }
    }
}

fn record(failure: &SharedFailure, value: WriteFailure) {
    *failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(value);
}

/// The thread body. Ends on `Close`, on the first I/O error, or when every sender is gone.
fn run(
    mut sink: impl Sink,
    mut rx: mpsc::Receiver<Msg>,
    written: &AtomicU64,
    failure: &SharedFailure,
    tracker: Option<&dyn WriteTracker>,
) {
    while let Some(msg) = rx.blocking_recv() {
        match msg {
            Msg::Data { offset, bytes } => {
                if let Err(err) = write_all_at(&mut sink, &bytes, offset, written) {
                    record(failure, WriteFailure::from_io(&err));
                    return;
                }
                if let Some(tracker) = tracker {
                    tracker.wrote(offset, bytes.len() as u64);
                }
            }
            Msg::Flush(reply) => {
                let frontier = tracker.map(WriteTracker::before_sync);
                let result = sink.sync_data();
                if !answer(result, reply, written, failure, tracker, frontier) {
                    return;
                }
            }
            Msg::Close(reply) => {
                let frontier = tracker.map(WriteTracker::before_sync);
                let result = sink.sync_all();
                answer(result, reply, written, failure, tracker, frontier);
                return;
            }
        }
    }
}

/// Replies with the frontier or the failure; returns whether the thread may continue.
fn answer(
    result: io::Result<()>,
    reply: Reply,
    written: &AtomicU64,
    failure: &SharedFailure,
    tracker: Option<&dyn WriteTracker>,
    frontier: Option<Frontier>,
) -> bool {
    match result {
        Ok(()) => {
            if let (Some(tracker), Some(frontier)) = (tracker, frontier.as_ref()) {
                tracker.synced(frontier);
            }
            let _ = reply.send(Ok(written.load(Ordering::Acquire)));
            true
        }
        Err(err) => {
            let value = WriteFailure::from_io(&err);
            record(failure, value.clone());
            let _ = reply.send(Err(value));
            false
        }
    }
}

/// Positional write loop: continues after short writes and interrupted calls.
fn write_all_at(
    sink: &mut impl Sink,
    mut buf: &[u8],
    mut offset: u64,
    written: &AtomicU64,
) -> io::Result<()> {
    while !buf.is_empty() {
        match sink.write_at(buf, offset) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(n) => {
                let n = n.min(buf.len());
                buf = &buf[n..];
                offset += n as u64;
                written.fetch_add(n as u64, Ordering::Release);
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}
