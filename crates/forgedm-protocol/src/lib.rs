//! ForgeDM wire protocol v1.
//!
//! This crate is pure: no I/O and no async runtime. It defines the framing codec, the
//! request, response and event envelopes, the DTOs and the shared enums exactly as in
//! PHASE_0_1_PLAN Appendix A, plus `validate()` for the request limits.
//!
//! - Frames: [`encode_frame`], [`FrameDecoder`], [`decode_json_object`].
//! - Requests (agent side, strict): [`parse_request`], [`Request::validate`].
//! - Server messages (client side, tolerant): [`parse_server_message`].
//! - Golden JSON for every shape lives in `tests/golden/v1` and is shared with the
//!   desktop TypeScript tests.

#![forbid(unsafe_code)]

mod actions;
mod constants;
mod dto;
mod enums;
mod envelope;
mod error;
mod events;
mod frame;
mod payloads;
mod results;
mod strict_json;
mod validate;
mod version;

pub use actions::{RequestBody, ResultBody};
pub use constants::*;
pub use dto::*;
pub use enums::*;
pub use envelope::{
    EncodeError, ParseError, Request, Response, ServerMessage, parse_request, parse_server_message,
};
pub use error::*;
pub use events::*;
pub use frame::{FrameDecoder, FrameError, decode_json_object, encode_frame};
pub use payloads::*;
pub use results::*;
pub use validate::ValidationError;
pub use version::{UnsupportedVersion, negotiate_version};
