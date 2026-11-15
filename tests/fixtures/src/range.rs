//! Single-range `Range` header evaluation (RFC 9110 section 14).

/// The outcome of evaluating a `Range` header against a representation size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeOutcome {
    /// Absent, unsupported (multi-range, other unit) or malformed: serve the full 200.
    Full,
    /// A satisfiable range, inclusive on both ends.
    Partial {
        /// First byte.
        start: u64,
        /// Last byte (inclusive).
        end: u64,
    },
    /// Valid syntax but nothing to serve: 416 with `Content-Range: bytes */size`.
    Unsatisfiable,
}

fn parse_number(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // An all-digit value too large for u64 saturates: it is beyond any size.
    Some(s.parse::<u64>().unwrap_or(u64::MAX))
}

/// Evaluates `header` (the raw `Range` value) for a representation of `size` bytes.
#[must_use]
pub fn evaluate(header: &str, size: u64) -> RangeOutcome {
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return RangeOutcome::Full;
    };
    if spec.contains(',') {
        return RangeOutcome::Full;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return RangeOutcome::Full;
    };
    match (parse_number(first.trim()), parse_number(last.trim())) {
        (None, Some(suffix)) => suffix_range(suffix, size),
        (Some(start), None) => bounded(start, size.saturating_sub(1), size),
        (Some(start), Some(end)) if start <= end => {
            bounded(start, end.min(size.saturating_sub(1)), size)
        }
        _ => RangeOutcome::Full,
    }
}

fn suffix_range(suffix: u64, size: u64) -> RangeOutcome {
    if suffix == 0 || size == 0 {
        return RangeOutcome::Unsatisfiable;
    }
    RangeOutcome::Partial {
        start: size.saturating_sub(suffix),
        end: size - 1,
    }
}

fn bounded(start: u64, end: u64, size: u64) -> RangeOutcome {
    if start >= size {
        RangeOutcome::Unsatisfiable
    } else {
        RangeOutcome::Partial { start, end }
    }
}

#[cfg(test)]
mod tests {
    use super::{RangeOutcome as R, evaluate};

    #[test]
    fn evaluates_edge_cases() {
        let cases: &[(&str, u64, R)] = &[
            ("bytes=0-9", 100, R::Partial { start: 0, end: 9 }),
            ("bytes=90-", 100, R::Partial { start: 90, end: 99 }),
            ("bytes=90-500", 100, R::Partial { start: 90, end: 99 }),
            ("bytes=-10", 100, R::Partial { start: 90, end: 99 }),
            ("bytes=-500", 100, R::Partial { start: 0, end: 99 }),
            ("bytes=-0", 100, R::Unsatisfiable),
            ("bytes=0-", 0, R::Unsatisfiable),
            ("bytes=-5", 0, R::Unsatisfiable),
            ("bytes=100-", 100, R::Unsatisfiable),
            ("bytes=5-2", 100, R::Full),
            ("bytes=0-1,5-6", 100, R::Full),
            ("items=0-1", 100, R::Full),
            ("bytes=abc", 100, R::Full),
            ("bytes=-", 100, R::Full),
            ("bytes=a-b", 100, R::Full),
            ("", 100, R::Full),
            ("bytes=99999999999999999999999-", 100, R::Unsatisfiable),
            (
                "bytes=0-99999999999999999999999",
                100,
                R::Partial { start: 0, end: 99 },
            ),
        ];
        for (header, size, want) in cases {
            assert_eq!(
                evaluate(header, *size),
                *want,
                "header {header:?} size {size}"
            );
        }
    }
}
