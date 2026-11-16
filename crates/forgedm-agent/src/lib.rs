//! ForgeDM agent library: the background process that owns the queue, the database and
//! all disk writes. The `forgedm-agent` binary is a thin wrapper around [`app`].
//!
//! Served so far: Handshake, Ping, GetAgentStatus, GetSnapshot, Subscribe, Unsubscribe,
//! ListDownloads, ShutdownAgentSafely (S0.8), EnqueueUrl and GetDownload (S1.7) and Pause,
//! Resume, Cancel, Retry and Remove (S1.9), UpdateLimits (S1.11, S1.13), PauseAll, ResumeAll
//! and UpdatePriority (S1.13); a handler without an engine answers `not_implemented`.
//!
//! - [`config`]: flags and the pipe name derivation.
//! - [`data_dir`]: data directory layout and the reparse-point check.
//! - [`logging`]: daily rolling JSON logs and the panic hook.
//! - [`sealer`]: DPAPI implementation of the storage `SecretSealer` port.
//! - [`reconcile`]: startup and shutdown settling of persisted states, and the part-file
//!   reconcile of `finalizing` downloads.
//! - `fault` (feature `fault-injection` only): crash injection for the recovery tests.
//! - [`events_hub`]: the StateHub actor (sequencing, replay, subscribers, periodic status).
//! - [`engine_host`]: the core engine wired to storage, the hub and the platform services.
//! - [`limits_store`]: the agent-wide limits kept in `settings`.
//! - [`store_adapter`]: the core `DownloadStore` port over `forgedm-storage`.
//! - [`destination`]: validation of destination, file name and checksum.
//! - [`detail`]: summaries with live numbers and the `GetDownload` view.
//! - [`handlers`]: the request handler.
//! - [`app`]: startup order, serving and shutdown.
//! - [`error`]: [`error::StartupError`] (exit codes) and [`error::AgentError`] (wire codes).

#![forbid(unsafe_code)]

pub mod app;
pub mod config;
pub mod data_dir;
pub mod destination;
pub mod detail;
pub mod engine_host;
pub mod error;
pub mod events_hub;
#[cfg(feature = "fault-injection")]
pub mod fault;
pub mod handlers;
pub mod limits_store;
pub mod logging;
pub mod reconcile;
mod reconcile_parts;
pub mod sealer;
pub mod store_adapter;
pub mod summary;
