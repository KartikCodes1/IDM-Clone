//! Checked integer and enum conversions between Rust types and SQLite columns.

use crate::error::StorageError;

/// `u64` to a SQLite integer.
pub(crate) fn to_i64(field: &'static str, value: u64) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(|_| StorageError::InvalidValue { field })
}

/// A SQLite integer to `u64`.
pub(crate) fn to_u64(field: &'static str, value: i64) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::InvalidValue { field })
}

/// A SQLite integer to `u32`.
pub(crate) fn to_u32(field: &'static str, value: i64) -> Result<u32, StorageError> {
    u32::try_from(value).map_err(|_| StorageError::InvalidValue { field })
}

/// Parse a stored wire string against an enum's `ALL` list. Unknown values are rejected
/// rather than mapped to a catch-all, so corrupt or newer data is never silently accepted.
pub(crate) fn parse_enum<T: Copy>(
    column: &'static str,
    value: &str,
    all: &[T],
    name: fn(T) -> &'static str,
) -> Result<T, StorageError> {
    all.iter()
        .copied()
        .find(|candidate| name(*candidate) == value)
        .ok_or_else(|| StorageError::UnknownEnumValue {
            column,
            value: value.to_owned(),
        })
}

/// As [`parse_enum`] for nullable columns.
pub(crate) fn parse_enum_opt<T: Copy>(
    column: &'static str,
    value: Option<&str>,
    all: &[T],
    name: fn(T) -> &'static str,
) -> Result<Option<T>, StorageError> {
    value.map(|v| parse_enum(column, v, all, name)).transpose()
}
