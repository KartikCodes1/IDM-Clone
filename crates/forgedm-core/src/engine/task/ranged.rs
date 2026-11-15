//! The ranged transfer: `bytes=c-(T-1)` with `If-Range`, every response judged by
//! [`decide`] before a single byte is written (PLAN S1.9, THREAT_MODEL NET-01 and NET-02).
//!
//! The same loop serves a fresh download (committed offset 0) and a resumed one. A 206 that
//! covers less than the request ("short") is followed by another request for the rest.

use std::fs::File;

use forgedm_protocol::{DownloadErrorCode, TransferMode};
use reqwest::Response;
use url::Url;

use super::{DownloadTask, Landed, client_stop, internal};
use crate::checkpoint::StoreCommitter;
use crate::client::{FetchSpec, response_meta};
use crate::error::{Failure, TaskStop};
use crate::http::content_range::RangeRequest;
use crate::http::range_decision::{RangeDecision, RangedRequest, decide};
use crate::http::validators::ResumeIdentity;
use crate::probe::Probed;
use crate::resume::Continuation;
use crate::transfer::single::{Plan, Stream, choose_name, stream_body};
use crate::writer::Writer;

/// Most requests one ranged transfer makes: a server answering one byte at a time must not
/// keep a download alive forever.
const MAX_REQUESTS: u32 = 4096;

/// What one request tells the loop to do next.
enum Step {
    /// The body of `[start, end_excl)` follows.
    Body { start: u64, end_excl: u64 },
    /// The server proved nothing is left to fetch.
    Done,
}

impl DownloadTask {
    /// A fresh download whose probe verified a 206 with a known `total`: stage the file,
    /// then run the ranged transfer from byte 0.
    pub(super) async fn fresh_ranged(
        &mut self,
        probed: Probed,
        total: u64,
    ) -> Result<Landed, TaskStop> {
        let name = choose_name(
            &self.job,
            probed.content_disposition.as_deref(),
            &probed.final_url,
        );
        let plan = Plan {
            transfer_mode: TransferMode::SingleRanged,
            total: Some(total),
            mime: probed.mime.clone(),
            validators: probed.result.validators.clone(),
        };
        let staged = self.stage(&plan, &probed.final_url, &name).await?;
        let path = staged.path.clone();
        let identity = ResumeIdentity::from_validators(Some(total), &plan.validators);
        let from = Continuation {
            committed: 0,
            total,
            identity,
        };
        let final_url = self.ranged_transfer(staged.file, &from).await?;
        Ok(Landed {
            path,
            final_url,
            name,
            total: Some(total),
        })
    }

    /// Transfers `[from.committed, from.total)` into `file` (whose first `committed` bytes are
    /// on disk) and moves the download to `verifying`. Returns the URL of the last response.
    pub(super) async fn ranged_transfer(
        &mut self,
        file: File,
        from: &Continuation,
    ) -> Result<Url, TaskStop> {
        let (mut writer, committer) = self.start_session(file, from.committed, Some(from.total))?;
        let outcome = self.ranged_loop(&mut writer, &committer, from).await;
        let (reached, final_url) = match outcome {
            Ok((reached, url)) => (Ok(reached), Some(url)),
            Err(stop) => (Err(stop), None),
        };
        self.end_session(writer, &committer, reached).await?;
        final_url.ok_or_else(internal)
    }

    /// Requests, judges and streams until the whole range is on its way to disk. Returns the
    /// absolute offset reached and the URL of the last response.
    async fn ranged_loop(
        &self,
        writer: &mut Writer,
        committer: &StoreCommitter,
        from: &Continuation,
    ) -> Result<(u64, Url), TaskStop> {
        let if_range = from.identity.if_range();
        let mut offset = from.committed;
        for _ in 0..MAX_REQUESTS {
            let (step, response, url) = self.ask(offset, from, if_range.as_deref()).await?;
            match step {
                Step::Done => return Ok((offset, url)),
                Step::Body { start, end_excl } => {
                    offset = self
                        .stream_part(response, writer, committer, start, end_excl)
                        .await?;
                }
            }
            if offset >= from.total {
                return Ok((offset, url));
            }
        }
        Err(Failure::protocol_violation("too_many_partial_responses").into())
    }

    /// Asks for everything from `offset` on and judges the response before anything is read
    /// from its body. A request at the very end asks for the byte after it: a 416 then proves
    /// completeness.
    async fn ask(
        &self,
        offset: u64,
        from: &Continuation,
        if_range: Option<&str>,
    ) -> Result<(Step, Response, Url), TaskStop> {
        let window = RangeRequest {
            start: offset,
            end_excl: if offset >= from.total {
                from.total.saturating_add(1)
            } else {
                from.total
            },
        };
        let token = self.ctx.live.stop().token().clone();
        let spec = FetchSpec::ranged(window, if_range.map(str::to_owned));
        let fetched = self
            .ctx
            .client
            .fetch(self.job.url.as_url(), &spec, &token)
            .await
            .map_err(|e| client_stop(e, &self.ctx))?;
        let request = RangedRequest {
            range: window,
            if_range_sent: if_range.is_some(),
            segment_no: 0,
            has_committed_data: offset > 0,
        };
        let decision = decide(&request, &from.identity, &response_meta(&fetched.response));
        let step = judge(decision)?;
        Ok((step, fetched.response, fetched.final_url))
    }

    /// Streams the accepted body `[start, end_excl)`; a body that ends early is incomplete.
    async fn stream_part(
        &self,
        response: Response,
        writer: &mut Writer,
        committer: &StoreCommitter,
        start: u64,
        end_excl: u64,
    ) -> Result<u64, TaskStop> {
        let env = Stream {
            live: &self.ctx.live,
            stop: self.ctx.live.stop(),
            dir: &self.job.target_dir,
            start,
            limit: Some(end_excl),
        };
        self.ctx.live.set_connections(1);
        let reached = stream_body(response, writer, &env, committer).await?;
        if reached < end_excl {
            return Err(Failure::new(DownloadErrorCode::BodyIncomplete).into());
        }
        Ok(reached)
    }
}

/// Turns the range decision into the next step, or the failure it stands for.
fn judge(decision: RangeDecision) -> Result<Step, TaskStop> {
    match decision {
        RangeDecision::Accept {
            start, end_excl, ..
        } => Ok(Step::Body { start, end_excl }),
        RangeDecision::Accept200Full { end_excl } => Ok(Step::Body { start: 0, end_excl }),
        RangeDecision::SegmentComplete => Ok(Step::Done),
        RangeDecision::RemoteChanged => Err(Failure::new(DownloadErrorCode::RemoteChanged).into()),
        RangeDecision::UnsupportedContentEncoding => {
            Err(Failure::new(DownloadErrorCode::UnsupportedContentEncoding).into())
        }
        RangeDecision::RangeUnsupported => {
            Err(Failure::protocol_violation("ranges_no_longer_supported").into())
        }
        RangeDecision::Reprobe => Err(Failure::protocol_violation("reprobe_needed").into()),
        RangeDecision::Violation(violation) => {
            Err(Failure::protocol_violation(violation.reason()).into())
        }
        RangeDecision::OtherStatus { status: 401 | 407 } => Err(TaskStop::NeedsCredentials),
        RangeDecision::OtherStatus { status } => Err(Failure::http_status(status).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::range_decision::RangeViolation;

    #[test]
    fn only_accepted_decisions_lead_to_a_body() {
        assert!(matches!(
            judge(RangeDecision::Accept {
                start: 5,
                end_excl: 9,
                short: false
            }),
            Ok(Step::Body {
                start: 5,
                end_excl: 9
            })
        ));
        assert!(matches!(
            judge(RangeDecision::Accept200Full { end_excl: 9 }),
            Ok(Step::Body {
                start: 0,
                end_excl: 9
            })
        ));
        assert!(matches!(
            judge(RangeDecision::SegmentComplete),
            Ok(Step::Done)
        ));
        for refused in [
            RangeDecision::RemoteChanged,
            RangeDecision::RangeUnsupported,
            RangeDecision::Reprobe,
            RangeDecision::UnsupportedContentEncoding,
            RangeDecision::Violation(RangeViolation::WrongStart),
            RangeDecision::OtherStatus { status: 503 },
        ] {
            assert!(judge(refused).is_err(), "{refused:?}");
        }
        assert_eq!(
            judge(RangeDecision::OtherStatus { status: 401 }).err(),
            Some(TaskStop::NeedsCredentials)
        );
    }

    #[test]
    fn remote_changed_is_the_user_retryable_remote_changed_failure() {
        let Err(TaskStop::Fail(failure)) = judge(RangeDecision::RemoteChanged) else {
            unreachable!("not a failure");
        };
        assert_eq!(failure.code, DownloadErrorCode::RemoteChanged);
    }
}
