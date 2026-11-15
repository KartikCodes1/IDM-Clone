//! `forgedm pause|resume|cancel|retry|remove <id>`: the control actions of PLAN A.3.
//!
//! None of them starts the agent: controlling a download only makes sense while the agent
//! that owns it is running. Each prints the state the agent reports. A repeated `remove` is
//! `not_found` on the wire and a success here (A.6).

use std::io::{BufRead, IsTerminal, Write};

use forgedm_protocol::{
    DownloadIdRequest, DownloadState, PauseRequest, RemoveRequest, RequestBody, ResultBody,
    RetryRequest,
};
use uuid::Uuid;

use crate::client::Target;
use crate::error::CliError;
use crate::output;

/// The wire code the agent answers a repeated `Remove` with.
const NOT_FOUND: &str = "not_found";

/// The state a control result carries.
fn state_of(result: ResultBody) -> Result<DownloadState, CliError> {
    match result {
        ResultBody::Pause(r)
        | ResultBody::Resume(r)
        | ResultBody::Cancel(r)
        | ResultBody::Retry(r) => Ok(r.state),
        _ => Err(CliError::Unexpected),
    }
}

async fn run_state(target: &Target, id: Uuid, body: RequestBody) -> Result<(), CliError> {
    let (mut client, _hello) = target.connect().await?;
    let state = state_of(client.call(body).await?)?;
    output::state_result(id, state);
    Ok(())
}

/// `forgedm pause <id> [--discard-progress]`.
pub async fn pause(target: &Target, id: Uuid, discard_progress: bool) -> Result<(), CliError> {
    let request = PauseRequest {
        download_id: id,
        discard_progress,
    };
    run_state(target, id, RequestBody::Pause(request)).await
}

/// `forgedm resume <id>`.
pub async fn resume(target: &Target, id: Uuid) -> Result<(), CliError> {
    let request = DownloadIdRequest { download_id: id };
    run_state(target, id, RequestBody::Resume(request)).await
}

/// `forgedm cancel <id>`.
pub async fn cancel(target: &Target, id: Uuid) -> Result<(), CliError> {
    let request = DownloadIdRequest { download_id: id };
    run_state(target, id, RequestBody::Cancel(request)).await
}

/// `forgedm retry <id> [--restart-from-zero]`.
pub async fn retry(target: &Target, id: Uuid, restart_from_zero: bool) -> Result<(), CliError> {
    let request = RetryRequest {
        download_id: id,
        restart_from_zero,
    };
    run_state(target, id, RequestBody::Retry(request)).await
}

/// `forgedm remove <id> [--delete-partial [--yes]]`.
///
/// Deleting the partial file needs `--yes` or an answer at a terminal; anything else is a
/// usage error (exit 2) before the agent is contacted.
pub async fn remove(
    target: &Target,
    id: Uuid,
    delete_partial: bool,
    yes: bool,
) -> Result<(), CliError> {
    if delete_partial {
        confirm_delete_partial(yes, std::io::stdin().is_terminal(), ask_at_terminal)?;
    }
    let (mut client, _hello) = target.connect().await?;
    let request = RemoveRequest {
        download_id: id,
        delete_partial_file: delete_partial,
    };
    match client.call(RequestBody::Remove(request)).await {
        Ok(ResultBody::Remove(_)) => output::line(&format!("removed {id}")),
        Ok(_) => return Err(CliError::Unexpected),
        Err(err) => match CliError::from(err) {
            CliError::Agent { code, .. } if code == NOT_FOUND => {
                output::line(&format!("already removed {id}"));
            }
            other => return Err(other),
        },
    }
    Ok(())
}

/// Whether deleting the partial file is confirmed: `yes`, or an affirmative answer from
/// `ask` when `interactive`. A caller that cannot be asked and did not pass `--yes` is refused.
///
/// # Errors
/// [`CliError::Input`] (exit 2) when not confirmed.
fn confirm_delete_partial(
    yes: bool,
    interactive: bool,
    ask: impl FnOnce() -> bool,
) -> Result<(), CliError> {
    if yes || (interactive && ask()) {
        return Ok(());
    }
    if interactive {
        Err(CliError::Input("not confirmed; the partial file was kept"))
    } else {
        Err(CliError::Input(
            "--delete-partial deletes a file: pass --yes to confirm (no terminal to ask)",
        ))
    }
}

/// Asks on the terminal; only `y` or `yes` (any case) confirms.
fn ask_at_terminal() -> bool {
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "Delete the partial file as well? [y/N] ");
    let _ = err.flush();
    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deleting_the_partial_file_needs_yes_or_a_terminal_answer() {
        assert!(confirm_delete_partial(true, false, || false).is_ok());
        assert!(confirm_delete_partial(false, true, || true).is_ok());
        let declined = confirm_delete_partial(false, true, || false);
        assert!(matches!(declined, Err(CliError::Input(_))));
        let asked = std::cell::Cell::new(false);
        let piped = confirm_delete_partial(false, false, || {
            asked.set(true);
            true
        });
        assert!(!asked.get(), "never asked without a terminal");
        let Err(err) = piped else {
            unreachable!("a pipe is not a confirmation");
        };
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("--yes"), "{err}");
    }

    #[test]
    fn control_results_carry_a_state() {
        use forgedm_protocol::{EmptyResult, StateResult};
        let paused = ResultBody::Pause(StateResult {
            state: DownloadState::Paused,
        });
        assert_eq!(state_of(paused).unwrap(), DownloadState::Paused);
        assert!(state_of(ResultBody::Remove(EmptyResult {})).is_err());
    }
}
