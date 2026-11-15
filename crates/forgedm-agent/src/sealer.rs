//! The DPAPI implementation of the storage `SecretSealer` port (ARCHITECTURE 7.5, ADR-012).
//!
//! Sealed format: `0x01 || CryptProtectData(plaintext, entropy)`. The optional entropy is
//! `ForgeDM/v1/<column>/<download_id>` (built by [`SealContext::entropy`]), so a blob copied
//! to another row or column fails to unseal. Scope is the current user; never the machine.
//! Failures are security events on the `forgedm::security` target and never include
//! plaintext.

use forgedm_platform::dpapi;
use forgedm_storage::{SealContext, SealError, SecretSealer};

/// Version byte that prefixes every sealed value.
const FORMAT_V1: u8 = 0x01;

/// Seals secrets with DPAPI for the current user.
#[derive(Debug, Default, Clone, Copy)]
pub struct DpapiSealer;

impl SecretSealer for DpapiSealer {
    fn seal(&self, plaintext: &[u8], ctx: &SealContext<'_>) -> Result<Vec<u8>, SealError> {
        let entropy = ctx.entropy();
        let blob = dpapi::protect(plaintext, entropy.as_bytes()).map_err(|err| {
            tracing::error!(
                target: "forgedm::security",
                column = ctx.column.as_str(),
                error = %err,
                "DPAPI seal failed"
            );
            SealError::Seal(err.to_string())
        })?;
        let mut sealed = Vec::with_capacity(blob.len() + 1);
        sealed.push(FORMAT_V1);
        sealed.extend_from_slice(&blob);
        Ok(sealed)
    }

    fn unseal(&self, sealed: &[u8], ctx: &SealContext<'_>) -> Result<Vec<u8>, SealError> {
        let Some((&FORMAT_V1, blob)) = sealed.split_first() else {
            tracing::error!(
                target: "forgedm::security",
                column = ctx.column.as_str(),
                "sealed value has an unknown format"
            );
            return Err(SealError::Unseal("unknown sealed format".to_owned()));
        };
        let entropy = ctx.entropy();
        dpapi::unprotect(blob, entropy.as_bytes())
            .map(|secret| secret.as_slice().to_vec())
            .map_err(|err| {
                tracing::error!(
                    target: "forgedm::security",
                    column = ctx.column.as_str(),
                    error = %err,
                    "DPAPI unseal failed"
                );
                SealError::Unseal(err.to_string())
            })
    }
}

#[cfg(test)]
mod tests {
    use forgedm_storage::SealColumn;

    use super::*;

    const ID_A: &str = "0192f3a0-0000-7000-8000-000000000001";
    const ID_B: &str = "0192f3a0-0000-7000-8000-000000000002";
    const CANARY: &[u8] = b"https://example.com/f?token=CANARY-7f3a";

    fn ctx(column: SealColumn, id: &str) -> SealContext<'_> {
        SealContext {
            column,
            download_id: id,
        }
    }

    #[test]
    fn roundtrip_with_version_byte_and_hidden_plaintext() {
        let sealer = DpapiSealer;
        let context = ctx(SealColumn::OriginalUrl, ID_A);

        let sealed = sealer.seal(CANARY, &context).unwrap();

        assert_eq!(sealed.first(), Some(&0x01));
        assert!(
            !sealed.windows(CANARY.len()).any(|w| w == CANARY),
            "ciphertext must not contain the plaintext"
        );
        assert_eq!(sealer.unseal(&sealed, &context).unwrap(), CANARY);
    }

    #[test]
    fn value_moved_to_another_row_or_column_fails_to_unseal() {
        let sealer = DpapiSealer;
        let sealed = sealer
            .seal(CANARY, &ctx(SealColumn::OriginalUrl, ID_A))
            .unwrap();

        let other_row = sealer.unseal(&sealed, &ctx(SealColumn::OriginalUrl, ID_B));
        let other_column = sealer.unseal(&sealed, &ctx(SealColumn::FinalUrl, ID_A));

        assert!(matches!(other_row, Err(SealError::Unseal(_))));
        assert!(matches!(other_column, Err(SealError::Unseal(_))));
    }

    #[test]
    fn malformed_blobs_are_typed_unseal_errors() {
        let sealer = DpapiSealer;
        let context = ctx(SealColumn::OriginalUrl, ID_A);
        let mut tampered = sealer.seal(CANARY, &context).unwrap();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xFF;

        for bad in [
            &b""[..],
            &b"\x02abc"[..],
            &b"\x01"[..],
            &b"\x01garbage"[..],
            &tampered[..],
        ] {
            assert!(
                matches!(sealer.unseal(bad, &context), Err(SealError::Unseal(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn error_text_never_contains_the_plaintext() {
        let sealer = DpapiSealer;
        let context = ctx(SealColumn::OriginalUrl, ID_A);
        let err = sealer.unseal(b"\x01garbage", &context).unwrap_err();
        assert!(!err.to_string().contains("CANARY"));
    }
}
