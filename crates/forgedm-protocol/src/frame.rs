//! Frame codec (Appendix A.1): `u32` little-endian length followed by a UTF-8 JSON object.
//!
//! The codec is pure: it never performs I/O. [`FrameDecoder`] consumes arbitrary byte
//! chunks and rejects an invalid length as soon as the four header bytes are known, before
//! a single body byte is buffered.

use std::collections::VecDeque;

use serde_json::{Map, Value};

use crate::constants::{FRAME_HEADER_LEN, MAX_FRAME};
use crate::enums::ErrorCode;

/// Body bytes reserved up front; larger bodies grow as data actually arrives, so a
/// peer cannot make the decoder allocate a full frame by sending only a header.
const INITIAL_BODY_CAPACITY: usize = 16 * 1024;

/// Failure to encode, decode or interpret a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The length prefix was zero.
    #[error("frame length is zero")]
    ZeroLength,
    /// The length prefix exceeded [`MAX_FRAME`].
    #[error("frame length {len} exceeds the {MAX_FRAME} byte maximum")]
    TooLarge {
        /// The declared length.
        len: u32,
    },
    /// The stream ended inside a header or body.
    #[error("stream ended inside a frame")]
    UnexpectedEof,
    /// The body was not valid UTF-8 JSON, or not a JSON object.
    #[error("frame body is not a UTF-8 JSON object")]
    InvalidBody,
}

impl FrameError {
    /// The protocol error code the agent reports for this failure. Every frame error
    /// closes the connection.
    #[must_use]
    pub const fn error_code(self) -> ErrorCode {
        match self {
            Self::TooLarge { .. } => ErrorCode::FrameTooLarge,
            Self::ZeroLength | Self::UnexpectedEof | Self::InvalidBody => ErrorCode::MalformedFrame,
        }
    }
}

/// Validates a declared body length (`1..=MAX_FRAME`).
fn check_len(len: u32) -> Result<usize, FrameError> {
    if len == 0 {
        return Err(FrameError::ZeroLength);
    }
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge { len });
    }
    usize::try_from(len).map_err(|_| FrameError::TooLarge { len })
}

/// Prefixes `body` with its little-endian length.
///
/// # Errors
/// [`FrameError::ZeroLength`] for an empty body and [`FrameError::TooLarge`] above
/// [`MAX_FRAME`]. A body too long for `u32` reports `len: u32::MAX` (saturated, not the
/// real size).
pub fn encode_frame(body: &[u8]) -> Result<Vec<u8>, FrameError> {
    let len = u32::try_from(body.len()).map_err(|_| FrameError::TooLarge { len: u32::MAX })?;
    check_len(len)?;
    let mut wire = Vec::with_capacity(FRAME_HEADER_LEN + body.len());
    wire.extend_from_slice(&len.to_le_bytes());
    wire.extend_from_slice(body);
    Ok(wire)
}

/// Parses a frame body as a JSON object.
///
/// # Errors
/// [`FrameError::InvalidBody`] for invalid UTF-8, invalid JSON or a non-object value.
pub fn decode_json_object(body: &[u8]) -> Result<Map<String, Value>, FrameError> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(FrameError::InvalidBody),
    }
}

/// Incremental frame decoder.
///
/// Feed byte chunks with [`feed`](Self::feed) and pop completed bodies with
/// [`next_frame`](Self::next_frame). After the first error the decoder is poisoned and
/// every later `feed` returns that error; frames completed before the error remain
/// available.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    header: [u8; FRAME_HEADER_LEN],
    header_len: usize,
    /// Declared length of the body being read; 0 while reading a header.
    expected: usize,
    body: Vec<u8>,
    ready: VecDeque<Vec<u8>>,
    failed: Option<FrameError>,
}

impl FrameDecoder {
    /// A decoder at a frame boundary.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes of an unfinished frame currently held (header bytes plus body bytes).
    #[must_use]
    pub fn buffered_bytes(&self) -> usize {
        self.header_len + self.body.len()
    }

    /// Consumes `chunk`, completing zero or more frames.
    ///
    /// # Errors
    /// The first framing error. A bad length is reported as soon as its four bytes are
    /// available, without buffering any body bytes.
    pub fn feed(&mut self, mut chunk: &[u8]) -> Result<(), FrameError> {
        if let Some(err) = self.failed {
            return Err(err);
        }
        while !chunk.is_empty() {
            let consumed = if self.expected == 0 {
                self.take_header(chunk)
            } else {
                Ok(self.take_body(chunk))
            };
            match consumed {
                Ok(n) => chunk = &chunk[n..],
                Err(err) => return Err(self.poison(err)),
            }
        }
        Ok(())
    }

    /// Pops the oldest completed frame body.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        self.ready.pop_front()
    }

    /// Declares end of stream.
    ///
    /// # Errors
    /// [`FrameError::UnexpectedEof`] when the stream ended inside a header or body, or
    /// the decoder's earlier error.
    pub fn finish(&self) -> Result<(), FrameError> {
        if let Some(err) = self.failed {
            return Err(err);
        }
        if self.header_len > 0 || self.expected > 0 {
            return Err(FrameError::UnexpectedEof);
        }
        Ok(())
    }

    fn poison(&mut self, err: FrameError) -> FrameError {
        self.failed = Some(err);
        self.header_len = 0;
        self.expected = 0;
        self.body = Vec::new();
        err
    }

    fn take_header(&mut self, chunk: &[u8]) -> Result<usize, FrameError> {
        let n = (FRAME_HEADER_LEN - self.header_len).min(chunk.len());
        self.header[self.header_len..self.header_len + n].copy_from_slice(&chunk[..n]);
        self.header_len += n;
        if self.header_len == FRAME_HEADER_LEN {
            let len = check_len(u32::from_le_bytes(self.header))?;
            self.header_len = 0;
            self.expected = len;
            self.body = Vec::with_capacity(len.min(INITIAL_BODY_CAPACITY));
        }
        Ok(n)
    }

    fn take_body(&mut self, chunk: &[u8]) -> usize {
        let n = (self.expected - self.body.len()).min(chunk.len());
        self.body.extend_from_slice(&chunk[..n]);
        if self.body.len() == self.expected {
            self.expected = 0;
            self.ready.push_back(std::mem::take(&mut self.body));
        }
        n
    }
}
