use crate::counters::{MediaTrafficCounters, MediaTrafficSnapshot};
use crate::error::RelayError;
use crate::policy::DestinationPolicy;
use bytes::Bytes;
use futures_core::Stream;
use isahc::config::Configurable;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

/// Configuration options for the media relay client.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// Optional outbound proxy (e.g. `socks5h://127.0.0.1:1080` or `http://127.0.0.1:8080`).
    pub media_proxy: Option<String>,
    /// TLS certificate and hostname verification (must be `true`).
    pub tls_verify: bool,
    /// Connection timeout. Default: 10s.
    pub connect_timeout: Duration,
    /// Timeout for each individual chunk read. Default: 120s.
    pub read_timeout: Duration,
    /// Maximum byte size of each buffered chunk. Default: 32768 (32 KiB).
    pub chunk_size: usize,
    /// Capacity of bounded chunk buffer channel. Default: 64 (2 MiB max buffered).
    pub max_buffered_chunks: usize,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            media_proxy: None,
            tls_verify: true,
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(120),
            chunk_size: 32768,
            max_buffered_chunks: 64,
        }
    }
}

impl RelayConfig {
    /// Validate configuration settings.
    pub fn validate(&self) -> Result<(), RelayError> {
        if !self.tls_verify {
            return Err(RelayError::InvalidConfig(
                "tls_verify must be true; disabling TLS verification is strictly forbidden".into(),
            ));
        }
        if self.chunk_size == 0 {
            return Err(RelayError::InvalidConfig("chunk_size must be > 0".into()));
        }
        if self.max_buffered_chunks == 0 {
            return Err(RelayError::InvalidConfig(
                "max_buffered_chunks must be > 0".into(),
            ));
        }
        if let Some(ref proxy) = self.media_proxy {
            let uri: isahc::http::Uri = proxy
                .parse()
                .map_err(|e| RelayError::InvalidConfig(format!("invalid proxy URI: {e}")))?;
            let scheme = uri
                .scheme_str()
                .ok_or_else(|| RelayError::InvalidConfig("proxy URI missing scheme".into()))?;
            match scheme.to_ascii_lowercase().as_str() {
                "socks5h" | "socks5" | "http" => {}
                other => {
                    return Err(RelayError::InvalidConfig(format!(
                        "unsupported proxy scheme '{other}', expected socks5h://, socks5://, or http://"
                    )));
                }
            }
            if uri.host().is_none() {
                return Err(RelayError::InvalidConfig("proxy URI missing host".into()));
            }
        }
        Ok(())
    }
}

/// Permitted HTTP method for relaying media source requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayMethod {
    Get,
    Head,
}

/// Request specification for fetching media from an upstream source.
#[derive(Debug, Clone)]
pub struct MediaSourceRequest {
    /// Raw target URL.
    pub url: String,
    /// HTTP method (`Get` or `Head`).
    pub method: RelayMethod,
    /// Optional HTTP Range header value (e.g. `bytes=0-1023`).
    pub range: Option<String>,
    /// Optional HTTP If-Range header value (validator token or date).
    pub if_range: Option<String>,
    /// Optional User-Agent header value.
    pub user_agent: Option<String>,
}

impl MediaSourceRequest {
    /// Create a GET request for the specified URL.
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: RelayMethod::Get,
            range: None,
            if_range: None,
            user_agent: None,
        }
    }

    /// Create a HEAD request for the specified URL.
    pub fn head(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: RelayMethod::Head,
            range: None,
            if_range: None,
            user_agent: None,
        }
    }

    /// Set the Range header value.
    pub fn with_range(mut self, range: impl Into<String>) -> Self {
        self.range = Some(range.into());
        self
    }

    /// Set the If-Range header value.
    pub fn with_if_range(mut self, if_range: impl Into<String>) -> Self {
        self.if_range = Some(if_range.into());
        self
    }

    /// Set the User-Agent header value.
    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = Some(user_agent.into());
        self
    }
}

/// Preserved metadata and headers from the upstream HTTP response.
#[derive(Debug, Clone)]
pub struct RelayResponseMeta {
    /// HTTP status code returned by upstream server (e.g. 200, 206, 416).
    pub status: http::StatusCode,
    /// Preserved media headers (`Content-Type`, `Content-Length`, `Content-Range`, `Accept-Ranges`, `ETag`, `Last-Modified`, `Cache-Control`).
    pub headers: http::HeaderMap,
    /// Parsed content length in bytes if provided by upstream.
    pub content_length: Option<u64>,
    /// Final URL after following redirects.
    pub final_url: String,
}

/// Lightweight token for cancelling in-flight media relay streams externally.
#[derive(Clone, Debug)]
pub struct RelayCancellationToken {
    tx: Arc<tokio::sync::watch::Sender<bool>>,
    rx: tokio::sync::watch::Receiver<bool>,
}

impl Default for RelayCancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl RelayCancellationToken {
    /// Create a new, uncancelled token.
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::watch::channel(false);
        Self {
            tx: Arc::new(tx),
            rx,
        }
    }

    /// Cancel the associated stream.
    pub fn cancel(&self) {
        let _ = self.tx.send(true);
    }

    /// Check if cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    /// Wait until cancellation is signalled.
    pub async fn cancelled(&self) {
        let mut rx = self.rx.clone();
        if *rx.borrow() {
            return;
        }
        let _ = rx.wait_for(|&c| c).await;
    }
}

/// Stream yielding chunks of media body bytes from the upstream server.
///
/// Dropping this stream before reaching EOF or failure triggers external cancellation
/// of the background reader task and increments the `streams_cancelled` counter.
pub struct RelayBodyStream {
    receiver: tokio::sync::mpsc::Receiver<Result<Bytes, RelayError>>,
    cancel_token: RelayCancellationToken,
    counters: MediaTrafficCounters,
    completed: bool,
}

impl std::fmt::Debug for RelayBodyStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayBodyStream")
            .field("completed", &self.completed)
            .finish()
    }
}

impl Stream for RelayBodyStream {
    type Item = Result<Bytes, RelayError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.receiver.poll_recv(cx) {
            Poll::Ready(Some(Ok(bytes))) => Poll::Ready(Some(Ok(bytes))),
            Poll::Ready(Some(Err(err))) => {
                self.completed = true;
                Poll::Ready(Some(Err(err)))
            }
            Poll::Ready(None) => {
                self.completed = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for RelayBodyStream {
    fn drop(&mut self) {
        self.counters.dec_active_streams();
        if !self.completed {
            self.cancel_token.cancel();
            self.counters.inc_streams_cancelled();
        }
    }
}

/// Complete response from opening an upstream media source.
#[derive(Debug)]
pub struct MediaStreamResponse {
    /// Response metadata and preserved headers.
    pub meta: RelayResponseMeta,
    /// Bounded body chunk stream.
    pub body: RelayBodyStream,
    /// Cancellation handle to abort streaming externally.
    pub cancel_token: RelayCancellationToken,
}

/// Independent, service-agnostic media relay client.
#[derive(Clone, Debug)]
pub struct MediaRelay {
    config: RelayConfig,
    policy: DestinationPolicy,
    client: isahc::HttpClient,
    counters: MediaTrafficCounters,
}

impl MediaRelay {
    /// Construct a new `MediaRelay` with configuration and destination policy.
    pub fn new(config: RelayConfig, policy: DestinationPolicy) -> Result<Self, RelayError> {
        Self::with_counters(config, policy, MediaTrafficCounters::new())
    }

    /// Construct a new `MediaRelay` with shared external traffic counters.
    pub fn with_counters(
        config: RelayConfig,
        policy: DestinationPolicy,
        counters: MediaTrafficCounters,
    ) -> Result<Self, RelayError> {
        config.validate()?;

        let mut builder = isahc::HttpClient::builder()
            .redirect_policy(isahc::config::RedirectPolicy::None)
            .connect_timeout(config.connect_timeout);

        if let Some(ref proxy) = config.media_proxy {
            let proxy_uri: isahc::http::Uri = proxy
                .parse()
                .map_err(|e| RelayError::InvalidConfig(format!("invalid proxy URI: {e}")))?;
            builder = builder.proxy(Some(proxy_uri));
        }

        let client = builder
            .build()
            .map_err(|e| RelayError::InvalidConfig(format!("failed to build HTTP client: {e}")))?;

        Ok(Self {
            config,
            policy,
            client,
            counters,
        })
    }

    /// Reference to this relay's traffic counters.
    pub fn counters(&self) -> &MediaTrafficCounters {
        &self.counters
    }

    /// Snapshot of traffic statistics.
    pub fn snapshot(&self) -> MediaTrafficSnapshot {
        self.counters.snapshot()
    }

    /// Open an upstream media stream without retry/refresh.
    pub async fn open(&self, req: MediaSourceRequest) -> Result<MediaStreamResponse, RelayError> {
        self.counters.inc_requests_total();
        match self.execute_open(req).await {
            Ok(res) => {
                self.counters.inc_requests_succeeded();
                Ok(res)
            }
            Err(err) => {
                self.counters.inc_requests_failed();
                Err(err)
            }
        }
    }

    /// Open an upstream media stream with a pre-header expired URL refresh hook.
    ///
    /// If the initial upstream request responds with 401, 403, 404, or 410 before headers
    /// are yielded, `refresh_url` is invoked once, the new URL is validated against
    /// destination policy, and the request is retried.
    pub async fn open_with_refresh<F, Fut>(
        &self,
        mut req: MediaSourceRequest,
        refresh_url: F,
    ) -> Result<MediaStreamResponse, RelayError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<String, RelayError>>,
    {
        self.counters.inc_requests_total();
        match self.execute_open(req.clone()).await {
            Ok(res) => {
                self.counters.inc_requests_succeeded();
                Ok(res)
            }
            Err(RelayError::UpstreamPreHeader { status, .. })
                if matches!(status, 401 | 403 | 404 | 410) =>
            {
                tracing::info!(status, "initial upstream request failed with pre-header error; refreshing URL");
                let new_url = match refresh_url().await {
                    Ok(url) => url,
                    Err(e) => {
                        self.counters.inc_requests_failed();
                        return Err(e);
                    }
                };
                self.counters.inc_pre_header_refreshes();

                let validated = match self.policy.validate_url(&new_url) {
                    Ok(u) => u,
                    Err(e) => {
                        self.counters.inc_requests_failed();
                        return Err(e);
                    }
                };

                req.url = validated.to_string();

                match self.execute_open(req).await {
                    Ok(res) => {
                        self.counters.inc_requests_succeeded();
                        Ok(res)
                    }
                    Err(err) => {
                        self.counters.inc_requests_failed();
                        Err(err)
                    }
                }
            }
            Err(err) => {
                self.counters.inc_requests_failed();
                Err(err)
            }
        }
    }

    async fn execute_open(
        &self,
        req: MediaSourceRequest,
    ) -> Result<MediaStreamResponse, RelayError> {
        let mut current_url = self.policy.validate_url(&req.url)?.to_string();
        let mut current_method = req.method;
        let mut current_range = req.range.clone();
        let mut current_if_range = req.if_range.clone();
        let mut redirect_count: usize = 0;

        let (final_parts, final_body, final_url) = loop {
            let method = match current_method {
                RelayMethod::Get => http::Method::GET,
                RelayMethod::Head => http::Method::HEAD,
            };

            let mut request_builder = isahc::Request::builder()
                .method(method)
                .uri(&current_url);

            if let Some(ref range) = current_range {
                request_builder = request_builder.header(http::header::RANGE, range);
            }
            if let Some(ref if_range) = current_if_range {
                request_builder = request_builder.header(http::header::IF_RANGE, if_range);
            }
            if let Some(ref ua) = req.user_agent {
                request_builder = request_builder.header(http::header::USER_AGENT, ua);
            }

            let request = request_builder
                .body(())
                .map_err(|e| RelayError::InvalidRequest(format!("failed to build request: {e}")))?;

            let response = self.client.send_async(request).await?;
            let status = response.status();

            if status.is_redirection() {
                if redirect_count >= self.policy.max_redirects {
                    return Err(RelayError::PolicyViolation(format!(
                        "too many redirects: exceeded limit of {}",
                        self.policy.max_redirects
                    )));
                }

                let location = response
                    .headers()
                    .get(http::header::LOCATION)
                    .ok_or_else(|| {
                        RelayError::InvalidRequest("redirect response missing Location header".into())
                    })?
                    .to_str()
                    .map_err(|_| {
                        RelayError::InvalidRequest("invalid non-ASCII Location header".into())
                    })?;

                let current_parsed = url::Url::parse(&current_url).map_err(|e| {
                    RelayError::InvalidRequest(format!("failed to parse current URL: {e}"))
                })?;

                let resolved_url = current_parsed.join(location).map_err(|e| {
                    RelayError::InvalidRequest(format!("failed to resolve redirect Location '{location}': {e}"))
                })?;

                let validated = self.policy.validate_url(resolved_url.as_str())?;

                match status.as_u16() {
                    301 | 302 => {
                        // For standard redirects, keep GET/HEAD and Range
                    }
                    303 => {
                        // 303 See Other: convert to GET, drop Range
                        current_method = RelayMethod::Get;
                        current_range = None;
                        current_if_range = None;
                    }
                    307 | 308 => {
                        // Temporary/Permanent redirect: preserve method and headers
                    }
                    _ => {}
                }

                current_url = validated.to_string();
                redirect_count += 1;
                continue;
            }

            let (parts, body) = response.into_parts();
            break (parts, body, current_url);
        };

        let status = final_parts.status;

        // Upstream pre-header error check:
        // 200, 206, 416, and other success statuses pass through; 4xx/5xx become UpstreamPreHeader
        if (status.is_client_error() || status.is_server_error())
            && status != http::StatusCode::RANGE_NOT_SATISFIABLE
        {
            return Err(RelayError::UpstreamPreHeader {
                status: status.as_u16(),
                message: format!("upstream HTTP error: {status}"),
            });
        }

        let headers = preserve_response_headers(&final_parts.headers);
        let content_length = final_parts
            .headers
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok());

        let meta = RelayResponseMeta {
            status,
            headers,
            content_length,
            final_url,
        };

        let cancel_token = RelayCancellationToken::new();

        if current_method == RelayMethod::Head {
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            self.counters.inc_active_streams();
            let body = RelayBodyStream {
                receiver: rx,
                cancel_token: cancel_token.clone(),
                counters: self.counters.clone(),
                completed: true,
            };
            return Ok(MediaStreamResponse {
                meta,
                body,
                cancel_token,
            });
        }

        // GET response body streaming
        let (tx, rx) = tokio::sync::mpsc::channel(self.config.max_buffered_chunks);
        self.counters.inc_active_streams();

        let body = RelayBodyStream {
            receiver: rx,
            cancel_token: cancel_token.clone(),
            counters: self.counters.clone(),
            completed: false,
        };

        let task_cancel = cancel_token.clone();
        let task_counters = self.counters.clone();
        let chunk_size = self.config.chunk_size;
        let read_timeout = self.config.read_timeout;
        let expected_len = meta.content_length;

        tokio::spawn(async move {
            use futures_lite::io::AsyncReadExt;

            let mut upstream_body = final_body;
            let mut bytes_sent: u64 = 0;
            let mut buf = vec![0u8; chunk_size];
            let mut mid_stream_failed = false;

            loop {
                if task_cancel.is_cancelled() {
                    break;
                }

                let read_result = tokio::select! {
                    _ = task_cancel.cancelled() => {
                        break;
                    }
                    res = tokio::time::timeout(read_timeout, upstream_body.read(&mut buf)) => {
                        res
                    }
                };

                let n = match read_result {
                    Ok(Ok(0)) => {
                        // Normal EOF reached
                        break;
                    }
                    Ok(Ok(n)) => n,
                    Ok(Err(io_err)) => {
                        let err = RelayError::MidStreamFailure {
                            bytes_sent,
                            expected_bytes: expected_len,
                            reason: format!("upstream I/O error: {io_err}"),
                        };
                        task_counters.inc_mid_stream_failures();
                        let _ = tx.send(Err(err)).await;
                        mid_stream_failed = true;
                        break;
                    }
                    Err(_) => {
                        let err = RelayError::MidStreamFailure {
                            bytes_sent,
                            expected_bytes: expected_len,
                            reason: format!("upstream read timed out after {read_timeout:?}"),
                        };
                        task_counters.inc_mid_stream_failures();
                        let _ = tx.send(Err(err)).await;
                        mid_stream_failed = true;
                        break;
                    }
                };

                let chunk = bytes::Bytes::copy_from_slice(&buf[..n]);
                bytes_sent += n as u64;
                task_counters.add_bytes_forwarded(n as u64);

                tokio::select! {
                    _ = task_cancel.cancelled() => {
                        break;
                    }
                    send_res = tx.send(Ok(chunk)) => {
                        if send_res.is_err() {
                            // Downstream caller dropped stream
                            break;
                        }
                    }
                }
            }

            // Detect premature EOF if Content-Length was advertised on GET
            if !mid_stream_failed && !task_cancel.is_cancelled() {
                if let Some(expected) = expected_len {
                    if bytes_sent < expected {
                        let err = RelayError::MidStreamFailure {
                            bytes_sent,
                            expected_bytes: Some(expected),
                            reason: format!(
                                "upstream prematurely closed connection: received {bytes_sent} of {expected} bytes"
                            ),
                        };
                        task_counters.inc_mid_stream_failures();
                        let _ = tx.send(Err(err)).await;
                    }
                }
            }
        });

        Ok(MediaStreamResponse {
            meta,
            body,
            cancel_token,
        })
    }
}

fn preserve_response_headers(upstream_headers: &http::HeaderMap) -> http::HeaderMap {
    let mut headers = http::HeaderMap::new();
    const PRESERVED_NAMES: &[http::header::HeaderName] = &[
        http::header::CONTENT_TYPE,
        http::header::CONTENT_LENGTH,
        http::header::CONTENT_RANGE,
        http::header::ACCEPT_RANGES,
        http::header::ETAG,
        http::header::LAST_MODIFIED,
        http::header::CACHE_CONTROL,
    ];
    for name in PRESERVED_NAMES {
        for val in upstream_headers.get_all(name) {
            headers.append(name.clone(), val.clone());
        }
    }
    headers
}
