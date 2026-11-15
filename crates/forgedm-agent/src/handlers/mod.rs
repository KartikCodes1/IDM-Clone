//! The agent's request handler (PHASE_0_1_PLAN S0.8).
//!
//! Served here: Handshake, Ping, GetAgentStatus, GetSnapshot, Subscribe, Unsubscribe,
//! ListDownloads, ShutdownAgentSafely, (S1.7) EnqueueUrl and GetDownload and (S1.9) Pause,
//! Resume, Cancel, Retry and Remove through an injected [`EngineControl`], and (S1.11)
//! `UpdateLimits` for the connection cap of one download. Without an engine
//! they answer `not_implemented`, as do the other limits actions ([`limits`]) until S1.13.
//!
//! The IPC server does not cancel handlers (S0.7), so every call is bounded here by
//! `ENGINE_REPLY_TIMEOUT` (5 s); a timeout maps to the retryable `busy` error (PCR-05).

mod control;
mod enqueue;
mod lifecycle;
mod limits;
mod query;

use std::sync::Arc;
use std::time::{Duration, Instant};

use forgedm_ipc::{Connection, HandlerFuture, RequestHandler};
use forgedm_protocol::{ErrorBody, ErrorCode, Request, RequestBody, Response, ResultBody};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::engine_host::EngineControl;
use crate::error::AgentError;
use crate::events_hub::HubHandle;

/// How long one request may take before the client gets `busy` (A.9 `ENGINE_REPLY_TIMEOUT`).
pub const ENGINE_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Answers requests from pipe clients.
pub struct AgentHandler {
    hub: HubHandle,
    shutdown: CancellationToken,
    started: Instant,
    reply_timeout: Duration,
    engine: Option<Arc<dyn EngineControl>>,
}

impl std::fmt::Debug for AgentHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentHandler")
            .field("engine", &self.engine.is_some())
            .finish_non_exhaustive()
    }
}

impl AgentHandler {
    /// A handler over `hub`. Cancelling `shutdown` is how `ShutdownAgentSafely` stops the
    /// agent; `started` is the uptime base.
    #[must_use]
    pub fn new(hub: HubHandle, shutdown: CancellationToken, started: Instant) -> Self {
        Self {
            hub,
            shutdown,
            started,
            reply_timeout: ENGINE_REPLY_TIMEOUT,
            engine: None,
        }
    }

    /// Serves EnqueueUrl and GetDownload through `engine`.
    #[must_use]
    pub fn with_engine(self, engine: Arc<dyn EngineControl>) -> Self {
        Self {
            engine: Some(engine),
            ..self
        }
    }

    /// The engine, or `not_implemented` for an engine-less handler.
    fn engine(&self) -> Result<&Arc<dyn EngineControl>, AgentError> {
        self.engine.as_ref().ok_or(AgentError::NotImplemented)
    }

    /// Overrides the reply bound (tests).
    #[cfg(test)]
    fn with_reply_timeout(self, reply_timeout: Duration) -> Self {
        Self {
            reply_timeout,
            ..self
        }
    }

    async fn dispatch(
        &self,
        conn: &mut Connection,
        body: RequestBody,
    ) -> Result<ResultBody, AgentError> {
        use RequestBody as B;
        if self.shutdown.is_cancelled() && is_mutating(&body) {
            return Err(AgentError::ShuttingDown);
        }
        match body {
            B::Handshake(_) => lifecycle::handshake(self),
            B::Ping(_) => lifecycle::ping(self),
            B::GetAgentStatus(_) => lifecycle::status(self).await,
            B::ShutdownAgentSafely(_) => lifecycle::shutdown(self).await,
            B::GetSnapshot(_) => query::snapshot(self).await,
            B::ListDownloads(request) => query::list(self, request).await,
            B::Subscribe(request) => query::subscribe(self, conn, request).await,
            B::Unsubscribe(_) => query::unsubscribe(conn),
            B::EnqueueUrl(request) => enqueue::enqueue_url(self, conn, request).await,
            B::GetDownload(request) => query::get_download(self, request).await,
            B::Pause(request) => control::pause(self, request).await,
            B::Resume(request) => control::resume(self, request).await,
            B::Cancel(request) => control::cancel(self, request).await,
            B::Retry(request) => control::retry(self, request).await,
            B::Remove(request) => control::remove(self, request).await,
            B::PauseAll(request) => limits::pause_all(&request),
            B::ResumeAll(request) => limits::resume_all(&request),
            B::UpdateLimits(request) => limits::update_limits(self, request).await,
            B::UpdatePriority(request) => limits::update_priority(&request),
        }
    }

    async fn answer(&self, conn: &mut Connection, request: Request) -> Response {
        let request_id = request.request_id;
        let action = request.body.action();
        let is_subscribe = matches!(request.body, RequestBody::Subscribe(_));
        let outcome = timeout(self.reply_timeout, self.dispatch(conn, request.body)).await;
        let result = match outcome {
            Ok(result) => result,
            Err(_elapsed) => {
                tracing::warn!(?action, "request exceeded the reply timeout");
                if is_subscribe {
                    // The sink may never be registered; do not leave a dead subscription
                    // exempting the connection from the idle timeout.
                    conn.unsubscribe();
                }
                Err(AgentError::Busy)
            }
        };
        match result {
            Ok(body) => Response::success(request_id, &body).unwrap_or_else(|err| {
                Response::failure(request_id, AgentError::Encode(err).to_wire())
            }),
            Err(error) => Response::failure(request_id, error.to_wire()),
        }
    }
}

impl RequestHandler for AgentHandler {
    fn handle<'a>(&'a self, conn: &'a mut Connection, request: Request) -> HandlerFuture<'a> {
        Box::pin(self.answer(conn, request))
    }
}

/// A fallback answer for requests that arrive before the agent finished starting.
#[must_use]
pub fn starting_response(request: &Request) -> Response {
    Response::failure(request.request_id, ErrorBody::new(ErrorCode::Busy))
}

/// Actions that change download state; refused with `shutting_down` once shutdown began.
const fn is_mutating(body: &RequestBody) -> bool {
    matches!(
        body,
        RequestBody::EnqueueUrl(_)
            | RequestBody::Pause(_)
            | RequestBody::Resume(_)
            | RequestBody::Cancel(_)
            | RequestBody::Retry(_)
            | RequestBody::Remove(_)
            | RequestBody::PauseAll(_)
            | RequestBody::ResumeAll(_)
            | RequestBody::UpdateLimits(_)
            | RequestBody::UpdatePriority(_)
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use forgedm_platform::peer::PeerIdentity;
    use forgedm_platform::sid::Sid;
    use forgedm_protocol::{
        AgentState, ClientKind, DownloadIdRequest, Empty, EnqueueUrlRequest, HandshakeRequest,
        ProxySource, SubscribeRequest,
    };
    use uuid::Uuid;

    use super::*;
    use crate::events_hub::{self, HubInit};

    fn conn() -> Connection {
        let peer = PeerIdentity {
            sid: Sid::current_user().unwrap(),
            image_path: std::env::current_exe().unwrap(),
        };
        Connection::new(1, peer)
    }

    fn handler() -> AgentHandler {
        let hub = events_hub::spawn(
            HubInit {
                instance_id: Uuid::now_v7(),
                proxy_source: ProxySource::None,
                data_dir: r"C:\data".to_owned(),
                started: Instant::now(),
                pid: std::process::id(),
            },
            Vec::new(),
        );
        AgentHandler::new(hub, CancellationToken::new(), Instant::now())
    }

    fn request(body: RequestBody) -> Request {
        Request::new(7, body)
    }

    fn error_code(response: &Response) -> ErrorCode {
        response.error().expect("a failure response").code
    }

    #[tokio::test(start_paused = true)]
    async fn handler_timeout_maps_to_busy() {
        let (hub, _never_serviced) = HubHandle::unresponsive();
        let handler = AgentHandler::new(hub, CancellationToken::new(), Instant::now());
        let mut conn = conn();

        let response = handler
            .handle(&mut conn, request(RequestBody::GetSnapshot(Empty {})))
            .await;

        assert_eq!(error_code(&response), ErrorCode::Busy);
        assert!(response.error().unwrap().retryable);
        assert_eq!(response.request_id(), 7);
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_subscribe_leaves_no_subscription_behind() {
        let (hub, _never_serviced) = HubHandle::unresponsive();
        let handler = AgentHandler::new(hub, CancellationToken::new(), Instant::now())
            .with_reply_timeout(Duration::from_millis(50));
        let mut conn = conn();
        let body = RequestBody::Subscribe(SubscribeRequest {
            agent_instance_id: Uuid::now_v7(),
            after_seq: 0,
        });

        let response = handler.handle(&mut conn, request(body)).await;

        assert_eq!(error_code(&response), ErrorCode::Busy);
        assert!(!conn.is_subscribed());
    }

    #[tokio::test]
    async fn handshake_advertises_only_implemented_capabilities() {
        let handler = handler();
        let mut conn = conn();
        let hello = HandshakeRequest {
            client_kind: ClientKind::Cli,
            client_name: "t".to_owned(),
            client_version: "0".to_owned(),
            min_protocol: 1,
            max_protocol: 1,
        };

        let response = handler
            .handle(&mut conn, request(RequestBody::Handshake(hello)))
            .await;

        let ResultBody::Handshake(result) = ResultBody::from_value(
            forgedm_protocol::Action::Handshake,
            response.result().unwrap().clone(),
        )
        .unwrap() else {
            unreachable!("not a handshake result");
        };
        assert_eq!(
            result.capabilities,
            ["subscribe_replay", "checksum_sha256", "resume"]
        );
        assert_eq!(result.agent_instance_id, handler.hub.instance_id());
        assert_eq!(result.agent_pid, std::process::id());
        assert_eq!((result.protocol_version, result.protocol_minor), (1, 0));
    }

    #[tokio::test]
    async fn later_slice_actions_are_not_implemented_and_not_retryable() {
        let handler = handler();
        let mut conn = conn();
        let id = Uuid::nil();
        let bodies = [
            RequestBody::EnqueueUrl(EnqueueUrlRequest::new("https://example.com/")),
            RequestBody::GetDownload(DownloadIdRequest { download_id: id }),
            RequestBody::Resume(DownloadIdRequest { download_id: id }),
            RequestBody::ResumeAll(Empty {}),
        ];
        for body in bodies {
            let response = handler.handle(&mut conn, request(body)).await;
            assert_eq!(error_code(&response), ErrorCode::NotImplemented);
            assert!(!response.error().unwrap().retryable);
        }
    }

    #[tokio::test]
    async fn mutating_actions_get_shutting_down_after_shutdown_started() {
        let handler = handler();
        let mut conn = conn();

        let reply = handler
            .handle(
                &mut conn,
                request(RequestBody::ShutdownAgentSafely(Empty {})),
            )
            .await;
        assert!(reply.result().is_some());
        assert!(handler.shutdown.is_cancelled());
        assert_eq!(
            handler.hub.status().await.unwrap().state,
            AgentState::ShuttingDown
        );

        let enqueue = RequestBody::EnqueueUrl(EnqueueUrlRequest::new("https://example.com/"));
        let refused = handler.handle(&mut conn, request(enqueue)).await;
        assert_eq!(error_code(&refused), ErrorCode::ShuttingDown);
        // Reads and a repeated shutdown still work.
        let ping = handler
            .handle(&mut conn, request(RequestBody::Ping(Empty {})))
            .await;
        assert!(ping.result().is_some());
        let again = handler
            .handle(
                &mut conn,
                request(RequestBody::ShutdownAgentSafely(Empty {})),
            )
            .await;
        assert!(again.result().is_some());
    }

    #[tokio::test]
    async fn failed_subscribe_clears_the_connection_subscription() {
        let handler = Arc::new(handler());
        let mut conn = conn();
        let wrong_instance = RequestBody::Subscribe(SubscribeRequest {
            agent_instance_id: Uuid::now_v7(),
            after_seq: 0,
        });

        let response = handler.handle(&mut conn, request(wrong_instance)).await;

        assert_eq!(error_code(&response), ErrorCode::ResyncRequired);
        assert!(!conn.is_subscribed());

        let good = RequestBody::Subscribe(SubscribeRequest {
            agent_instance_id: handler.hub.instance_id(),
            after_seq: 0,
        });
        let response = handler.handle(&mut conn, request(good)).await;
        assert!(response.result().is_some());
        assert!(conn.is_subscribed());
        let _ = handler
            .handle(&mut conn, request(RequestBody::Unsubscribe(Empty {})))
            .await;
        assert!(!conn.is_subscribed());
    }

    /// An engine that only records connection caps.
    struct CapEngine {
        caps: std::sync::Mutex<Vec<(Uuid, u32)>>,
    }

    impl EngineControl for CapEngine {
        fn enqueue(
            &self,
            _spec: crate::engine_host::EnqueueSpec,
        ) -> forgedm_core::clock::BoxFuture<
            '_,
            Result<forgedm_protocol::EnqueueUrlResult, AgentError>,
        > {
            Box::pin(async { Err(AgentError::NotImplemented) })
        }

        fn detail(
            &self,
            _id: Uuid,
        ) -> forgedm_core::clock::BoxFuture<'_, Result<forgedm_protocol::DownloadDetail, AgentError>>
        {
            Box::pin(async { Err(AgentError::NotImplemented) })
        }

        fn set_max_connections(
            &self,
            id: Uuid,
            value: u32,
        ) -> forgedm_core::clock::BoxFuture<'_, Result<u32, AgentError>> {
            self.caps.lock().unwrap().push((id, value));
            Box::pin(async move { Ok(value) })
        }
    }

    fn limits_request(
        scope: forgedm_protocol::LimitScope,
    ) -> forgedm_protocol::UpdateLimitsRequest {
        forgedm_protocol::UpdateLimitsRequest::new(scope)
    }

    #[tokio::test]
    async fn update_limits_sets_the_connection_cap_of_one_download() {
        let engine = Arc::new(CapEngine {
            caps: std::sync::Mutex::new(Vec::new()),
        });
        let handler = handler().with_engine(engine.clone());
        let mut conn = conn();
        let id = Uuid::now_v7();
        let mut request =
            limits_request(forgedm_protocol::LimitScope::Download { download_id: id });
        request.max_connections = Some(12);

        let response = handler
            .handle(
                &mut conn,
                super::Request::new(3, RequestBody::UpdateLimits(request)),
            )
            .await;

        let ResultBody::UpdateLimits(result) = ResultBody::from_value(
            forgedm_protocol::Action::UpdateLimits,
            response.result().expect("a success").clone(),
        )
        .unwrap() else {
            unreachable!("not an UpdateLimits result");
        };
        assert_eq!(result.effective.max_connections, Some(12));
        assert_eq!(engine.caps.lock().unwrap().as_slice(), [(id, 12)]);
    }

    #[tokio::test]
    async fn limits_that_belong_to_later_slices_are_refused_not_ignored() {
        let engine = Arc::new(CapEngine {
            caps: std::sync::Mutex::new(Vec::new()),
        });
        let handler = handler().with_engine(engine.clone());
        let mut conn = conn();
        let id = Uuid::now_v7();
        let mut global = limits_request(forgedm_protocol::LimitScope::Global);
        global.max_connections = Some(4);
        let mut rate = limits_request(forgedm_protocol::LimitScope::Download { download_id: id });
        rate.rate_limit_bps = Some(Some(2048));
        for request in [global, rate] {
            let response = handler
                .handle(
                    &mut conn,
                    super::Request::new(4, RequestBody::UpdateLimits(request)),
                )
                .await;
            assert_eq!(error_code(&response), ErrorCode::NotImplemented);
        }
        assert!(
            engine.caps.lock().unwrap().is_empty(),
            "nothing was applied"
        );
    }

    #[tokio::test]
    async fn update_limits_without_an_engine_is_not_implemented() {
        let handler = handler();
        let mut conn = conn();
        let mut request = limits_request(forgedm_protocol::LimitScope::Download {
            download_id: Uuid::nil(),
        });
        request.max_connections = Some(2);

        let response = handler
            .handle(
                &mut conn,
                super::Request::new(5, RequestBody::UpdateLimits(request)),
            )
            .await;

        assert_eq!(error_code(&response), ErrorCode::NotImplemented);
    }
    #[test]
    fn starting_response_is_retryable_busy() {
        let response = starting_response(&request(RequestBody::Ping(Empty {})));
        assert_eq!(error_code(&response), ErrorCode::Busy);
    }
}
