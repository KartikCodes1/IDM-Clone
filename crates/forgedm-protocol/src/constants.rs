//! Protocol constants (PHASE_0_1_PLAN Appendix A.1, A.2, A.3, A.9).

use std::time::Duration;

/// Protocol major version carried in every request.
pub const PROTOCOL_VERSION: u32 = 1;
/// Protocol minor version advertised in the handshake result.
pub const PROTOCOL_MINOR: u32 = 0;

/// Maximum frame body size in bytes (1 MiB).
pub const MAX_FRAME: u32 = 1_048_576;
/// Size of the little-endian length prefix.
pub const FRAME_HEADER_LEN: usize = 4;

/// Largest integer a JavaScript client can represent exactly (2^53 - 1).
pub const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// Handshake must arrive within this long of the connection opening.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// A frame body must complete within this long of its header.
pub const FRAME_BODY_TIMEOUT: Duration = Duration::from_secs(10);
/// Idle timeout for connections that are not subscribed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Maximum `url` length in bytes.
pub const MAX_URL_BYTES: usize = 8192;
/// Maximum `file_name` length in UTF-16 code units on the wire.
pub const MAX_FILE_NAME_UTF16: usize = 255;
/// Length of `expected_sha256` in hex characters.
pub const SHA256_HEX_LEN: usize = 64;
/// Maximum `client_name` length in characters.
pub const MAX_CLIENT_NAME_CHARS: usize = 64;
/// Maximum `client_version` length in characters.
pub const MAX_CLIENT_VERSION_CHARS: usize = 32;

/// Largest page `ListDownloads` accepts.
pub const LIST_PAGE_MAX: u32 = 500;
/// Largest number of downloads in a snapshot.
pub const SNAPSHOT_MAX: usize = 1000;
/// Largest number of ids in a `QueueChanged` order.
pub const QUEUE_ORDER_MAX: usize = 1000;
/// Largest number of redacted events in a `DownloadDetail`.
pub const RECENT_EVENTS_MAX: usize = 20;

/// Smallest per-download or global connection count.
pub const MIN_CONNECTIONS: u32 = 1;
/// Largest per-download or global connection count.
pub const MAX_CONNECTIONS: u32 = 16;
/// Smallest `rate_limit_bps` when a limit is set.
pub const MIN_RATE_LIMIT_BPS: u64 = 1024;
/// Smallest `max_active_downloads`.
pub const MIN_ACTIVE_DOWNLOADS: u32 = 1;
/// Largest `max_active_downloads`.
pub const MAX_ACTIVE_DOWNLOADS: u32 = 10;
