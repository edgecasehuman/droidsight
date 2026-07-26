#![recursion_limit = "256"]
mod a11y_diff;
mod adb;
mod adb_protocol;
mod app;
mod automation;
mod capabilities;
mod config;
mod crash_reports;
mod debug_exposure;
mod device_metrics;
mod element_snapshots;
mod events;
mod files;
mod forensics;
mod input;
mod intents;
mod logs;
mod network;
mod notifications;
mod recording;
mod response;
mod sensors;
mod sentinel;
mod session;
mod stream;
mod system;
mod tools;
mod vision;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProtocolState {
    AwaitingInitialize,
    AwaitingInitializedNotification,
    Ready,
}

struct ProtocolSession {
    state: ProtocolState,
}

impl ProtocolSession {
    fn new() -> Self {
        Self {
            state: ProtocolState::AwaitingInitialize,
        }
    }

    /// Admit messages in transport order before any request is spawned. This
    /// keeps lifecycle transitions deterministic while tool calls remain
    /// concurrently executable after initialization.
    fn admit(&mut self, request: &JsonRpcRequest) -> Result<(), Value> {
        match request.method.as_str() {
            "initialize" => {
                if request.id.is_none() {
                    return Err(json!({
                        "code": -32600,
                        "message": "Invalid Request: initialize must include an id"
                    }));
                }
                if self.state != ProtocolState::AwaitingInitialize {
                    return Err(json!({
                        "code": -32600,
                        "message": "Invalid Request: server is already initialized"
                    }));
                }
                self.state = ProtocolState::AwaitingInitializedNotification;
                Ok(())
            }
            "notifications/initialized" => {
                if request.id.is_some() {
                    return Err(json!({
                        "code": -32600,
                        "message": "Invalid Request: notifications/initialized must not include an id"
                    }));
                }
                if self.state != ProtocolState::AwaitingInitializedNotification {
                    return Err(json!({
                        "code": -32600,
                        "message": "Invalid Request: unexpected notifications/initialized"
                    }));
                }
                self.state = ProtocolState::Ready;
                Ok(())
            }
            "tools/list" | "tools/call" | "mcp.list_tools" | "mcp.call_tool" => {
                if self.state != ProtocolState::Ready {
                    return Err(json!({
                        "code": -32002,
                        "message": "Server not initialized"
                    }));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    params: Option<Value>,
    id: Option<Value>,
}

#[derive(Serialize, Deserialize, Debug)]
struct JsonRpcResponse {
    jsonrpc: String,
    result: Option<Value>,
    error: Option<Value>,
    id: Option<Value>,
}

#[derive(Serialize, Debug)]
#[serde(untagged)]
enum OutboundMessage {
    Single(JsonRpcResponse),
    Batch(Vec<JsonRpcResponse>),
}

#[derive(Debug)]
enum ParsedMessage {
    Single(JsonRpcRequest),
    Batch(Vec<Result<JsonRpcRequest, Value>>),
}

enum BatchWork {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
}

const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
enum InboundMessage {
    Line(String),
    TooLarge,
    InvalidUtf8,
    Eof,
}

/// Read one newline-delimited request without allowing an untrusted peer to
/// grow the input buffer beyond `max_bytes`. Oversized lines are drained so a
/// subsequent valid request can still be processed.
async fn read_bounded_line<R>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    max_bytes: usize,
) -> std::io::Result<InboundMessage>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;

    buffer.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if buffer.is_empty() {
                Ok(InboundMessage::Eof)
            } else {
                Ok(match String::from_utf8(std::mem::take(buffer)) {
                    Ok(line) => InboundMessage::Line(line),
                    Err(_) => InboundMessage::InvalidUtf8,
                })
            };
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        if buffer.len().saturating_add(consumed) > max_bytes {
            reader.consume(consumed);
            if newline.is_none() {
                loop {
                    let remaining = reader.fill_buf().await?;
                    if remaining.is_empty() {
                        break;
                    }
                    let next_newline = remaining.iter().position(|byte| *byte == b'\n');
                    let drained = next_newline.map_or(remaining.len(), |position| position + 1);
                    reader.consume(drained);
                    if next_newline.is_some() {
                        break;
                    }
                }
            }
            buffer.clear();
            return Ok(InboundMessage::TooLarge);
        }

        buffer.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(match String::from_utf8(std::mem::take(buffer)) {
                Ok(line) => InboundMessage::Line(line),
                Err(_) => InboundMessage::InvalidUtf8,
            });
        }
    }
}

fn register_tools() -> Arc<tools::ToolRegistry> {
    let mut registry = tools::ToolRegistry::new();
    registry.register(tools::input::InputActTool);
    registry.register(tools::app::AppManageTool);
    registry.register(tools::system::SystemControlTool);
    registry.register(tools::device::DeviceControlTool);
    registry.register(tools::network::NetworkControlTool);
    registry.register(tools::sensors::SensorControlTool);
    registry.register(tools::forensics::ForensicsControlTool);
    registry.register(tools::fs::FileSystemTool);
    registry.register(tools::logs::LogStreamTool);
    registry.register(tools::session::SessionTool);
    registry.register(tools::session::StopSessionTool);
    if config::Config::allow_arbitrary_shell() {
        registry.register(tools::shell::ShellTool);
        registry.register(tools::session::RunMacroTool);
    }
    registry.register(tools::intent::OpenUrlTool);
    registry.register(tools::intent::StartIntentTool);
    registry.register(tools::media::StartRecordingTool);
    registry.register(tools::media::StopRecordingTool);
    registry.register(tools::notifications::GetNotificationsTool);
    registry.register(tools::vision::VisionQueryTool);
    registry.register(tools::vision::VisionStreamTool);
    registry.register(tools::vision::GetViewHierarchyTool);
    registry.register(tools::automation::SmartWaitTool);
    registry.register(tools::logs::ReadRecentEventsTool);
    registry.register(tools::logs::LogFilterTool);
    registry.register(tools::sentinel::SentinelControlTool);
    registry.register(tools::gesture::RecordGestureTool);
    registry.register(tools::gesture::PlayGestureTool);
    registry.register(tools::instrumentation::AppInstrumentationTool);
    registry.register(tools::companion::CompanionTool);
    registry.register(tools::device::CheckHealthTool);
    registry.register(tools::device::CheckDebugExposureTool);
    registry.register(tools::atomic::AtomicTapTextTool);
    registry.register(tools::flow::RunFlowTool);
    Arc::new(registry)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure tracing without polluting stdout, which is reserved for MCP.
    let pid = std::process::id();
    let default_log_name = format!("mcp_{pid}.log");
    let mut log_guard = None;

    if let Some(path_str) = config::Config::debug_log_path() {
        let path = std::path::Path::new(&path_str);
        let file_appender = if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
            tracing_appender::rolling::never(
                parent,
                path.file_name()
                    .map(|f| format!("{}_{}", pid, f.to_string_lossy()))
                    .unwrap_or(default_log_name.clone()),
            )
        } else {
            tracing_appender::rolling::never(".", default_log_name.clone())
        };
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        log_guard = Some(guard);
        let filter = tracing_subscriber::EnvFilter::try_from_env("DROIDSIGHT_LOG")
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug"));
        tracing_subscriber::fmt()
            .with_writer(non_blocking)
            .with_ansi(false)
            .with_env_filter(filter)
            .init();
    } else {
        let filter = tracing_subscriber::EnvFilter::try_from_env("DROIDSIGHT_LOG")
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .with_env_filter(filter)
            .init();
    }

    tracing::info!("SERVER STARTING: v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Running concurrently with PID {}", pid);

    std::panic::set_hook(Box::new(|info| {
        let msg = match info.payload().downcast_ref::<&str>() {
            Some(s) => *s,
            None => match info.payload().downcast_ref::<String>() {
                Some(s) => &**s,
                None => "Box<Any>",
            },
        };
        tracing::error!("CRITICAL PANIC: {}", msg);
    }));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async_main())?;

    // Keep the non-blocking file writer guard alive through runtime shutdown.
    drop(log_guard);

    Ok(())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    use tokio::sync::mpsc;

    // The sentinel re-applies watched device state on a timer, including
    // unlocking the screen, so it stays off unless explicitly requested.
    // An explicit "1", not mere presence: `DROIDSIGHT_SENTINEL=0` has to mean off.
    if std::env::var("DROIDSIGHT_SENTINEL").as_deref() == Ok("1") {
        tracing::info!("Sentinel enabled via DROIDSIGHT_SENTINEL");
        tokio::spawn(async {
            sentinel::start_loop().await;
        });
    } else {
        tracing::info!("Sentinel disabled (set DROIDSIGHT_SENTINEL=1 to enable)");
    }

    let stream_manager = Arc::new(stream::StreamManager::new());
    // Maintain a continuously refreshed frame cache for instant screenshots
    // and input observations. Explicit stream stop/start calls remain
    // authoritative for clients that need to suspend capture temporarily.
    stream_manager.start();

    // Built once and shared for the process lifetime.
    tracing::info!("Initializing Tool Registry...");
    let tool_registry = register_tools();
    tracing::info!(
        "Tool Registry initialized with {} tools.",
        tool_registry.list_tools().len()
    );

    // Channel for responses (allows concurrent request processing)
    let (response_tx, mut response_rx) = mpsc::channel::<OutboundMessage>(100);

    // Spawn a blocking stdout writer task that serializes all responses.
    let writer_task = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::io::Write;
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();

        while let Some(resp) = response_rx.blocking_recv() {
            let resp_str = serde_json::to_string(&resp)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            writeln!(handle, "{resp_str}")?;
            handle.flush()?;
        }
        Ok(())
    });

    let (request_tx, mut request_rx) = mpsc::channel::<InboundMessage>(100);

    // Spawn async Input Reader using tokio::io::stdin()
    tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
        let mut buffer = Vec::with_capacity(8 * 1024);

        loop {
            match read_bounded_line(&mut reader, &mut buffer, MAX_REQUEST_BYTES).await {
                Ok(InboundMessage::Eof) => break,
                Ok(message) => {
                    if matches!(&message, InboundMessage::Line(line) if line.trim().is_empty()) {
                        continue;
                    }
                    if request_tx.send(message).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!("Stdin read error: {}", e);
                    break;
                }
            }
        }
    });

    const MAX_IN_FLIGHT_REQUESTS: usize = 32;
    let request_slots = Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT_REQUESTS));
    let mut request_tasks = tokio::task::JoinSet::new();
    let mut protocol_session = ProtocolSession::new();

    // Main Event Loop: Process requests from channel with timeout
    loop {
        while let Some(join_result) = request_tasks.try_join_next() {
            if let Err(error) = join_result {
                tracing::error!("Request task failed: {}", error);
            }
        }

        let Some(message) = request_rx.recv().await else {
            break;
        };

        let line = match message {
            InboundMessage::Line(line) => line,
            InboundMessage::TooLarge => {
                let _ = response_tx
                    .send(OutboundMessage::Single(JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        result: None,
                        error: Some(json!({
                            "code": -32600,
                            "message": "Request exceeds 16 MiB limit"
                        })),
                        id: Some(Value::Null),
                    }))
                    .await;
                continue;
            }
            InboundMessage::InvalidUtf8 => {
                let _ = response_tx
                    .send(OutboundMessage::Single(JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        result: None,
                        error: Some(json!({
                            "code": -32700,
                            "message": "Parse error: request is not valid UTF-8"
                        })),
                        id: Some(Value::Null),
                    }))
                    .await;
                continue;
            }
            InboundMessage::Eof => break,
        };

        // Tool arguments may contain PINs, Wi-Fi passwords, message bodies, or
        // clipboard data. Never copy the raw JSON-RPC payload into logs.
        tracing::debug!("Received JSON-RPC message ({} bytes)", line.len());

        match parse_message(&line) {
            Ok(ParsedMessage::Single(req)) => {
                tracing::debug!("Handling request: {}", req.method);

                if let Err(error) = protocol_session.admit(&req) {
                    if req.id.is_some() {
                        let _ = response_tx
                            .send(OutboundMessage::Single(JsonRpcResponse {
                                jsonrpc: "2.0".to_string(),
                                result: None,
                                error: Some(error),
                                id: req.id,
                            }))
                            .await;
                    } else {
                        tracing::warn!("Rejected notification: {}", error);
                    }
                    continue;
                }

                let sm = stream_manager.clone();
                let reg = tool_registry.clone();
                let tx = response_tx.clone();

                // Lifecycle and metadata methods are cheap and must complete in
                // input order. Only tool execution needs an in-flight task.
                if !matches!(req.method.as_str(), "tools/call" | "mcp.call_tool") {
                    if let Some(response) = handle_request(req, sm, reg).await {
                        let _ = tx.send(OutboundMessage::Single(response)).await;
                    }
                    continue;
                }

                let Ok(permit) = request_slots.clone().acquire_owned().await else {
                    break;
                };
                request_tasks.spawn(async move {
                    let _permit = permit;
                    let response = handle_request(req, sm, reg).await;
                    if let Some(resp) = response {
                        let _ = tx.send(OutboundMessage::Single(resp)).await;
                    }
                });
            }
            Ok(ParsedMessage::Batch(entries)) => {
                let work = prepare_batch(entries, &mut protocol_session);
                let sm = stream_manager.clone();
                let reg = tool_registry.clone();
                let tx = response_tx.clone();
                let Ok(permit) = request_slots.clone().acquire_owned().await else {
                    break;
                };
                request_tasks.spawn(async move {
                    let _permit = permit;
                    let responses = handle_batch(work, sm, reg).await;
                    if !responses.is_empty() {
                        let _ = tx.send(OutboundMessage::Batch(responses)).await;
                    }
                });
            }
            Err(error) => {
                tracing::error!("Rejected JSON-RPC input: {}", error);
                let _ = response_tx
                    .send(OutboundMessage::Single(JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        result: None,
                        error: Some(error),
                        id: Some(Value::Null),
                    }))
                    .await;
            }
        }
    }

    // EOF is a transport shutdown signal, not permission to discard requests
    // that were already accepted. Drain them and then wait for stdout to flush.
    while let Some(join_result) = request_tasks.join_next().await {
        if let Err(error) = join_result {
            tracing::error!("Request task failed during shutdown: {}", error);
        }
    }
    events::shutdown_event_monitor();
    drop(response_tx);
    match writer_task.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!("Stdout writer failed: {}", error),
        Err(error) => tracing::error!("Stdout writer task failed: {}", error),
    }

    // Graceful shutdown
    tracing::info!("SERVER SHUTDOWN: Clean exit (stdin channel closed)");

    Ok(())
}

fn parse_message(line: &str) -> Result<ParsedMessage, Value> {
    let value: Value = serde_json::from_str(line).map_err(|error| {
        json!({
            "code": -32700,
            "message": format!("Parse error: {}", error)
        })
    })?;

    match value {
        Value::Array(values) if values.is_empty() => {
            Err(invalid_request("batch must not be empty"))
        }
        Value::Array(values) => Ok(ParsedMessage::Batch(
            values.into_iter().map(parse_request_value).collect(),
        )),
        value => parse_request_value(value).map(ParsedMessage::Single),
    }
}

#[cfg(test)]
fn parse_request(line: &str) -> Result<JsonRpcRequest, Value> {
    let value: Value = serde_json::from_str(line).map_err(|error| {
        json!({
            "code": -32700,
            "message": format!("Parse error: {}", error)
        })
    })?;
    parse_request_value(value)
}

fn parse_request_value(value: Value) -> Result<JsonRpcRequest, Value> {
    let request: JsonRpcRequest = serde_json::from_value(value).map_err(|error| {
        json!({
            "code": -32600,
            "message": format!("Invalid Request: {}", error)
        })
    })?;

    if request.jsonrpc != "2.0" {
        return Err(json!({
            "code": -32600,
            "message": "Invalid Request: jsonrpc must be exactly '2.0'"
        }));
    }

    if !matches!(
        request.id,
        None | Some(Value::Null | Value::String(_) | Value::Number(_))
    ) {
        return Err(json!({
            "code": -32600,
            "message": "Invalid Request: id must be a string, number, or null"
        }));
    }

    Ok(request)
}

fn invalid_request(message: impl Into<String>) -> Value {
    json!({
        "code": -32600,
        "message": format!("Invalid Request: {}", message.into())
    })
}

fn error_response(error: Value, id: Option<Value>) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        result: None,
        error: Some(error),
        id: Some(id.unwrap_or(Value::Null)),
    }
}

fn prepare_batch(
    entries: Vec<Result<JsonRpcRequest, Value>>,
    session: &mut ProtocolSession,
) -> Vec<BatchWork> {
    entries
        .into_iter()
        .filter_map(|entry| match entry {
            Err(error) => Some(BatchWork::Response(error_response(error, None))),
            Ok(request) => match session.admit(&request) {
                Ok(()) => Some(BatchWork::Request(request)),
                Err(error) if request.id.is_some() => {
                    Some(BatchWork::Response(error_response(error, request.id)))
                }
                Err(error) => {
                    tracing::warn!("Rejected notification in batch: {}", error);
                    None
                }
            },
        })
        .collect()
}

async fn handle_batch(
    work: Vec<BatchWork>,
    stream_manager: Arc<stream::StreamManager>,
    registry: Arc<tools::ToolRegistry>,
) -> Vec<JsonRpcResponse> {
    let mut responses = Vec::new();
    for entry in work {
        match entry {
            BatchWork::Response(response) => responses.push(response),
            BatchWork::Request(request) => {
                if let Some(response) =
                    handle_request(request, stream_manager.clone(), registry.clone()).await
                {
                    responses.push(response);
                }
            }
        }
    }
    responses
}

async fn handle_request(
    req: JsonRpcRequest,
    stream_manager: Arc<stream::StreamManager>,
    registry: Arc<tools::ToolRegistry>,
) -> Option<JsonRpcResponse> {
    let is_notification = req.id.is_none();
    let result = match req.method.as_str() {
        "initialize" => Ok(initialize()),
        // `mcp.*` are legacy aliases for the spec-standard method names.
        "tools/list" | "mcp.list_tools" => Ok(list_tools(&registry)),
        "tools/call" | "mcp.call_tool" => {
            tracing::debug!("Calling tool (async)...");
            call_tool(req.params, stream_manager, &registry).await
        }
        "notifications/initialized" => Ok(json!({})),
        _ => Err(json!({"code": -32601, "message": format!("Method not found: {}", req.method)})),
    };

    if is_notification {
        if let Err(error) = result {
            tracing::warn!("Notification failed without a response channel: {}", error);
        }
        return None;
    }

    Some(match result {
        Ok(res) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(res),
            error: None,
            id: req.id,
        },
        Err(err) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(err),
            id: req.id,
        },
    })
}

fn initialize() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "droidsight",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn list_tools(registry: &tools::ToolRegistry) -> Value {
    // Use the passed registry, don't rebuild it
    let tools = registry.list_tools();
    json!({ "tools": tools })
}

fn mcp_tool_error(message: impl Into<String>) -> Value {
    json!({
        "content": [{"type": "text", "text": message.into()}],
        "isError": true
    })
}

async fn call_tool(
    params: Option<Value>,
    stream_manager: Arc<stream::StreamManager>,
    registry: &tools::ToolRegistry,
) -> Result<Value, Value> {
    let params = params.ok_or_else(|| {
        json!({
            "code": -32602,
            "message": "Invalid params: tools/call requires an object"
        })
    })?;
    let params = params.as_object().ok_or_else(|| {
        json!({
            "code": -32602,
            "message": "Invalid params: tools/call params must be an object"
        })
    })?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            json!({
                "code": -32602,
                "message": "Invalid params: non-empty string 'name' is required"
            })
        })?;
    let default_args = Value::Object(serde_json::Map::new());
    let args = params.get("arguments").unwrap_or(&default_args);
    if !args.is_object() {
        return Err(json!({
            "code": -32602,
            "message": "Invalid params: 'arguments' must be an object"
        }));
    }

    // Use passed registry
    if let Some(tool) = registry.get(name) {
        let ctx = tools::ToolContext {
            stream_manager: stream_manager.clone(),
        };
        let execution = if tool.holds_device_lock() {
            let _device_guard = tools::DEVICE_OPERATION_LOCK.lock().await;
            if tool.needs_unlock(args) {
                if let Err(error) = system::ensure_ready().await {
                    return Ok(mcp_tool_error(format!(
                        "Device preparation failed: {error}"
                    )));
                }
            }
            tool.execute(args, &ctx).await
        } else {
            if tool.needs_unlock(args) {
                let _device_guard = tools::DEVICE_OPERATION_LOCK.lock().await;
                if let Err(error) = system::ensure_ready().await {
                    return Ok(mcp_tool_error(format!(
                        "Device preparation failed: {error}"
                    )));
                }
            }
            tool.execute(args, &ctx).await
        };
        return match execution {
            Ok(result) => Ok(result),
            Err(error) => {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .map_or_else(|| error.to_string(), str::to_string);
                Ok(mcp_tool_error(message))
            }
        };
    }

    // Tool not found in registry
    Err(json!({"code": -32601, "message": format!("Tool not found: {}", name)}))
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ConcurrencyProbe {
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl tools::Tool for ConcurrencyProbe {
        fn name(&self) -> &'static str {
            "test_concurrency_probe"
        }
        fn description(&self) -> &'static str {
            "test-only concurrency probe"
        }
        fn schema(&self) -> Value {
            json!({"inputSchema": {"type": "object", "properties": {}}})
        }
        async fn execute(&self, args: &Value, _ctx: &tools::ToolContext) -> response::ToolResult {
            if args.get("fail").and_then(Value::as_bool) == Some(true) {
                return response::error_response("expected tool failure");
            }
            let current = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(current, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            response::text_response("ok")
        }
    }

    #[test]
    fn rejects_malformed_json_with_parse_error() {
        let error = parse_request("{").unwrap_err();
        assert_eq!(error["code"], -32700);
    }

    #[test]
    fn rejects_structurally_invalid_requests() {
        let error = parse_request(r#"{"jsonrpc":"2.0","id":1}"#).unwrap_err();
        assert_eq!(error["code"], -32600);

        let error = parse_request(r#"{"jsonrpc":"1.0","method":"initialize","id":1}"#).unwrap_err();
        assert_eq!(error["code"], -32600);
    }

    #[test]
    fn accepts_valid_requests_and_notifications() {
        let request =
            parse_request(r#"{"jsonrpc":"2.0","method":"initialize","id":"request-1"}"#).unwrap();
        assert_eq!(request.method, "initialize");

        let notification =
            parse_request(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();
        assert!(notification.id.is_none());
    }

    #[test]
    fn parses_batch_members_independently_and_rejects_empty_batches() {
        let ParsedMessage::Batch(entries) = parse_message(
            r#"[
                {"jsonrpc":"2.0","method":"initialize","id":1},
                7,
                {"jsonrpc":"2.0","method":"notifications/initialized"}
            ]"#,
        )
        .unwrap() else {
            panic!("expected batch");
        };

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].as_ref().unwrap().method, "initialize");
        assert_eq!(entries[1].as_ref().unwrap_err()["code"], -32600);
        assert_eq!(
            entries[2].as_ref().unwrap().method,
            "notifications/initialized"
        );
        assert_eq!(parse_message("[]").unwrap_err()["code"], -32600);
    }

    #[tokio::test]
    async fn mixed_batch_preserves_lifecycle_order_and_omits_notifications() {
        let ParsedMessage::Batch(entries) = parse_message(
            r#"[
                {"jsonrpc":"2.0","method":"initialize","id":"init"},
                {"jsonrpc":"2.0","method":"notifications/initialized"},
                {"jsonrpc":"2.0","method":"tools/list","id":2},
                {"jsonrpc":"2.0","method":"unknown.notification"},
                {"jsonrpc":"2.0","method":"missing","id":3},
                null
            ]"#,
        )
        .unwrap() else {
            panic!("expected batch");
        };

        let mut session = ProtocolSession::new();
        let work = prepare_batch(entries, &mut session);
        assert_eq!(session.state, ProtocolState::Ready);
        let responses = handle_batch(
            work,
            Arc::new(stream::StreamManager::new()),
            register_tools(),
        )
        .await;

        assert_eq!(responses.len(), 4);
        assert_eq!(responses[0].id, Some(json!("init")));
        assert!(responses[0].result.is_some());
        assert_eq!(responses[1].id, Some(json!(2)));
        assert!(responses[1].result.is_some());
        assert_eq!(responses[2].id, Some(json!(3)));
        assert_eq!(responses[2].error.as_ref().unwrap()["code"], -32601);
        assert_eq!(responses[3].id, Some(Value::Null));
        assert_eq!(responses[3].error.as_ref().unwrap()["code"], -32600);

        let serialized = serde_json::to_string(&OutboundMessage::Batch(responses)).unwrap();
        let value: Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(value.as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn notification_only_batch_has_no_response_payload() {
        let ParsedMessage::Batch(entries) = parse_message(
            r#"[
                {"jsonrpc":"2.0","method":"unknown.notification"},
                {"jsonrpc":"2.0","method":"another.notification"}
            ]"#,
        )
        .unwrap() else {
            panic!("expected batch");
        };
        let mut session = ProtocolSession::new();
        let responses = handle_batch(
            prepare_batch(entries, &mut session),
            Arc::new(stream::StreamManager::new()),
            register_tools(),
        )
        .await;
        assert!(responses.is_empty());
    }

    #[tokio::test]
    async fn bounded_reader_drains_oversized_lines_and_recovers() {
        let input = b"0123456789\n{}\n";
        let mut reader = tokio::io::BufReader::with_capacity(4, &input[..]);
        let mut buffer = Vec::new();

        assert_eq!(
            read_bounded_line(&mut reader, &mut buffer, 8)
                .await
                .unwrap(),
            InboundMessage::TooLarge
        );
        assert_eq!(
            read_bounded_line(&mut reader, &mut buffer, 8)
                .await
                .unwrap(),
            InboundMessage::Line("{}\n".to_string())
        );
        assert_eq!(
            read_bounded_line(&mut reader, &mut buffer, 8)
                .await
                .unwrap(),
            InboundMessage::Eof
        );
    }

    #[tokio::test]
    async fn bounded_reader_rejects_invalid_utf8_without_losing_next_line() {
        let input = [0xff, b'\n', b'{', b'}', b'\n'];
        let mut reader = tokio::io::BufReader::with_capacity(2, &input[..]);
        let mut buffer = Vec::new();

        assert_eq!(
            read_bounded_line(&mut reader, &mut buffer, 8)
                .await
                .unwrap(),
            InboundMessage::InvalidUtf8
        );
        assert_eq!(
            read_bounded_line(&mut reader, &mut buffer, 8)
                .await
                .unwrap(),
            InboundMessage::Line("{}\n".to_string())
        );
    }

    #[test]
    fn registered_tools_are_unique_and_publish_object_schemas() {
        let registry = register_tools();
        let definitions = registry.list_tools();
        let mut names = HashSet::new();

        assert!(!definitions.is_empty());
        for definition in definitions {
            let name = definition["name"].as_str().expect("tool name");
            assert!(names.insert(name.to_string()), "duplicate tool: {name}");
            assert_eq!(definition["inputSchema"]["type"], "object", "tool: {name}");
            let properties = definition["inputSchema"]["properties"]
                .as_object()
                .expect("object schema properties");
            if let Some(required) = definition["inputSchema"].get("required") {
                for field in required.as_array().expect("required must be an array") {
                    let field = field.as_str().expect("required field must be a string");
                    assert!(
                        properties.contains_key(field),
                        "tool {name} requires undeclared field {field}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn tools_call_rejects_invalid_parameter_shapes() {
        let registry = register_tools();
        let stream = Arc::new(stream::StreamManager::new());

        let error = call_tool(None, stream.clone(), &registry)
            .await
            .unwrap_err();
        assert_eq!(error["code"], -32602);
        let error = call_tool(Some(json!([])), stream.clone(), &registry)
            .await
            .unwrap_err();
        assert_eq!(error["code"], -32602);
        let error = call_tool(
            Some(json!({"name": "mcp_android_check_health", "arguments": []})),
            stream,
            &registry,
        )
        .await
        .unwrap_err();
        assert_eq!(error["code"], -32602);
    }

    #[tokio::test]
    async fn initialized_notification_is_processed_without_a_response() {
        let mut session = ProtocolSession::new();
        let initialize_request =
            parse_request(r#"{"jsonrpc":"2.0","method":"initialize","id":1}"#).unwrap();
        session.admit(&initialize_request).unwrap();
        let request =
            parse_request(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();
        session.admit(&request).unwrap();
        let response = handle_request(
            request,
            Arc::new(stream::StreamManager::new()),
            register_tools(),
        )
        .await;
        assert!(response.is_none());
    }

    #[test]
    fn tools_are_rejected_until_initialized_notification_arrives() {
        let mut session = ProtocolSession::new();
        let tools = parse_request(r#"{"jsonrpc":"2.0","method":"tools/list","id":1}"#).unwrap();
        assert_eq!(session.admit(&tools).unwrap_err()["code"], -32002);

        let initialize =
            parse_request(r#"{"jsonrpc":"2.0","method":"initialize","id":2}"#).unwrap();
        session.admit(&initialize).unwrap();
        assert_eq!(session.admit(&tools).unwrap_err()["code"], -32002);

        let initialized =
            parse_request(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();
        session.admit(&initialized).unwrap();
        session.admit(&tools).unwrap();
    }

    #[test]
    fn lifecycle_admission_is_ordered_and_rejects_duplicates() {
        let mut session = ProtocolSession::new();
        let initialized =
            parse_request(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();
        assert_eq!(session.admit(&initialized).unwrap_err()["code"], -32600);

        let initialize =
            parse_request(r#"{"jsonrpc":"2.0","method":"initialize","id":1}"#).unwrap();
        session.admit(&initialize).unwrap();
        assert_eq!(session.admit(&initialize).unwrap_err()["code"], -32600);

        session.admit(&initialized).unwrap();
        assert_eq!(session.admit(&initialize).unwrap_err()["code"], -32600);
    }

    #[test]
    fn initialize_notification_does_not_advance_lifecycle() {
        let mut session = ProtocolSession::new();
        let initialize_notification =
            parse_request(r#"{"jsonrpc":"2.0","method":"initialize"}"#).unwrap();
        assert_eq!(
            session.admit(&initialize_notification).unwrap_err()["code"],
            -32600
        );

        let tools = parse_request(r#"{"jsonrpc":"2.0","method":"tools/list","id":1}"#).unwrap();
        assert_eq!(session.admit(&tools).unwrap_err()["code"], -32002);
    }

    #[test]
    fn initialized_method_with_an_id_does_not_advance_lifecycle() {
        let mut session = ProtocolSession::new();
        let initialize =
            parse_request(r#"{"jsonrpc":"2.0","method":"initialize","id":1}"#).unwrap();
        session.admit(&initialize).unwrap();

        let initialized_request =
            parse_request(r#"{"jsonrpc":"2.0","method":"notifications/initialized","id":2}"#)
                .unwrap();
        assert_eq!(
            session.admit(&initialized_request).unwrap_err()["code"],
            -32600
        );

        let tools = parse_request(r#"{"jsonrpc":"2.0","method":"tools/list","id":3}"#).unwrap();
        assert_eq!(session.admit(&tools).unwrap_err()["code"], -32002);
    }

    #[tokio::test]
    async fn concurrent_tool_calls_are_serialized() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let mut registry = tools::ToolRegistry::new();
        registry.register(ConcurrencyProbe {
            active: active.clone(),
            max_active: max_active.clone(),
        });
        let stream = Arc::new(stream::StreamManager::new());
        let params = Some(json!({"name": "test_concurrency_probe", "arguments": {}}));

        let (first, second) = tokio::join!(
            call_tool(params.clone(), stream.clone(), &registry),
            call_tool(params, stream, &registry),
        );

        assert!(first.is_ok());
        assert!(second.is_ok());
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn tool_execution_failures_use_mcp_error_results() {
        let mut registry = tools::ToolRegistry::new();
        registry.register(ConcurrencyProbe {
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
        });
        let result = call_tool(
            Some(json!({"name": "test_concurrency_probe", "arguments": {"fail": true}})),
            Arc::new(stream::StreamManager::new()),
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], "expected tool failure");
    }

    #[test]
    fn only_ui_actions_request_implicit_unlock() {
        use crate::tools::Tool;

        assert!(tools::input::InputActTool.needs_unlock(&json!({"action": "tap"})));
        assert!(tools::app::AppManageTool.needs_unlock(&json!({"action": "launch"})));
        assert!(!tools::app::AppManageTool.needs_unlock(&json!({"action": "list"})));
        assert!(!tools::system::SystemControlTool.needs_unlock(&json!({"action": "set_overlay"})));
    }
}
