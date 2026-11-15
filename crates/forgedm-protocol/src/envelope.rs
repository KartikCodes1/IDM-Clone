//! Message envelopes and two-step parsing (Appendix A.1).
//!
//! Requests are parsed in two steps: a raw envelope with `deny_unknown_fields`, then the
//! typed payload (also `deny_unknown_fields`). There is no `flatten` anywhere in the
//! envelope path. Server messages are parsed tolerantly for results and events.

use std::str::FromStr;

use serde::ser::{Error as _, SerializeStruct};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use crate::actions::{RequestBody, ResultBody};
use crate::constants::PROTOCOL_VERSION;
use crate::enums::{Action, ErrorCode};
use crate::error::ErrorBody;
use crate::events::{Event, EventBody};
use crate::frame::{FrameError, encode_frame};
use crate::payloads::present_or_null;
use crate::strict_json::StrictValue;

/// Longest field name echoed back in an `invalid_payload` detail.
const MAX_ECHOED_FIELD_CHARS: usize = 64;

/// A failure to encode a message into a frame.
#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    /// The message could not be serialized.
    #[error("message could not be serialized")]
    Serialize(#[from] serde_json::Error),
    /// The serialized message does not fit a frame.
    #[error("message does not fit a frame")]
    Frame(#[from] FrameError),
}

/// A failure to parse an incoming message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Not UTF-8, not JSON, or not a JSON object. Closes the connection.
    #[error("malformed frame")]
    Malformed,
    /// `protocol_version` is not 1.
    #[error("unsupported protocol version")]
    UnsupportedVersion {
        /// The request to answer.
        request_id: u32,
    },
    /// The action name is not one of the 19 actions.
    #[error("unknown action")]
    UnknownAction {
        /// The request to answer.
        request_id: u32,
    },
    /// The envelope or payload has an unknown, missing or mistyped field.
    #[error("invalid payload")]
    InvalidPayload {
        /// The request to answer, when it could be recovered.
        request_id: Option<u32>,
        /// The offending field name, when known.
        field: Option<String>,
        /// A static machine reason such as `unknown_field`.
        reason: &'static str,
    },
}

impl ParseError {
    /// The id of the request to answer, when it could be recovered.
    #[must_use]
    pub fn request_id(&self) -> Option<u32> {
        match self {
            Self::Malformed => None,
            Self::UnsupportedVersion { request_id } | Self::UnknownAction { request_id } => {
                Some(*request_id)
            }
            Self::InvalidPayload { request_id, .. } => *request_id,
        }
    }

    /// The response error the agent sends. It never echoes request values.
    #[must_use]
    pub fn to_error_body(&self) -> ErrorBody {
        match self {
            Self::Malformed => ErrorBody::new(ErrorCode::MalformedFrame),
            Self::UnsupportedVersion { .. } => ErrorBody::unsupported_protocol_version(),
            Self::UnknownAction { .. } => ErrorBody::new(ErrorCode::UnknownAction),
            Self::InvalidPayload { field, reason, .. } => {
                ErrorBody::invalid_payload(field.as_deref(), reason)
            }
        }
    }

    /// Whether the agent closes the connection after replying.
    #[must_use]
    pub fn closes_connection(&self) -> bool {
        self.to_error_body().code.closes_connection()
    }
}

/// Maps a serde error to a field name and a static reason without echoing any value.
fn describe(err: &serde_json::Error) -> (Option<String>, &'static str) {
    const REASONS: [(&str, &str, bool); 6] = [
        ("unknown field", "unknown_field", true),
        ("missing field", "missing_field", true),
        ("duplicate field", "duplicate_field", true),
        ("invalid type", "invalid_type", false),
        ("unknown variant", "unknown_variant", false),
        ("invalid length", "invalid_length", false),
    ];
    let text = err.to_string();
    for (prefix, reason, names_field) in REASONS {
        if text.starts_with(prefix) {
            let field = names_field.then(|| quoted_name(&text)).flatten();
            return (field, reason);
        }
    }
    (None, "invalid_value")
}

/// The first backtick-quoted identifier in a serde message, sanitized and capped.
fn quoted_name(text: &str) -> Option<String> {
    let name: String = text
        .split('`')
        .nth(1)?
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(MAX_ECHOED_FIELD_CHARS)
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Classifies a serde error from parsing a whole frame body.
fn classify_body_error(body: &[u8], err: &serde_json::Error) -> ParseError {
    let is_object = body.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{');
    if err.is_data() && is_object {
        let (field, reason) = describe(err);
        return ParseError::InvalidPayload {
            request_id: recover_request_id(body),
            field,
            reason,
        };
    }
    ParseError::Malformed
}

/// Best-effort `request_id` of a request that failed envelope parsing.
fn recover_request_id(body: &[u8]) -> Option<u32> {
    let value: Value = serde_json::from_slice(body).ok()?;
    u32::try_from(value.get("request_id")?.as_u64()?).ok()
}

/// A request: `{"protocol_version":1,"request_id":..,"action":..,"payload":{..}}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Unique per connection.
    pub request_id: u32,
    /// The action and its payload.
    pub body: RequestBody,
}

/// Step one of request parsing.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRequest {
    protocol_version: u32,
    request_id: u32,
    action: String,
    payload: StrictValue,
}

/// Parses one request frame body (step one: envelope, step two: typed payload).
///
/// # Errors
/// [`ParseError`], which maps onto the response error codes of Appendix A.7.
pub fn parse_request(body: &[u8]) -> Result<Request, ParseError> {
    let raw: RawRequest =
        serde_json::from_slice(body).map_err(|err| classify_body_error(body, &err))?;
    let request_id = raw.request_id;
    if raw.protocol_version != PROTOCOL_VERSION {
        return Err(ParseError::UnsupportedVersion { request_id });
    }
    let action =
        Action::from_str(&raw.action).map_err(|_| ParseError::UnknownAction { request_id })?;
    let body = RequestBody::from_parts(action, raw.payload.0).map_err(|err| {
        let (field, reason) = describe(&err);
        ParseError::InvalidPayload {
            request_id: Some(request_id),
            field,
            reason,
        }
    })?;
    Ok(Request { request_id, body })
}

impl Request {
    /// A request with the given id and body.
    #[must_use]
    pub fn new(request_id: u32, body: RequestBody) -> Self {
        Self { request_id, body }
    }

    /// The request as a JSON value.
    ///
    /// # Errors
    /// Only if serialization fails, which the request types never do.
    pub fn to_value(&self) -> Result<Value, serde_json::Error> {
        serde_json::to_value(self)
    }

    /// The request as compact JSON bytes.
    ///
    /// # Errors
    /// Only if serialization fails, which the request types never do.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// The request as a length-prefixed frame.
    ///
    /// # Errors
    /// [`EncodeError`] when serialization fails or the frame would exceed the maximum.
    pub fn to_frame(&self) -> Result<Vec<u8>, EncodeError> {
        Ok(encode_frame(&self.to_json()?)?)
    }
}

impl Serialize for Request {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let payload = self.body.payload_value().map_err(S::Error::custom)?;
        let mut st = serializer.serialize_struct("Request", 4)?;
        st.serialize_field("protocol_version", &PROTOCOL_VERSION)?;
        st.serialize_field("request_id", &self.request_id)?;
        st.serialize_field("action", self.body.action().as_str())?;
        st.serialize_field("payload", &payload)?;
        st.end()
    }
}

/// A response: success with a result, or failure with an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// `ok: true`.
    Success {
        /// The request being answered.
        request_id: u32,
        /// The untyped result; see [`ResultBody::from_value`].
        result: Value,
    },
    /// `ok: false`.
    Failure {
        /// The request being answered.
        request_id: u32,
        /// What went wrong.
        error: ErrorBody,
    },
}

impl Response {
    /// A success response carrying `result`.
    ///
    /// # Errors
    /// Only if serialization fails, which the result types never do.
    pub fn success(request_id: u32, result: &ResultBody) -> Result<Self, serde_json::Error> {
        Ok(Self::Success {
            request_id,
            result: result.to_value()?,
        })
    }

    /// A failure response carrying `error`.
    #[must_use]
    pub fn failure(request_id: u32, error: ErrorBody) -> Self {
        Self::Failure { request_id, error }
    }

    /// The request being answered.
    #[must_use]
    pub fn request_id(&self) -> u32 {
        match self {
            Self::Success { request_id, .. } | Self::Failure { request_id, .. } => *request_id,
        }
    }

    /// The result of a success response.
    #[must_use]
    pub fn result(&self) -> Option<&Value> {
        match self {
            Self::Success { result, .. } => Some(result),
            Self::Failure { .. } => None,
        }
    }

    /// The error of a failure response.
    #[must_use]
    pub fn error(&self) -> Option<&ErrorBody> {
        match self {
            Self::Success { .. } => None,
            Self::Failure { error, .. } => Some(error),
        }
    }

    /// The response as a length-prefixed frame.
    ///
    /// # Errors
    /// [`EncodeError`] when serialization fails or the frame would exceed the maximum.
    pub fn to_frame(&self) -> Result<Vec<u8>, EncodeError> {
        Ok(encode_frame(&serde_json::to_vec(self)?)?)
    }
}

impl Serialize for Response {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut st = serializer.serialize_struct("Response", 4)?;
        st.serialize_field("type", "response")?;
        st.serialize_field("request_id", &self.request_id())?;
        match self {
            Self::Success { result, .. } => {
                st.serialize_field("ok", &true)?;
                st.serialize_field("result", result)?;
            }
            Self::Failure { error, .. } => {
                st.serialize_field("ok", &false)?;
                st.serialize_field("error", error)?;
            }
        }
        st.end()
    }
}

impl Event {
    /// The event as a length-prefixed frame.
    ///
    /// # Errors
    /// [`EncodeError`] when serialization fails or the frame would exceed the maximum.
    pub fn to_frame(&self) -> Result<Vec<u8>, EncodeError> {
        Ok(encode_frame(&serde_json::to_vec(self)?)?)
    }
}

impl Serialize for Event {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let data = self.body.data_value().map_err(S::Error::custom)?;
        let mut st = serializer.serialize_struct("Event", 4)?;
        st.serialize_field("type", "event")?;
        st.serialize_field("seq", &self.seq)?;
        st.serialize_field("event", self.body.event_name())?;
        st.serialize_field("data", &data)?;
        st.end()
    }
}

/// Anything the agent sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMessage {
    /// A response to a request.
    Response(Response),
    /// A sequenced event.
    Event(Event),
    /// An event name this build does not know; clients ignore it.
    UnknownEvent {
        /// The event's sequence number.
        seq: u64,
        /// The unrecognized name.
        event: String,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireMessage {
    Response(WireResponse),
    Event(WireEvent),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireResponse {
    request_id: u32,
    ok: bool,
    #[serde(default, deserialize_with = "present_or_null")]
    result: Option<Option<Value>>,
    #[serde(default, deserialize_with = "present_or_null")]
    error: Option<Option<ErrorBody>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEvent {
    seq: u64,
    event: String,
    data: Value,
}

fn invalid(reason: &'static str) -> ParseError {
    ParseError::InvalidPayload {
        request_id: None,
        field: None,
        reason,
    }
}

fn response_from_wire(wire: WireResponse) -> Result<Response, ParseError> {
    let request_id = wire.request_id;
    match (wire.ok, wire.result, wire.error) {
        (true, Some(Some(result)), None) if result.is_object() => {
            Ok(Response::Success { request_id, result })
        }
        (false, None, Some(Some(error))) => Ok(Response::Failure { request_id, error }),
        _ => Err(invalid("inconsistent_response")),
    }
}

fn event_from_wire(wire: WireEvent) -> Result<ServerMessage, ParseError> {
    if !EventBody::NAMES.contains(&wire.event.as_str()) {
        return Ok(ServerMessage::UnknownEvent {
            seq: wire.seq,
            event: wire.event,
        });
    }
    let body = EventBody::from_parts(&wire.event, wire.data).map_err(|err| {
        let (field, reason) = describe(&err);
        ParseError::InvalidPayload {
            request_id: None,
            field,
            reason,
        }
    })?;
    Ok(ServerMessage::Event(Event {
        seq: wire.seq,
        body,
    }))
}

/// Parses one frame body sent by the agent (tolerant of unknown result fields, event
/// names and error codes).
///
/// # Errors
/// [`ParseError`] when the body is not a well-formed response or event.
pub fn parse_server_message(body: &[u8]) -> Result<ServerMessage, ParseError> {
    let wire: WireMessage =
        serde_json::from_slice(body).map_err(|err| classify_body_error(body, &err))?;
    match wire {
        WireMessage::Response(response) => {
            response_from_wire(response).map(ServerMessage::Response)
        }
        WireMessage::Event(event) => event_from_wire(event),
    }
}
