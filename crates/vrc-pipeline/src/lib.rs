//! VRChat WebSocket event stream (M2).
//!
//! [`Pipeline::connect`] keeps a `wss://pipeline.vrchat.cloud` connection open
//! and yields typed [`PipelineEvent`]s. The stream reconnects automatically
//! after drops with capped exponential backoff (1 s → 5 min) plus jitter
//! (RULES.md §3). Every handshake sends [`vrc_api::USER_AGENT`] and carries
//! the `auth` cookie as `?authToken=`; the token is never logged (RULES.md
//! §2, §4).

mod connect;
mod events;

pub use connect::{Pipeline, PipelineConfig, PipelineEvents, PipelineHandle, PIPELINE_URL};
pub use events::PipelineEvent;
