//! ForgeDM adversarial HTTP fixture server (dev only).
//!
//! Serves deterministic content at `GET|HEAD /f/{seed}/{size}` on `127.0.0.1` only, with
//! behaviour flags in the query string (see [`flags`]) and a [`log::RequestLog`] of every request.
//! This crate depends on no other ForgeDM crate.

#![forbid(unsafe_code)]

pub mod body;
pub mod content;
pub mod flags;
pub mod log;
pub mod range;
pub mod response;
pub mod server;
pub mod verify;

pub use log::{LoggedRequest, RequestLog};
pub use server::{FixtureError, FixtureServer};
