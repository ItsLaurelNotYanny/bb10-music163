//! Service-agnostic media relay client library.
//!
//! `media-relay` provides a resilient, bounded, and policy-guarded HTTP media relay
//! client designed for proxying audio and video streams from upstream content delivery
//! networks (CDNs) to constrained local consumers (such as BlackBerry 10 media players).
//!
//! # Key Architectural Concepts
//!
//! ## Ownership and Lifecycle
//! - [`MediaRelay`] is an immutable, cloneable client holding an HTTP engine, destination
//!   policy, and atomic traffic counters. It can be shared across tasks or threads safely.
//! - Calling [`MediaRelay::open`] or [`MediaRelay::open_with_refresh`] initiates an upstream
//!   connection and returns a [`MediaStreamResponse`] containing response metadata, the
//!   bounded chunk stream ([`RelayBodyStream`]), and a [`RelayCancellationToken`].
//! - [`RelayBodyStream`] owns an internal drop guard that automatically decrements
//!   `active_streams` and notifies cancellation if dropped before reaching EOF or failure.
//!
//! ## Backpressure and Bounded Memory
//! - Streams are decoupled from upstream network reading via a bounded channel of size
//!   `max_buffered_chunks` (default: 4 chunks of 16 KiB = 64 KiB peak buffered per active stream).
//! - If the downstream caller is slow, the bounded channel fills up, causing the background reader
//!   to wait before pulling more chunks from the upstream socket.
//!
//! ## Cancellation Semantics
//! - Streams can be cancelled in two ways:
//!   1. Explicitly via [`RelayCancellationToken::cancel`].
//!   2. Implicitly by dropping [`RelayBodyStream`].
//! - In either case, the background reader task terminates immediately, resources are reclaimed,
//!   and `active_streams` returns to zero.
//!
//! ## Error Semantics and Stream Reliability
//! - Errors occurring **before** response headers are returned to the caller are categorized as
//!   [`RelayError::UpstreamPreHeader`] or [`RelayError::PolicyViolation`]. If configured,
//!   [`MediaRelay::open_with_refresh`] will automatically attempt a single URL refresh on
//!   expired statuses (401, 403, 404, 410).
//! - Errors occurring **after** response headers have already been delivered are categorized as
//!   [`RelayError::MidStreamFailure`]. Mid-stream failures are **never** restarted from byte 0,
//!   preventing audio replay loops and corrupted media files.

pub mod counters;
pub mod error;
pub mod policy;
pub mod relay;

pub use counters::{MediaTrafficCounters, MediaTrafficSnapshot};
pub use error::RelayError;
pub use policy::{
    is_blocked_ip, is_blocked_ipv4, is_blocked_ipv6, is_forbidden_upstream_header,
    sanitize_upstream_headers, DestinationPolicy, FORBIDDEN_UPSTREAM_HEADERS,
};
pub use relay::{
    MediaRelay, MediaSourceRequest, MediaStreamResponse, RelayBodyStream, RelayCancellationToken,
    RelayConfig, RelayMethod, RelayResponseMeta,
};
