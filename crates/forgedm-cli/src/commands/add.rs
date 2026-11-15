//! `forgedm add <url|-> [--dir] [--file-name] [--sha256] [--watch] [--no-start]`.
//!
//! The URL may carry a secret token, so `-` reads it from standard input and nothing here
//! ever prints it: the agent seals it and only a redacted form comes back.

use std::io::Read;

use forgedm_protocol::{EnqueueUrlRequest, MAX_URL_BYTES, RequestBody, ResultBody};

use crate::cli::AddArgs;
use crate::client::Target;
use crate::commands::watch;
use crate::error::CliError;
use crate::output;

/// The URL argument that means "read it from standard input".
const STDIN_MARKER: &str = "-";

/// Queues the download and, with `--watch`, follows it to the end.
pub async fn run(target: &Target, args: AddArgs) -> Result<(), CliError> {
    let url = read_url(&args.url, &mut std::io::stdin().lock())?;
    let (mut client, hello) = if args.no_start {
        target.connect().await?
    } else {
        target.connect_or_start().await?
    };
    let mut request = EnqueueUrlRequest::new(url);
    request.target_dir = args
        .dir
        .as_ref()
        .map(|dir| dir.to_string_lossy().into_owned());
    request.file_name = args.file_name;
    request.expected_sha256 = args.sha256;
    let queued = match client.call(RequestBody::EnqueueUrl(request)).await? {
        ResultBody::EnqueueUrl(result) => result,
        _ => return Err(CliError::Unexpected),
    };
    output::added(queued.download_id, &queued.file_name);
    if args.watch {
        watch::follow(&mut client, &hello, queued.download_id).await
    } else {
        Ok(())
    }
}

/// The URL from the command line, or the first line of `stdin` when the argument is `-`.
///
/// # Errors
/// [`CliError::Input`] when standard input holds no URL.
fn read_url(argument: &str, stdin: &mut impl Read) -> Result<String, CliError> {
    if argument != STDIN_MARKER {
        return Ok(argument.to_owned());
    }
    // One byte more than the longest URL the agent accepts: anything longer is refused by
    // the agent with `invalid_url`, without reading an unbounded stream here.
    let limit = u64::try_from(MAX_URL_BYTES + 1).unwrap_or(u64::MAX);
    let mut text = String::new();
    stdin
        .take(limit)
        .read_to_string(&mut text)
        .map_err(|_| CliError::Input("cannot read the URL from standard input"))?;
    let url = text.lines().next().unwrap_or_default().trim();
    if url.is_empty() {
        return Err(CliError::Input("standard input contains no URL"));
    }
    Ok(url.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_argument_is_used_as_is() {
        let mut empty = std::io::empty();
        assert_eq!(read_url("https://a/b", &mut empty).unwrap(), "https://a/b");
    }

    #[test]
    fn a_dash_reads_the_first_line_of_stdin() {
        let mut input = &b"  https://a/b?token=1  \r\nsecond line\n"[..];
        assert_eq!(read_url("-", &mut input).unwrap(), "https://a/b?token=1");
    }

    #[test]
    fn empty_stdin_is_an_input_error() {
        let mut input = &b"\n\n"[..];
        assert!(matches!(read_url("-", &mut input), Err(CliError::Input(_))));
        let mut nothing = std::io::empty();
        assert!(matches!(
            read_url("-", &mut nothing),
            Err(CliError::Input(_))
        ));
    }

    #[test]
    fn stdin_is_read_up_to_the_url_limit_only() {
        let long = format!("https://a/{}", "x".repeat(MAX_URL_BYTES * 2));
        let mut input = long.as_bytes();
        let url = read_url("-", &mut input).unwrap();
        assert_eq!(url.len(), MAX_URL_BYTES + 1);
    }
}
