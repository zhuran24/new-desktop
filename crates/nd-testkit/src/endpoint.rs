use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use futures::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// Missing agent header denotes the main conversation; matching is exact, without fallbacks.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Route {
    pub agent: Option<String>,
    pub model: String,
}
impl Route {
    pub fn new(agent: Option<&str>, model: &str) -> Self {
        Self {
            agent: agent.map(str::to_owned),
            model: model.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ModelRequest {
    pub id: u64,
    pub route: Route,
    /// Actual API request body. Authentication headers are never retained.
    pub body: Value,
}

pub struct ModelReply {
    content: Vec<Value>,
    stop_reason: &'static str,
    gate: Option<tokio::sync::oneshot::Receiver<()>>,
    chunk_chars: usize,
    pause_ms: u64,
}
impl ModelReply {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![json!({"type":"text","text":text.into()})],
            stop_reason: "end_turn",
            gate: None,
            chunk_chars: usize::MAX,
            pause_ms: 0,
        }
    }
    /// Emit a real SSE delta per character chunk, with configurable offline pacing.
    pub fn streaming_text(text: impl Into<String>, chunk_chars: usize, pause_ms: u64) -> Self {
        let mut reply = Self::text(text);
        reply.chunk_chars = chunk_chars.max(1);
        reply.pause_ms = pause_ms;
        reply
    }
    pub fn tool(id: &str, name: &str, input: Value) -> Self {
        Self {
            content: vec![json!({"type":"tool_use","id":id,"name":name,"input":input})],
            stop_reason: "tool_use",
            gate: None,
            chunk_chars: usize::MAX,
            pause_ms: 0,
        }
    }
}

/// Consuming release opens one planned response. Dropping it cancels that response.
pub struct ResponseGate(tokio::sync::oneshot::Sender<()>);
impl ResponseGate {
    pub fn release(self) {
        let _ = self.0.send(());
    }
}

#[derive(Default)]
struct Script {
    replies: HashMap<Route, VecDeque<ModelReply>>,
    requests: Vec<ModelRequest>,
    any_agent: HashMap<String, VecDeque<ModelReply>>,
}

#[derive(Clone)]
struct EndpointState {
    script: Arc<Mutex<Script>>,
    shutdown: tokio::sync::watch::Receiver<bool>,
}

pub struct ClaudeEndpoint {
    socket: PathBuf,
    script: Arc<Mutex<Script>>,
    shutdown: tokio::sync::watch::Sender<bool>,
    _task: tokio::task::JoinHandle<()>,
}
impl ClaudeEndpoint {
    pub async fn bind(socket: impl AsRef<Path>) -> crate::Result<Self> {
        let socket = socket.as_ref().to_owned();
        let listener = tokio::net::UnixListener::bind(&socket)?;
        let script = Arc::new(Mutex::new(Script::default()));
        let (shutdown, mut stop) = tokio::sync::watch::channel(false);
        let router = Router::new()
            .route("/v1/messages", post(messages))
            .with_state(EndpointState {
                script: script.clone(),
                shutdown: stop.clone(),
            });
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = stop.changed().await;
                })
                .await;
        });
        Ok(Self {
            socket,
            script,
            shutdown,
            _task: task,
        })
    }
    pub fn socket(&self) -> &Path {
        &self.socket
    }
    pub fn enqueue(&self, route: Route, reply: ModelReply) {
        self.script
            .lock()
            .unwrap()
            .replies
            .entry(route)
            .or_default()
            .push_back(reply);
    }
    /// Explicit model-wide plan for dynamically generated Workflow agent IDs.
    /// An exact agent/model route takes precedence; absence of either plan still fails closed.
    pub fn enqueue_any_agent(&self, model: &str, reply: ModelReply) {
        self.script
            .lock()
            .unwrap()
            .any_agent
            .entry(model.into())
            .or_default()
            .push_back(reply);
    }
    pub fn enqueue_held(&self, route: Route, mut reply: ModelReply) -> ResponseGate {
        let (tx, rx) = tokio::sync::oneshot::channel();
        reply.gate = Some(rx);
        self.enqueue(route, reply);
        ResponseGate(tx)
    }
    pub async fn wait_for_requests(
        &self,
        route: &Route,
        count: usize,
        timeout: std::time::Duration,
    ) -> crate::Result<()> {
        tokio::time::timeout(timeout, async {
            while self.count(route) < count {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| {
            format!(
                "expected {count} requests for {route:?}; observed {}",
                self.count(route)
            )
        })?;
        Ok(())
    }
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.script.lock().unwrap().requests.clone()
    }
    pub fn count(&self, route: &Route) -> usize {
        self.script
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| &r.route == route)
            .count()
    }
}
impl Drop for ClaudeEndpoint {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        let _ = std::fs::remove_file(&self.socket);
    }
}

async fn messages(
    State(mut state): State<EndpointState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(model) = body["model"].as_str() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let route = Route::new(
        headers
            .get("x-claude-code-agent-id")
            .and_then(|h| h.to_str().ok()),
        model,
    );
    let streaming = body["stream"] == true;
    let (id, reply) = {
        let mut script = state.script.lock().unwrap();
        let id = script.requests.len() as u64 + 1;
        script.requests.push(ModelRequest {
            id,
            route: route.clone(),
            body,
        });
        (
            id,
            script
                .replies
                .get_mut(&route)
                .and_then(VecDeque::pop_front)
                .or_else(|| {
                    script
                        .any_agent
                        .get_mut(&route.model)
                        .and_then(VecDeque::pop_front)
                }),
        )
    };
    let Some(mut reply) = reply else {
        return (StatusCode::CONFLICT, Json(json!({"type":"error","error":{"type":"invalid_request_error","message":"unplanned model request"}}))).into_response();
    };
    if let Some(gate) = reply.gate.take() {
        let released = tokio::select! {
            biased;
            _ = state.shutdown.changed() => false,
            result = gate => result.is_ok(),
        };
        if !released {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"response gate cancelled"})),
            )
                .into_response();
        }
    }
    let message = json!({"id":format!("msg_nd_{id}"),"type":"message","role":"assistant","model":route.model,
        "content":reply.content,"stop_reason":reply.stop_reason,"stop_sequence":null,
        "usage":{"input_tokens":10,"output_tokens":10}});
    if !streaming {
        return Json(message).into_response();
    }
    let mut start = message.clone();
    start["content"] = json!([]);
    start["stop_reason"] = Value::Null;
    start["usage"]["output_tokens"] = json!(0);
    let mut events = vec![json!({"type":"message_start","message":start})];
    for (index, block) in reply.content.iter().enumerate() {
        let (start, delta) = if block["type"] == "tool_use" {
            (
                json!({"type":"tool_use","id":block["id"],"name":block["name"],"input":{}}),
                json!({"type":"input_json_delta","partial_json":block["input"].to_string()}),
            )
        } else {
            (
                json!({"type":"text","text":""}),
                json!({"type":"text_delta","text":block["text"]}),
            )
        };
        events.push(json!({"type":"content_block_start","index":index,"content_block":start}));
        if delta["type"] == "text_delta" {
            let chars = delta["text"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .collect::<Vec<_>>();
            for chunk in chars.chunks(reply.chunk_chars) {
                events.push(json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":chunk.iter().collect::<String>()}}));
            }
        } else {
            events.push(json!({"type":"content_block_delta","index":index,"delta":delta}));
        }
        events.push(json!({"type":"content_block_stop","index":index}));
    }
    events.push(json!({"type":"message_delta","delta":{"stop_reason":reply.stop_reason,"stop_sequence":null},"usage":{"output_tokens":10}}));
    events.push(json!({"type":"message_stop"}));
    let pause_ms = reply.pause_ms;
    axum::response::Sse::new(futures::stream::iter(events).then(move |event| async move {
        if pause_ms > 0 && event["type"] == "content_block_delta" {
            tokio::time::sleep(std::time::Duration::from_millis(pause_ms)).await;
        }
        Ok::<_, std::convert::Infallible>(
            axum::response::sse::Event::default()
                .event(event["type"].as_str().unwrap())
                .data(event.to_string()),
        )
    }))
    .into_response()
}
