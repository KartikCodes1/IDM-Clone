//! The secret-sealing port. `forgedm-agent` implements it over DPAPI; tests use a
//! deterministic sealer (ARCHITECTURE 7.5, ADR-012).

use std::fmt;

use uuid::Uuid;

/// Which sealed column a value belongs to. Part of the sealing context so a blob moved to
/// another column fails to unseal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealColumn {
    /// `downloads.original_url_sealed`.
    OriginalUrl,
    /// `downloads.final_url_sealed`.
    FinalUrl,
}

impl SealColumn {
    /// The column name used in the entropy string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OriginalUrl => "original_url_sealed",
            Self::FinalUrl => "final_url_sealed",
        }
    }
}

/// Binds a sealed value to its row and column.
#[derive(Debug, Clone, Copy)]
pub struct SealContext<'a> {
    /// Column the value is stored in.
    pub column: SealColumn,
    /// Row the value belongs to, as the hyphenated lowercase UUID stored in `downloads.id`.
    pub download_id: &'a str,
}

impl SealContext<'_> {
    /// The DPAPI optional entropy: `ForgeDM/v1/<column>/<download_id>`.
    #[must_use]
    pub fn entropy(&self) -> String {
        format!("ForgeDM/v1/{}/{}", self.column.as_str(), self.download_id)
    }
}

/// A sealer failure. Reasons are platform diagnostics and never contain plaintext.
#[derive(Debug, thiserror::Error)]
pub enum SealError {
    /// Sealing failed.
    #[error("seal failed: {0}")]
    Seal(String),
    /// Unsealing failed (tampered blob, wrong context, wrong user, reset password).
    #[error("unseal failed: {0}")]
    Unseal(String),
}

/// Seals and unseals secret column values. Implementations must be deterministic in
/// their binding to the context: a value sealed for one `(column, download_id)` must not
/// unseal under another.
pub trait SecretSealer: Send + Sync + fmt::Debug {
    /// Seal `plaintext` for `ctx`.
    ///
    /// # Errors
    /// [`SealError::Seal`] when the platform refuses.
    fn seal(&self, plaintext: &[u8], ctx: &SealContext<'_>) -> Result<Vec<u8>, SealError>;

    /// Unseal a blob produced by [`SecretSealer::seal`] for the same `ctx`.
    ///
    /// # Errors
    /// [`SealError::Unseal`] when the blob is malformed, tampered with or bound to another
    /// context.
    fn unseal(&self, sealed: &[u8], ctx: &SealContext<'_>) -> Result<Vec<u8>, SealError>;
}

/// A plaintext URL read back from a sealed column. `Debug` never prints it.
pub struct SecretUrl(String);

impl SecretUrl {
    /// Wrap a plaintext URL.
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    /// The plaintext. Callers must not log it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretUrl([redacted])")
    }
}

/// Render a row id the way it is stored and bound into the sealing context.
pub(crate) fn id_text(id: Uuid) -> String {
    id.hyphenated().to_string()
}
