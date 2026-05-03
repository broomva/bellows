//! `bellows-model` — LLM provider connectors.
//!
//! Provider menu:
//! - [`MockProvider`] — in-process echo, used in tests and examples without
//!   network or API keys.
//! - [`AnthropicProvider`] — calls Anthropic's Messages API. Supports both
//!   `ANTHROPIC_API_KEY` (`x-api-key`) and the Claude Code OAuth token
//!   (`Authorization: Bearer sk-ant-oat01-...`) authentication paths.
//!
//! Streaming is not yet wired — `stream` returns the same content as
//! `complete` framed as a single `TextDelta` + `EndTurn`. Real streaming +
//! tool-use deltas land in v0.2 (see `docs/ROADMAP.md`).

pub mod anthropic;
pub mod mock;

pub use anthropic::{AnthropicAuth, AnthropicProvider};
pub use mock::MockProvider;
