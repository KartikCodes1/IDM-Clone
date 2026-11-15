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

enum Msg {
    Data { offset: u64, bytes: Bytes },
    Flush(Reply),
    Close(Reply),
}

type SharedFailure = Arc<Mutex<Option<WriteFailure>>>;

/// The producer's end: coalesces data and talks to the writer thread.
#[derive(Debug)]
pub struct Writer {
    tx: mpsc::Sender<Msg>,
    thread: Option<JoinHandle<()>>,
    failure: SharedFailure,
    pending: BytesMut,
    pending_offset: u64,
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
        let (tx, rx) = mpsc::channel(WRITER_QUEUE);
        let failure: SharedFailure = Arc::new(Mutex::new(None));
        let thread_failure = Arc::clone(&failure);
        let thread = std::thread::Builder::new()
            .name(name)
            .spawn(move || run(sink, rx, &written, &thread_failure))
            .map_err(WriterError::Spawn)?;
        Ok(Self {
            tx,
            thread: Some(thread),
            failure,
            pending: BytesMut::with_capacity(WRITE_COALESCE),
            pending_offset: 0,
        })
    }

    /// Appends `data` at `offset`. Contiguous data is coalesced; a full piece is sent to
    /// the thread (waiting when the queue holds [`WRITER_QUEUE`] messages).
    ///
    /// # Errors
    /// [`WriterError::Failed`] when the thread stopped, [`WriterError::NotContiguous`] when
    /// `offset` does not continue the buffered data.
    pub async fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), WriterError> {
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
        while self.pending.len() >= WRITE_COALESCE {
            let piece = self.pending.split_to(WRITE_COALESCE).freeze();
            let at = self.pending_offset;
            self.pending_offset += WRITE_COALESCE as u64;
            self.send(Msg::Data {
                offset: at,
                bytes: piece,
            })
            .await?;
        }
        Ok(())
    }

    /// Bytes accepted but not yet handed to the thread.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.pending.len()
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
        if self.pending.is_empty() {
            return Ok(());
        }
        let bytes = self.pending.split().freeze();
        let offset = self.pending_offset;
        self.send(Msg::Data { offset, bytes }).await
    }

    async fn round_trip(&self, make: impl FnOnce(Reply) -> Msg) -> Result<u64, WriterError> {
        let (reply, answer) = oneshot::channel();
        self.send(make(reply)).await?;
        match answer.await {
            Ok(Ok(frontier)) => Ok(frontier),
            Ok(Err(failure)) => Err(WriterError::Failed(failure)),
            Err(_) => Err(self.stopped()),
        }
    }

    async fn send(&self, msg: Msg) -> Result<(), WriterError> {
        self.tx.send(msg).await.map_err(|_| self.stopped())
    }

    /// The error for a writer that is gone: its recorded failure, or a generic one.
    fn stopped(&self) -> WriterError {
        let recorded = self
            .failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        WriterError::Failed(recorded.unwrap_or(WriteFailure {
            class: IoClass::Other,
            kind: io::ErrorKind::BrokenPipe,
            message: "the writer thread stopped".to_owned(),
        }))
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
) {
    while let Some(msg) = rx.blocking_recv() {
        match msg {
            Msg::Data { offset, bytes } => {
                if let Err(err) = write_all_at(&mut sink, &bytes, offset, written) {
                    record(failure, WriteFailure::from_io(&err));
                    return;
                }
            }
            Msg::Flush(reply) => {
                let result = sink.sync_data();
                if !answer(result, reply, written, failure) {
                    return;
                }
            }
            Msg::Close(reply) => {
                let result = sink.sync_all();
                answer(result, reply, written, failure);
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
) -> bool {
    match result {
        Ok(()) => {
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
