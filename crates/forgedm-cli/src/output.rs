//! The CLI's only console output point: results to stdout, problems to stderr.
//!
//! The workspace denies `print_stdout` and `print_stderr`; this module is the designated
//! exception because a command-line client's job is to print.
//!
//! Writes go through locked handles and ignore I/O errors, so a closed pipe (for example
//! `forgedm list | head`) ends output quietly instead of panicking.

use std::io::Write;

use forgedm_protocol::{AgentStatusInfo, DownloadSummary, HandshakeResult};

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
}
