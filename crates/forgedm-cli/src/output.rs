//! The CLI's only console output point: results to stdout, problems to stderr.
//!
//! The workspace denies `print_stdout` and `print_stderr`; this module is the designated
//! exception because a command-line client's job is to print.
//!
//! Writes go through locked handles and ignore I/O errors, so a closed pipe (for example
//! `forgedm list | head`) ends output quietly instead of panicking.

use std::io::Write;

use forgedm_protocol::{
    AgentStatusInfo, DownloadDetail, DownloadState, DownloadSummary, HandshakeResult, ProgressItem,
};
use uuid::Uuid;

/// Prints one line to standard output.
pub fn line(text: &str) {
    let _ = writeln!(std::io::stdout().lock(), "{text}");
}

/// Prints one error line to standard error.
pub fn error_line(message: &str) {
    let _ = writeln!(std::io::stderr().lock(), "forgedm: {message}");
}

/// Prints a clap parse error or help text the way clap formats it.
pub fn clap_error(err: &clap::Error) {
    // clap picks stdout (help, version) or stderr (usage errors) itself.
    let _ = err.print();
}

/// Replaces control, bidi and other format characters with U+FFFD so text supplied by the
/// agent cannot drive the terminal (escape sequences, carriage returns, row spoofing).
pub fn sanitize_for_terminal(text: &str) -> String {
    text.chars()
        .map(|c| if is_unsafe_char(c) { '\u{FFFD}' } else { c })
        .collect()
}

fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

/// Prints the agent status block.
pub fn status(hello: &HandshakeResult, info: &AgentStatusInfo) {
    line(&format!(
        "agent {} (pid {})",
        info.agent_version, info.agent_pid
    ));
    line(&format!(
        "protocol {}.{}",
        hello.protocol_version, hello.protocol_minor
    ));
    line(&format!(
        "state {} (up {} s)",
        enum_text(serde_json::to_value(info.state)),
        info.uptime_ms / 1000
    ));
    line(&format!(
        "downloads {} active, {} queued, {} B/s",
        info.active, info.queued, info.total_speed_bps
    ));
    line(&format!(
        "proxy {}",
        enum_text(serde_json::to_value(info.proxy_source))
    ));
    line(&format!(
        "data dir {}",
        sanitize_for_terminal(&info.data_dir)
    ));
}

/// Prints the download list as a plain table, or a note when it is empty.
pub fn download_table(items: &[DownloadSummary]) {
    if items.is_empty() {
        line("No downloads.");
        return;
    }
    for item in items {
        line(&format!(
            "{}  {:<12}  {:>12}  {}",
            item.id,
            enum_text(serde_json::to_value(item.state)),
            progress_text(item),
            sanitize_for_terminal(&item.display_name)
        ));
    }
}

/// `accepted/total` bytes, or `accepted/?` when the size is unknown.
fn progress_text(item: &DownloadSummary) -> String {
    match item.total_bytes {
        Some(total) => format!("{}/{}", item.accepted_bytes, total),
        None => format!("{}/?", item.accepted_bytes),
    }
}

/// The snake_case wire text of a protocol enum value, `unknown` when it has none.
fn enum_text(value: Result<serde_json::Value, serde_json::Error>) -> String {
    value
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Bytes per KiB, MiB and GiB.
const KIB: u64 = 1024;
const MIB: u64 = KIB * 1024;
const GIB: u64 = MIB * 1024;

/// `1.43 MiB`, `812 B`: two decimals above 1 KiB, integer arithmetic only.
pub fn format_bytes(bytes: u64) -> String {
    let (unit, name) = match bytes {
        b if b >= GIB => (GIB, "GiB"),
        b if b >= MIB => (MIB, "MiB"),
        b if b >= KIB => (KIB, "KiB"),
        _ => return format!("{bytes} B"),
    };
    let whole = bytes / unit;
    let hundredths = u128::from(bytes % unit) * 100 / u128::from(unit);
    format!("{whole}.{hundredths:02} {name}")
}

/// `1.43 MiB/s`, or `-- B/s` while the speed is unknown.
pub fn format_speed(bytes_per_second: Option<u64>) -> String {
    bytes_per_second.map_or_else(
        || "-- B/s".to_owned(),
        |bps| format!("{}/s", format_bytes(bps)),
    )
}

/// `ETA 1m05s`, `ETA 12s`, or `ETA --` while it is unknown.
pub fn format_eta(eta_ms: Option<u64>) -> String {
    let Some(ms) = eta_ms else {
        return "ETA --".to_owned();
    };
    let seconds = ms.div_ceil(1000);
    match (seconds / 3600, seconds / 60 % 60, seconds % 60) {
        (0, 0, s) => format!("ETA {s}s"),
        (0, m, s) => format!("ETA {m}m{s:02}s"),
        (h, m, s) => format!("ETA {h}h{m:02}m{s:02}s"),
    }
}

/// The progress line shown while a download runs: true bytes against the real total, speed
/// and ETA. Unknown totals show no percentage.
pub fn progress_line(item: &ProgressItem) -> String {
    let speed = format_speed(item.speed_bps);
    let eta = format_eta(item.eta_ms);
    match item.total_bytes.filter(|total| *total > 0) {
        Some(total) => {
            let percent = u128::from(item.accepted_bytes) * 100 / u128::from(total);
            format!(
                "downloading {} / {} ({percent}%)  {speed}  {eta}",
                format_bytes(item.accepted_bytes),
                format_bytes(total)
            )
        }
        None => format!(
            "downloading {}  {speed}  {eta}",
            format_bytes(item.accepted_bytes)
        ),
    }
}

/// Prints the download id and name right after `add`.
pub fn added(id: Uuid, file_name: &str) {
    line(&format!("added {id} {}", sanitize_for_terminal(file_name)));
}

/// Prints a state change while watching.
pub fn state_change(state: DownloadState) {
    line(&format!("-> {}", enum_text(serde_json::to_value(state))));
}

/// Prints a progress line.
pub fn progress(item: &ProgressItem) {
    line(&progress_line(item));
}

/// Notes that an interrupted download starts over (it cannot resume).
pub fn restarting_from_zero() {
    line("interrupted earlier; this download cannot resume, so it restarts from 0");
}

/// Prints the result of a completed download: where it is and its verified digest.
pub fn completed(detail: &DownloadDetail) {
    let summary = &detail.summary;
    line(&format!(
        "completed {}",
        sanitize_for_terminal(summary.final_path.as_deref().unwrap_or("(unknown path)"))
    ));
    if let Some(sha) = &detail.checksum.actual {
        line(&format!("sha256 {}", sanitize_for_terminal(sha)));
    }
    if detail.length_verified == Some(false) {
        line("note: the server declared no length, so completeness is not proven by size");
    }
}

/// The text describing how a watched download ended badly.
pub fn ending_text(summary: &DownloadSummary) -> String {
    let state = enum_text(serde_json::to_value(summary.state));
    match &summary.last_error {
        Some(error) => format!(
            "{state} ({}): {}",
            enum_text(serde_json::to_value(error.code)),
            sanitize_for_terminal(&error.message)
        ),
        None => match summary.state_reason.as_deref() {
            Some(reason) => format!("{state} ({})", sanitize_for_terminal(reason)),
            None => state,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_escape_cr_and_bidi_controls() {
        let out = sanitize_for_terminal("a\x1b[2Jb\rc\u{202E}d\n");

        assert_eq!(out, "a\u{FFFD}[2Jb\u{FFFD}c\u{FFFD}d\u{FFFD}");
        assert!(!out.chars().any(char::is_control));
    }

    #[test]
    fn sanitize_keeps_ordinary_unicode() {
        assert_eq!(
            sanitize_for_terminal("\u{FC}ber 日本.zip"),
            "\u{FC}ber 日本.zip"
        );
    }
    #[test]
    fn bytes_use_binary_units_with_two_decimals() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.00 KiB");
        assert_eq!(format_bytes(3_000_000), "2.86 MiB");
        assert_eq!(
            format_bytes(5 * 1024 * 1024 * 1024 + 512 * 1024 * 1024),
            "5.50 GiB"
        );
        assert_eq!(format_bytes(u64::MAX), "17179869183.99 GiB");
    }

    #[test]
    fn speed_and_eta_cover_known_and_unknown_values() {
        assert_eq!(format_speed(Some(1_500_000)), "1.43 MiB/s");
        assert_eq!(format_speed(None), "-- B/s");
        assert_eq!(format_eta(None), "ETA --");
        assert_eq!(format_eta(Some(12_000)), "ETA 12s");
        assert_eq!(format_eta(Some(12_001)), "ETA 13s");
        assert_eq!(format_eta(Some(65_000)), "ETA 1m05s");
        assert_eq!(format_eta(Some(3_725_000)), "ETA 1h02m05s");
    }

    #[test]
    fn progress_lines_show_percentage_only_for_a_known_total() {
        let mut item = ProgressItem {
            id: Uuid::nil(),
            accepted_bytes: 1_500_000,
            committed_bytes: 1_000_000,
            total_bytes: Some(3_000_000),
            speed_bps: Some(1_500_000),
            eta_ms: Some(1_000),
            active_connections: 1,
        };
        assert_eq!(
            progress_line(&item),
            "downloading 1.43 MiB / 2.86 MiB (50%)  1.43 MiB/s  ETA 1s"
        );
        item.total_bytes = None;
        assert_eq!(
            progress_line(&item),
            "downloading 1.43 MiB  1.43 MiB/s  ETA 1s"
        );
        item.total_bytes = Some(0);
        assert!(!progress_line(&item).contains('%'));
    }
}
