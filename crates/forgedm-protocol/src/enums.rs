//! Shared string-valued enums (Appendix A.3 to A.7). Wire strings are snake_case
//! (actions are PascalCase) and covered by the `state_and_error_code_strings_stable` test.

use serde::{Deserialize, Serialize};

/// Declares a string-valued enum with a stable `as_str` and an `ALL` list.
///
/// The `tolerant` form adds a trailing `Unknown` variant that absorbs values from a newer
/// agent (clients treat unknown states and codes generically, Appendix A.1). `Unknown`
/// is not part of `ALL`.
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $($(#[$vmeta:meta])* $variant:ident = $wire:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vmeta])* #[serde(rename = $wire)] $variant),+
        }

        impl $name {
            /// Every known value, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The exact wire string.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }
        }
    };
    (
        tolerant
        $(#[$meta:meta])*
        $name:ident { $($(#[$vmeta:meta])* $variant:ident = $wire:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vmeta])* #[serde(rename = $wire)] $variant,)+
            /// A value this build does not know (sent by a newer peer).
            #[serde(rename = "unknown", other)]
            Unknown,
        }

        impl $name {
            /// Every known value, in declaration order (excludes `Unknown`).
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The exact wire string (`unknown` for an unrecognized value).
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire,)+ Self::Unknown => "unknown" }
            }
        }
    };
}

wire_enum! {
    /// Every action the agent serves (Appendix A.3). Wire names are PascalCase.
    Action {
        Handshake = "Handshake",
        Ping = "Ping",
        GetAgentStatus = "GetAgentStatus",
        GetSnapshot = "GetSnapshot",
        Subscribe = "Subscribe",
        Unsubscribe = "Unsubscribe",
        ListDownloads = "ListDownloads",
        ShutdownAgentSafely = "ShutdownAgentSafely",
        EnqueueUrl = "EnqueueUrl",
        GetDownload = "GetDownload",
        Pause = "Pause",
        Resume = "Resume",
        Cancel = "Cancel",
        Retry = "Retry",
        Remove = "Remove",
        PauseAll = "PauseAll",
        ResumeAll = "ResumeAll",
        UpdateLimits = "UpdateLimits",
        UpdatePriority = "UpdatePriority",
    }
}

/// Error returned when a string is not a known [`Action`] name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown action")]
pub struct UnknownActionError;

impl std::str::FromStr for Action {
    type Err = UnknownActionError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|a| a.as_str() == name)
            .ok_or(UnknownActionError)
    }
}

wire_enum! {
    /// Kind of client in the handshake; also recorded as a download's `source`.
    ClientKind { Cli = "cli", Desktop = "desktop" }
}

wire_enum! {
    /// Download scheduling priority (stored 2/1/0).
    Priority { High = "high", Normal = "normal", Low = "low" }
}

wire_enum! {
    /// Agent lifecycle state.
    AgentState { Starting = "starting", Ready = "ready", ShuttingDown = "shutting_down" }
}

wire_enum! {
    /// Where the agent's proxy configuration came from.
    ProxySource { None = "none", System = "system", Environment = "environment" }
}

wire_enum! {
    /// How reliably a download can be resumed.
    ResumeSupport {
        Reliable = "reliable",
        BestEffort = "best_effort",
        NotResumable = "not_resumable",
        Unknown = "unknown",
    }
}

wire_enum! {
    /// How the body is being transferred.
    TransferMode {
        Segmented = "segmented",
        SingleRanged = "single_ranged",
        SingleRangedUnknownLength = "single_ranged_unknown_length",
        SingleNonresumable = "single_nonresumable",
        SingleUnknownLength = "single_unknown_length",
        ZeroLength = "zero_length",
    }
}

wire_enum! {
    /// Mark-of-the-Web outcome for a finished file.
    MotwStatus {
        Pending = "pending",
        Marked = "marked",
        MarkedDirect = "marked_direct",
        UnsupportedFs = "unsupported_fs",
        Failed = "failed",
    }
}

wire_enum! {
    /// Strength of an HTTP validator.
    ValidatorStrength { Strong = "strong", Weak = "weak", None = "none" }
}

wire_enum! {
    /// Checksum verification status.
    ChecksumStatus {
        None = "none",
        Pending = "pending",
        Match = "match",
        Mismatch = "mismatch",
    }
}

wire_enum! {
    /// Why a subscriber must resynchronize.
    ResyncReason {
        Lagged = "lagged",
        InstanceChanged = "instance_changed",
        Evicted = "evicted",
    }
}

wire_enum! {
    /// `invalid_url` detail reasons.
    InvalidUrlReason {
        Parse = "parse",
        Scheme = "scheme",
        TooLong = "too_long",
        Userinfo = "userinfo",
        Host = "host",
        ControlChars = "control_chars",
    }
}

wire_enum! {
    /// `invalid_destination` detail reasons.
    InvalidDestinationReason {
        NotAbsolute = "not_absolute",
        NotFound = "not_found",
        NotDirectory = "not_directory",
        DevicePath = "device_path",
        UncUnsupported = "unc_unsupported",
        ReparsePoint = "reparse_point",
    }
}

wire_enum! {
    tolerant
    /// The 14 download states (Appendix A.6).
    DownloadState {
        Queued = "queued",
        Probing = "probing",
        Downloading = "downloading",
        Pausing = "pausing",
        Paused = "paused",
        RetryWaiting = "retry_waiting",
        Verifying = "verifying",
        Finalizing = "finalizing",
        Completed = "completed",
        Failed = "failed",
        Cancelled = "cancelled",
        Interrupted = "interrupted",
        NeedsCredentials = "needs_credentials",
        NeedsUrlRefresh = "needs_url_refresh",
    }
}

impl DownloadState {
    /// Whether the state is terminal for the automatic flow (`completed`, `failed`,
    /// `cancelled`).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

wire_enum! {
    tolerant
    /// Response error codes (Appendix A.7). Unknown codes from a newer agent decode to
    /// `Unknown`; honour the accompanying `retryable` flag.
    ErrorCode {
        HandshakeRequired = "handshake_required",
        MalformedFrame = "malformed_frame",
        FrameTooLarge = "frame_too_large",
        UnsupportedProtocolVersion = "unsupported_protocol_version",
        UnknownAction = "unknown_action",
        InvalidPayload = "invalid_payload",
        NotFound = "not_found",
        InvalidState = "invalid_state",
        NotResumable = "not_resumable",
        InvalidUrl = "invalid_url",
        InvalidDestination = "invalid_destination",
        InvalidFileName = "invalid_file_name",
        InvalidChecksum = "invalid_checksum",
        LimitExceeded = "limit_exceeded",
        NotImplemented = "not_implemented",
        Internal = "internal",
        Busy = "busy",
        ShuttingDown = "shutting_down",
        ResyncRequired = "resync_required",
        StorageUnavailable = "storage_unavailable",
    }
}

wire_enum! {
    tolerant
    /// Download failure codes carried in `last_error.code` (Appendix A.7, including
    /// PCR-01 `part_file_invalid` and PCR-02 `path_escape`).
    DownloadErrorCode {
        NetworkTimeout = "network_timeout",
        NetworkConnect = "network_connect",
        NetworkReset = "network_reset",
        BodyIncomplete = "body_incomplete",
        DnsFailure = "dns_failure",
        RateLimited = "rate_limited",
        HttpStatus = "http_status",
        AuthRequired = "auth_required",
        UrlExpired = "url_expired",
        TlsFailure = "tls_failure",
        TooManyRedirects = "too_many_redirects",
        RedirectLoop = "redirect_loop",
        RedirectDowngrade = "redirect_downgrade",
        RedirectBlocked = "redirect_blocked",
        RangeProtocolViolation = "range_protocol_violation",
        UnsupportedContentEncoding = "unsupported_content_encoding",
        RemoteChanged = "remote_changed",
        SizeMismatch = "size_mismatch",
        ChecksumMismatch = "checksum_mismatch",
        DiskFull = "disk_full",
        DestinationUnavailable = "destination_unavailable",
        DiskIo = "disk_io",
        PathTooLong = "path_too_long",
        FileSystemLimit = "file_system_limit",
        SecretsUnavailable = "secrets_unavailable",
        BlockedByPolicy = "blocked_by_policy",
        BlockedByAntivirus = "blocked_by_antivirus",
        Internal = "internal",
        PathEscape = "path_escape",
        FinalizeLocked = "finalize_locked",
        RetryBudgetExhausted = "retry_budget_exhausted",
        PartFileInvalid = "part_file_invalid",
    }
}

impl ErrorCode {
    /// Whether the client may retry the same request later (Appendix A.7). Unknown codes
    /// are not retryable by default; honour the response's own `retryable` flag instead.
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::Busy | Self::ShuttingDown | Self::ResyncRequired | Self::StorageUnavailable
        )
    }

    /// Generic, secret-free message for this code.
    #[must_use]
    pub const fn default_message(self) -> &'static str {
        match self {
            Self::HandshakeRequired => "handshake required before any other action",
            Self::MalformedFrame => "malformed frame",
            Self::FrameTooLarge => "frame too large",
            Self::UnsupportedProtocolVersion => "protocol version not supported",
            Self::UnknownAction => "unknown action",
            Self::InvalidPayload => "invalid request payload",
            Self::NotFound => "download not found",
            Self::InvalidState => "action not allowed in the current state",
            Self::NotResumable => "download is not resumable",
            Self::InvalidUrl => "url rejected",
            Self::InvalidDestination => "destination rejected",
            Self::InvalidFileName => "file name rejected",
            Self::InvalidChecksum => "checksum rejected",
            Self::LimitExceeded => "limit exceeded",
            Self::NotImplemented => "not implemented",
            Self::Internal => "internal error",
            Self::Busy => "agent is busy, retry shortly",
            Self::ShuttingDown => "agent is shutting down",
            Self::ResyncRequired => "resynchronization required",
            Self::StorageUnavailable => "storage unavailable",
            Self::Unknown => "unknown error",
        }
    }

    /// Whether the agent closes the connection after sending this error.
    #[must_use]
    pub const fn closes_connection(self) -> bool {
        matches!(
            self,
            Self::HandshakeRequired
                | Self::MalformedFrame
                | Self::FrameTooLarge
                | Self::UnsupportedProtocolVersion
        )
    }
}
