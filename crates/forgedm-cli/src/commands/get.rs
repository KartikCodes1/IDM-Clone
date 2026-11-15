//! `forgedm get <id> [--json]` (alias `show`): one download in detail. Never starts the agent.

use forgedm_protocol::{DownloadIdRequest, RequestBody, ResultBody};
use uuid::Uuid;

use crate::client::Target;
use crate::error::CliError;
use crate::output;

/// Fetches `GetDownload` and prints it as text, or as one JSON object with `--json`: the
/// summary and the detail fields side by side, exactly as on the wire.
pub async fn run(target: &Target, id: Uuid, json: bool) -> Result<(), CliError> {
    let (mut client, _hello) = target.connect().await?;
    let request = RequestBody::GetDownload(DownloadIdRequest { download_id: id });
    let ResultBody::GetDownload(detail) = client.call(request).await? else {
        return Err(CliError::Unexpected);
    };
    if json {
        output::line(&serde_json::to_string(&*detail)?);
    } else {
        output::detail(&detail);
    }
    Ok(())
}
