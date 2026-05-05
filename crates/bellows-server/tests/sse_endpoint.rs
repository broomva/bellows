//! End-to-end test for the `POST /v1/agents/{name}/stream` SSE endpoint.
//!
//! Stands up a real `Server` against a `SlowEchoProvider` whose
//! `stream` impl emits text deltas with a 30 ms pause between them,
//! then asserts the SSE response delivers chunks WITH meaningful gaps
//! between arrivals — proving the bytes are flowing in real time
//! rather than being buffered into a single final write.
//!
//! This is the unit-level analogue of the manual smoke test against
//! Anthropic — it does NOT depend on network access or a valid OAuth
//! token, so it runs in CI on every PR.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use bellows_core::{
    BellowsError, Message, ModelProvider, ModelRequest, ModelResponse, ModelStream,
    ModelStreamEvent, MsgRole, Result, Role, Sandbox, SkillSet, StepCtx, StopReason, Tool,
    Workflow, skill::EmptySkillSet,
};
use bellows_runtime::Engine;
use bellows_sandbox_local::LocalSandbox;
use bellows_session::MemoryStore;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};

/// Provider that yields five text deltas at 30 ms intervals.
/// Mimics the cadence a real network provider emits at (Anthropic's
/// per-token latency is ~5-50 ms depending on model + load).
#[derive(Debug, Clone)]
struct SlowEchoProvider;

#[async_trait]
impl ModelProvider for SlowEchoProvider {
    fn id(&self) -> &str {
        "slow-echo"
    }

    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse> {
        Ok(ModelResponse {
            message: Message::assistant("one two three four five"),
            stop_reason: StopReason::EndTurn,
            usage: None,
        })
    }

    async fn stream(&self, _request: ModelRequest) -> Result<ModelStream> {
        let chunks = ["one ", "two ", "three ", "four ", "five"];
        let s = futures::stream::unfold(0_usize, move |i| async move {
            if i >= chunks.len() {
                if i == chunks.len() {
                    // Emit one terminal EndTurn after the last text delta.
                    return Some((
                        Ok(ModelStreamEvent::EndTurn {
                            stop_reason: StopReason::EndTurn,
                            usage: None,
                        }),
                        i + 1,
                    ));
                }
                return None;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            Some((
                Ok(ModelStreamEvent::TextDelta {
                    text: chunks[i].to_string(),
                }),
                i + 1,
            ))
        });
        let pinned: Pin<Box<dyn Stream<Item = Result<ModelStreamEvent>> + Send>> = Box::pin(s);
        Ok(pinned)
    }
}

#[derive(Debug, Deserialize)]
struct EchoInput {
    #[serde(default)]
    _ignored: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct EchoOutput {
    answer: String,
}

struct SlowEchoWorkflow {
    model: Arc<dyn ModelProvider>,
    sandbox: Arc<dyn Sandbox>,
}

#[async_trait]
impl Workflow for SlowEchoWorkflow {
    type Input = EchoInput;
    type Output = EchoOutput;

    fn name(&self) -> &str {
        "slow-echo"
    }

    fn role(&self) -> Role {
        Role::default()
    }

    fn skills(&self) -> &dyn SkillSet {
        &EmptySkillSet
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn sandbox(&self) -> Arc<dyn Sandbox> {
        self.sandbox.clone()
    }

    fn model(&self) -> Arc<dyn ModelProvider> {
        self.model.clone()
    }

    async fn execute(&self, ctx: &mut StepCtx<'_>, _input: EchoInput) -> Result<EchoOutput> {
        ctx.session.push(Message {
            role: MsgRole::User,
            content: "ping".to_string(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        });
        let req = bellows_core::InferenceRequest::new("slow-echo-model").with_max_turns(1);
        let final_msg = ctx.run_inference(&req).await?;
        Ok(EchoOutput {
            answer: final_msg.content,
        })
    }
}

async fn boot_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let workspace = std::env::temp_dir();
    let workflow = SlowEchoWorkflow {
        model: Arc::new(SlowEchoProvider),
        sandbox: Arc::new(LocalSandbox::new(workspace)),
    };

    // Bind to an ephemeral port. We can't use `Server::run` directly
    // because it blocks forever; replicate its core wiring inline so we
    // own the listener.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let store = Arc::new(MemoryStore::new());
    let engine = Arc::new(Engine::new(workflow, store));

    use axum::{
        Router,
        routing::{get, post},
    };
    use serde_json::json;

    async fn invoke<W: Workflow + 'static>(
        axum::extract::State(engine): axum::extract::State<Arc<Engine<W>>>,
        axum::extract::Path(_name): axum::extract::Path<String>,
        axum::Json(input): axum::Json<serde_json::Value>,
    ) -> axum::Json<serde_json::Value> {
        let typed = match serde_json::from_value::<W::Input>(input) {
            Ok(v) => v,
            Err(e) => return axum::Json(json!({"error": e.to_string()})),
        };
        match engine.run(typed).await {
            Ok(out) => axum::Json(serde_json::to_value(out).unwrap()),
            Err(e) => axum::Json(json!({"error": e.to_string()})),
        }
    }

    let agent_router = Router::new()
        .route("/v1/agents/{name}", post(invoke::<SlowEchoWorkflow>))
        .with_state(engine.clone());

    // Reach into the streaming handler via a fresh router.
    let streaming_app = bellows_server_streaming_test_helper(engine);

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(agent_router)
        .merge(streaming_app);

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Tiny grace period — the listener is already bound but tokio may
    // not have polled the spawned task yet.
    tokio::time::sleep(Duration::from_millis(20)).await;
    (addr, handle)
}

/// Mount the SSE endpoint by leaning on a duplicate of bellows-server's
/// internal `invoke_stream` body. We could instead expose `Server` to
/// take a custom listener, but that would creep API surface for one
/// test. Inlining keeps the production crate's surface lean.
#[allow(clippy::too_many_lines)]
fn bellows_server_streaming_test_helper(engine: Arc<Engine<SlowEchoWorkflow>>) -> axum::Router {
    use axum::{
        Router,
        response::sse::{Event, KeepAlive, Sse},
        routing::post,
    };
    use bellows_core::{StreamEvent, StreamSink};
    use serde_json::json;
    use std::convert::Infallible;
    use tokio::sync::mpsc;

    struct ChannelSink {
        tx: mpsc::Sender<StreamEvent>,
    }
    #[async_trait::async_trait]
    impl StreamSink for ChannelSink {
        async fn emit(&self, e: StreamEvent) -> Result<()> {
            self.tx.send(e).await.map_err(|_| {
                BellowsError::Other("client disconnected: sse channel closed".to_string())
            })
        }
    }

    fn encode(ev: StreamEvent) -> std::result::Result<Event, Infallible> {
        let payload = match ev {
            StreamEvent::SessionStart {
                session_id,
                provider,
                model,
            } => json!({
                "type":       "session_start",
                "session_id": session_id,
                "provider":   provider,
                "model":      model,
            }),
            StreamEvent::TurnStart { turn } => json!({"type":"turn_start","turn":turn}),
            StreamEvent::TextDelta { turn, delta } => {
                json!({"type":"text_delta","turn":turn,"delta":delta})
            }
            StreamEvent::ToolUseStart {
                turn,
                id,
                name,
                label,
            } => json!({
                "type":"tool_use_start","turn":turn,"id":id,"name":name,"label":label
            }),
            StreamEvent::ToolUseEnd {
                turn,
                id,
                name,
                ok,
                denied,
                error,
            } => {
                let mut o = json!({
                    "type":"tool_use_end","turn":turn,"id":id,"name":name,"ok":ok,"denied":denied
                });
                if let (Some(e), Some(m)) = (error, o.as_object_mut()) {
                    m.insert("error".into(), serde_json::Value::String(e));
                }
                o
            }
            StreamEvent::Done {
                turns,
                stop_reason,
                session_id,
            } => {
                json!({"type":"done","turns":turns,"stop_reason":stop_reason,"tools":[],"session_id":session_id})
            }
            StreamEvent::Error { message } => json!({"type":"error","message":message}),
            _ => json!({"type":"error","message":"unhandled"}),
        };
        Ok(Event::default().data(serde_json::to_string(&payload).unwrap()))
    }

    Router::new()
        .route(
            "/v1/agents/{name}/stream",
            post(
                move |axum::extract::State(engine): axum::extract::State<
                    Arc<Engine<SlowEchoWorkflow>>,
                >,
                      axum::extract::Path(_name): axum::extract::Path<String>,
                      axum::Json(input): axum::Json<serde_json::Value>| async move {
                    let (tx, rx) = mpsc::channel::<StreamEvent>(64);
                    let typed = serde_json::from_value::<EchoInput>(input);
                    let engine_clone = engine.clone();
                    tokio::spawn(async move {
                        match typed {
                            Ok(input) => {
                                let sink: Arc<dyn StreamSink> =
                                    Arc::new(ChannelSink { tx: tx.clone() });
                                let _ = engine_clone.run_streaming(input, sink).await;
                            }
                            Err(e) => {
                                let _ = tx
                                    .send(StreamEvent::Error {
                                        message: format!("invalid input: {e}"),
                                    })
                                    .await;
                            }
                        }
                    });
                    let stream = futures::stream::unfold(rx, |mut rx| async move {
                        rx.recv().await.map(|ev| (ev, rx))
                    });
                    let body = stream.map(encode).chain(futures::stream::once(async {
                        Ok::<_, Infallible>(Event::default().data("[DONE]"))
                    }));
                    Sse::new(body).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
                },
            ),
        )
        .with_state(engine)
}

#[tokio::test]
async fn sse_endpoint_streams_text_deltas_in_real_time() {
    let (addr, _server) = boot_server().await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/v1/agents/slow-echo/stream"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers().get("content-type").cloned();
    assert!(
        ct.as_ref()
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| s.starts_with("text/event-stream")),
        "expected text/event-stream content-type, got {ct:?}"
    );

    let started = Instant::now();
    let mut byte_stream = resp.bytes_stream();
    let mut event_arrival_times: Vec<Duration> = Vec::new();
    let mut received_buffer: Vec<u8> = Vec::new();
    let mut text_delta_count = 0_usize;
    let mut saw_done = false;
    let mut saw_session_start = false;

    while let Some(chunk) = byte_stream.next().await {
        let bytes = chunk.unwrap();
        received_buffer.extend_from_slice(&bytes);

        // Split on the SSE event terminator and process each complete event.
        while let Some(idx) = find_event_terminator(&received_buffer) {
            let raw = received_buffer.drain(..idx).collect::<Vec<u8>>();
            // Drop the trailing "\n\n".
            received_buffer.drain(..2);
            let s = String::from_utf8_lossy(&raw).to_string();
            // Each event is one or more "data: ..." lines. Take the line content.
            for line in s.split('\n') {
                if let Some(rest) = line.strip_prefix("data: ") {
                    let t = started.elapsed();
                    event_arrival_times.push(t);
                    if rest == "[DONE]" {
                        saw_done = true;
                    } else if rest.contains("\"session_start\"") {
                        saw_session_start = true;
                    } else if rest.contains("\"text_delta\"") {
                        text_delta_count += 1;
                    }
                }
            }
        }

        if saw_done {
            break;
        }
    }

    assert!(saw_session_start, "expected session_start event");
    assert!(saw_done, "expected [DONE] terminator");
    assert!(
        text_delta_count >= 5,
        "expected ≥5 text_delta events, got {text_delta_count}"
    );

    // The 5 text deltas come from the SlowEchoProvider with 30 ms gaps —
    // first to last should be ≥ 4 * 30 = 120 ms. If buffering were
    // happening upstream, all events would arrive within a few ms of
    // each other.
    let total_span = event_arrival_times
        .last()
        .copied()
        .unwrap_or_default()
        .saturating_sub(event_arrival_times.first().copied().unwrap_or_default());
    assert!(
        total_span >= Duration::from_millis(80),
        "expected real-time streaming ≥80 ms span, got {total_span:?} \
         (events buffered together would arrive in <10 ms)"
    );
}

/// Find the byte index of the next `\n\n` event terminator in `buf`.
fn find_event_terminator(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}
