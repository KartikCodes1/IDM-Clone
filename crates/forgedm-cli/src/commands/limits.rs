//! `forgedm limits|pause-all|resume-all|priority`: agent-wide limits, batch control and queue
//! priority (PLAN A.3, S1.13). None of them starts the agent: they act on what it owns.

use forgedm_protocol::{
    Empty, LimitScope, PauseAllRequest, Priority, RequestBody, ResultBody, UpdateLimitsRequest,
    UpdatePriorityRequest,
};
use uuid::Uuid;

use crate::cli::{LimitsArgs, PriorityArg, SpeedLimit};
use crate::client::Target;
use crate::error::CliError;
use crate::output;

/// The wire request for `args`. A bare `limits` changes nothing and reports what is in force.
fn limits_request(args: LimitsArgs) -> UpdateLimitsRequest {
    let mut request = UpdateLimitsRequest::new(LimitScope::Global);
    request.max_active_downloads = args.max_active;
    request.max_connections = args.connections;
    request.rate_limit_bps = args.speed.map(|speed| match speed {
        SpeedLimit::Off => None,
        SpeedLimit::BytesPerSecond(bps) => Some(bps),
    });
    request
}

/// `forgedm limits [--max-active N] [--speed <bytes/s|off>] [--connections N]`.
pub async fn limits(target: &Target, args: LimitsArgs) -> Result<(), CliError> {
    let (mut client, _hello) = target.connect().await?;
    let body = RequestBody::UpdateLimits(limits_request(args));
    match client.call(body).await? {
        ResultBody::UpdateLimits(result) => output::limits(&result.effective),
        _ => return Err(CliError::Unexpected),
    }
    Ok(())
}

/// `forgedm pause-all [--discard-progress]`.
pub async fn pause_all(target: &Target, discard_progress: bool) -> Result<(), CliError> {
    let (mut client, _hello) = target.connect().await?;
    let body = RequestBody::PauseAll(PauseAllRequest { discard_progress });
    match client.call(body).await? {
        ResultBody::PauseAll(result) => {
            output::paused_all(&result.paused, &result.skipped_not_resumable);
        }
        _ => return Err(CliError::Unexpected),
    }
    Ok(())
}

/// `forgedm resume-all`.
pub async fn resume_all(target: &Target) -> Result<(), CliError> {
    let (mut client, _hello) = target.connect().await?;
    match client.call(RequestBody::ResumeAll(Empty {})).await? {
        ResultBody::ResumeAll(result) => output::resumed_all(&result.resumed),
        _ => return Err(CliError::Unexpected),
    }
    Ok(())
}

const fn wire_priority(arg: PriorityArg) -> Priority {
    match arg {
        PriorityArg::High => Priority::High,
        PriorityArg::Normal => Priority::Normal,
        PriorityArg::Low => Priority::Low,
    }
}

/// `forgedm priority <id> high|normal|low`.
pub async fn priority(target: &Target, id: Uuid, priority: PriorityArg) -> Result<(), CliError> {
    let (mut client, _hello) = target.connect().await?;
    let body = RequestBody::UpdatePriority(UpdatePriorityRequest {
        download_id: id,
        priority: wire_priority(priority),
    });
    match client.call(body).await? {
        ResultBody::UpdatePriority(_) => {
            output::line(&format!("priority {} {id}", enum_word(priority)));
        }
        _ => return Err(CliError::Unexpected),
    }
    Ok(())
}

const fn enum_word(arg: PriorityArg) -> &'static str {
    match arg {
        PriorityArg::High => "high",
        PriorityArg::Normal => "normal",
        PriorityArg::Low => "low",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(
        max_active: Option<u32>,
        speed: Option<SpeedLimit>,
        connections: Option<u32>,
    ) -> LimitsArgs {
        LimitsArgs {
            max_active,
            speed,
            connections,
        }
    }

    #[test]
    fn a_bare_limits_command_changes_nothing() {
        let request = limits_request(args(None, None, None));
        assert_eq!(request, UpdateLimitsRequest::new(LimitScope::Global));
    }

    #[test]
    fn off_means_unlimited_and_absent_means_unchanged() {
        let off = limits_request(args(None, Some(SpeedLimit::Off), None));
        assert_eq!(off.rate_limit_bps, Some(None));
        let set = limits_request(args(
            Some(2),
            Some(SpeedLimit::BytesPerSecond(4096)),
            Some(5),
        ));
        assert_eq!(set.rate_limit_bps, Some(Some(4096)));
        assert_eq!(set.max_active_downloads, Some(2));
        assert_eq!(set.max_connections, Some(5));
        assert_eq!(set.scope, LimitScope::Global);
    }

    #[test]
    fn priorities_map_to_the_wire() {
        assert_eq!(wire_priority(PriorityArg::High), Priority::High);
        assert_eq!(wire_priority(PriorityArg::Normal), Priority::Normal);
        assert_eq!(wire_priority(PriorityArg::Low), Priority::Low);
    }
}
