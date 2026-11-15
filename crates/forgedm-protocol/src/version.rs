//! Handshake version negotiation (Appendix A.1, A.2).

use crate::constants::PROTOCOL_VERSION;
use crate::error::ErrorBody;

/// The client's `[min_protocol, max_protocol]` range does not include major 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("client protocol range {client_min}..={client_max} does not include 1")]
pub struct UnsupportedVersion {
    /// The client's lowest supported major.
    pub client_min: u32,
    /// The client's highest supported major.
    pub client_max: u32,
}

impl UnsupportedVersion {
    /// The `unsupported_protocol_version` response error (`server_min = server_max = 1`).
    #[must_use]
    pub fn to_error_body(self) -> ErrorBody {
        ErrorBody::unsupported_protocol_version()
    }
}

/// Picks the protocol major for a connection: 1 if the client's range includes it.
///
/// # Errors
/// [`UnsupportedVersion`] when the range excludes 1 or is inverted (`min > max`).
pub fn negotiate_version(client_min: u32, client_max: u32) -> Result<u32, UnsupportedVersion> {
    if client_min <= PROTOCOL_VERSION && PROTOCOL_VERSION <= client_max {
        Ok(PROTOCOL_VERSION)
    } else {
        Err(UnsupportedVersion {
            client_min,
            client_max,
        })
    }
}
