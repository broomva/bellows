//! End-to-end tests for `AnthropicProvider::stream` against a wiremock
//! upstream that fakes Anthropic's SSE wire protocol.
//!
//! These tests exercise the full pipeline:
//! 1. We build the same JSON request the real provider sends.
//! 2. wiremock returns a realistic `Content-Type: text/event-stream`
//!    response with the same multi-event shape Anthropic emits.
//! 3. The provider's bytes_stream + eventsource-stream parser pulls
//!    each event apart, and `StreamState` projects them into our
//!    `ModelStreamEvent` taxonomy.
//!
//! We assert event-for-event so that the projection table in
//! `crate::anthropic` stays honest. The tests must NOT introduce
//! tolerance for "approximately right" output — every contract event
//! is load-bearing for the chat UI.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bellows_core::{
    ModelProvider, ModelRequest, ModelStreamEvent, ModelUsage, StopReason, model::ModelStream,
};
use bellows_model::{AnthropicAuth, AnthropicProvider};
use futures::StreamExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider_pointing_at(server: &MockServer) -> AnthropicProvider {
    AnthropicProvider::new(AnthropicAuth::ApiKey("test-key".to_string()))
        .with_base_url(server.uri())
}

fn empty_request() -> ModelRequest {
    ModelRequest {
        model: "claude-test-model".to_string(),
        messages: Vec::new(),
        role: None,
        tools: Vec::new(),
        max_tokens: Some(64),
        temperature: None,
        stop: Vec::new(),
    }
}

/// Build a single Anthropic SSE event as it would appear on the wire.
/// Returns `event: <name>\ndata: <json>\n\n` so the eventsource parser
/// frames it correctly.
fn sse(event: &str, data: &str) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

async fn collect(
    stream: ModelStream,
) -> Vec<std::result::Result<ModelStreamEvent, bellows_core::BellowsError>> {
    stream.collect::<Vec<_>>().await
}

#[tokio::test]
async fn stream_emits_text_delta_then_end_turn() {
    let server = MockServer::start().await;

    let body = [
        sse(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_01","type":"message","role":"assistant","content":[],"model":"claude-test-model","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#,
        ),
        sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        ),
        sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":", world"}}"#,
        ),
        sse(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        sse(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#,
        ),
        sse("message_stop", r#"{"type":"message_stop"}"#),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = provider_pointing_at(&server);
    let stream = provider.stream(empty_request()).await.unwrap();
    let events: Vec<_> = collect(stream)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    assert_eq!(events.len(), 3, "events: {events:?}");
    match &events[0] {
        ModelStreamEvent::TextDelta { text } => assert_eq!(text, "Hello"),
        other => panic!("event 0 should be TextDelta('Hello'); got {other:?}"),
    }
    match &events[1] {
        ModelStreamEvent::TextDelta { text } => assert_eq!(text, ", world"),
        other => panic!("event 1 should be TextDelta(', world'); got {other:?}"),
    }
    match &events[2] {
        ModelStreamEvent::EndTurn { stop_reason, usage } => {
            assert_eq!(*stop_reason, StopReason::EndTurn);
            // message_delta only carried output_tokens=5
            let u = usage.as_ref().expect("EndTurn carries usage");
            assert_eq!(u.output_tokens, 5);
        }
        other => panic!("event 2 should be EndTurn; got {other:?}"),
    }
}

#[tokio::test]
async fn stream_emits_tool_use_start_and_input_json_delta() {
    let server = MockServer::start().await;

    let body = [
        sse(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_02","type":"message","role":"assistant","content":[],"model":"claude-test-model","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":12,"output_tokens":1}}}"#,
        ),
        sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_01XYZ","name":"fs_read","input":{}}}"#,
        ),
        sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
        ),
        sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"/etc/hosts\"}"}}"#,
        ),
        sse(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        sse(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":42}}"#,
        ),
        sse("message_stop", r#"{"type":"message_stop"}"#),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = provider_pointing_at(&server);
    let stream = provider.stream(empty_request()).await.unwrap();
    let events: Vec<_> = collect(stream)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    assert_eq!(events.len(), 4, "events: {events:?}");
    match &events[0] {
        ModelStreamEvent::ToolCallStart { id, name } => {
            assert_eq!(id, "toolu_01XYZ");
            assert_eq!(name, "fs_read");
        }
        other => panic!("event 0 should be ToolCallStart; got {other:?}"),
    }
    match &events[1] {
        ModelStreamEvent::ToolCallDelta { id, arguments_json } => {
            assert_eq!(id, "toolu_01XYZ");
            assert_eq!(arguments_json, "{\"path\":");
        }
        other => panic!("event 1 should be ToolCallDelta; got {other:?}"),
    }
    match &events[2] {
        ModelStreamEvent::ToolCallDelta { id, arguments_json } => {
            assert_eq!(id, "toolu_01XYZ");
            assert_eq!(arguments_json, "\"/etc/hosts\"}");
        }
        other => panic!("event 2 should be ToolCallDelta; got {other:?}"),
    }
    match &events[3] {
        ModelStreamEvent::EndTurn { stop_reason, .. } => {
            assert_eq!(*stop_reason, StopReason::ToolUse);
        }
        other => panic!("event 3 should be EndTurn(ToolUse); got {other:?}"),
    }
}

#[tokio::test]
async fn stream_propagates_inline_error_event() {
    let server = MockServer::start().await;

    let body = [
        sse(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_03","type":"message","role":"assistant","content":[],"model":"claude-test-model","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1}}}"#,
        ),
        sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"thinking"}}"#,
        ),
        sse(
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        ),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = provider_pointing_at(&server);
    let stream = provider.stream(empty_request()).await.unwrap();
    let events = collect(stream).await;

    // text delta for "thinking", then Err
    assert_eq!(events.len(), 2, "events: {events:?}");
    let _text = events[0].as_ref().unwrap();
    let err = events[1].as_ref().unwrap_err();
    let s = err.to_string();
    assert!(s.contains("overloaded_error"), "stringified err: {s}");
    assert!(s.contains("Overloaded"), "stringified err: {s}");
}

#[tokio::test]
async fn stream_returns_http_error_before_first_event() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("content-type", "application/json")
                .set_body_string(
                    r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#,
                ),
        )
        .mount(&server)
        .await;

    let provider = provider_pointing_at(&server);
    let result = provider.stream(empty_request()).await;
    assert!(result.is_err(), "expected stream() to fail on HTTP 429");
    let err = result.err().unwrap().to_string();
    assert!(err.contains("HTTP 429"), "got: {err}");
    assert!(err.contains("rate_limit_error"), "got: {err}");
}

#[tokio::test]
async fn stream_falls_back_to_end_turn_when_message_delta_omits_stop_reason() {
    // Defensive: providers occasionally drop stop_reason on truncation.
    // We must still emit a terminal EndTurn so downstream consumers
    // never hang.
    let server = MockServer::start().await;
    let body = [
        sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#,
        ),
        sse("message_stop", r#"{"type":"message_stop"}"#),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = provider_pointing_at(&server);
    let stream = provider.stream(empty_request()).await.unwrap();
    let events: Vec<_> = collect(stream)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[1],
        ModelStreamEvent::EndTurn {
            stop_reason: StopReason::EndTurn,
            ..
        }
    ));
}

#[tokio::test]
async fn stream_uses_oauth_bearer_when_configured() {
    let server = MockServer::start().await;

    let body = [
        sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        ),
        sse(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
        ),
        sse("message_stop", r#"{"type":"message_stop"}"#),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer oat-token"))
        .and(header("anthropic-beta", "oauth-2025-04-20"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(AnthropicAuth::OAuthBearer("oat-token".to_string()))
        .with_base_url(server.uri());
    let stream = provider.stream(empty_request()).await.unwrap();
    let events = collect(stream).await;
    // 1 text + 1 end-turn
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(std::result::Result::is_ok));
    assert!(matches!(
        events[1].as_ref().unwrap(),
        ModelStreamEvent::EndTurn { .. }
    ));
}

/// Snapshot the canonical Anthropic SSE → ModelStreamEvent projection
/// across a representative mixed stream (text + tool-use + final). If
/// this snapshot drifts, somebody has changed the contract surface and
/// the chat UI's wire format will move with it — review carefully.
#[tokio::test]
async fn stream_projection_snapshot() {
    let server = MockServer::start().await;

    // Mixed stream: text → tool_use → text, ending with stop_reason=tool_use.
    // Mirrors the real shape Anthropic emits when the model narrates before
    // a tool call.
    let body = [
        sse(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_snap","type":"message","role":"assistant","content":[],"model":"claude-test","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":20,"output_tokens":1}}}"#,
        ),
        sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Let me check."}}"#,
        ),
        sse(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_AAA","name":"fs_list","input":{}}}"#,
        ),
        sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\".\"}"}}"#,
        ),
        sse(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":1}"#,
        ),
        sse(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":11}}"#,
        ),
        sse("message_stop", r#"{"type":"message_stop"}"#),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = provider_pointing_at(&server);
    let stream = provider.stream(empty_request()).await.unwrap();
    let events: Vec<ModelStreamEvent> = collect(stream)
        .await
        .into_iter()
        .map(std::result::Result::unwrap)
        .collect();

    // Serialize through the public ModelStreamEvent serde shape so the
    // snapshot tracks the actual wire-bound representation.
    let json = serde_json::to_value(&events).unwrap();
    insta::assert_json_snapshot!("anthropic_stream_mixed", json);
}

#[tokio::test]
async fn stream_preserves_usage_input_tokens_across_events() {
    // Anthropic emits input_tokens on message_start (which we don't surface)
    // and output_tokens on message_delta. We only forward what message_delta
    // gives us — input_tokens may legitimately be 0 in the EndTurn
    // event. This test pins that contract so a future implementation
    // that decides to read message_start usage doesn't silently change it.
    let server = MockServer::start().await;
    let body = [
        sse(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_99","type":"message","role":"assistant","content":[],"model":"x","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":42,"output_tokens":1}}}"#,
        ),
        sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}"#,
        ),
        sse(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
        ),
        sse("message_stop", r#"{"type":"message_stop"}"#),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let provider = provider_pointing_at(&server);
    let stream = provider.stream(empty_request()).await.unwrap();
    let events: Vec<_> = collect(stream)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    let last = events.last().unwrap();
    let ModelStreamEvent::EndTurn { usage, .. } = last else {
        panic!("expected EndTurn, got {last:?}")
    };
    let u: &ModelUsage = usage.as_ref().unwrap();
    assert_eq!(u.output_tokens, 7);
    // input_tokens defaults to 0 because we don't surface message_start usage
    assert_eq!(u.input_tokens, 0);
}
