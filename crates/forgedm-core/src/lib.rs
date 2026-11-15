//! ForgeDM domain and download engine.
//!
//! Pure policy modules ([`url_policy`], [`naming`], [`state`], [`http`], [`retry`],
//! [`progress`], [`rate_limit`]) feed the engine: [`client`] (HTTP, ADR-007), [`writer`] (the
//! writer thread), [`hash`], [`finalize`], [`transfer`] and [`engine`]. The engine talks to
//! the outside only through the [`ports`] (`DownloadStore`, `EngineEventSink`).

#![forbid(unsafe_code)]

// Lets `testing.rs` name `forgedm_core::` paths both inside this crate and when an
// integration test includes the file with `#[path]`.
#[cfg(any(test, feature = "test-support"))]
extern crate self as forgedm_core;

pub mod checkpoint;
pub mod client;
pub mod clock;
pub mod engine;
pub mod error;
pub mod finalize;
pub mod hash;
pub mod http;
pub mod naming;
pub mod ports;
pub mod probe;
pub mod progress;
pub mod rate_limit;
pub mod resume;
pub mod retry;
pub mod state;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod transfer;
pub mod url_policy;
pub mod writer;
