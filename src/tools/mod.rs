use crate::response::ToolResult;
use crate::stream;
use crate::vision as vision_module;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

pub mod app;
pub mod atomic;
pub mod automation;
pub mod companion;
pub mod device;
pub mod flow;
pub mod forensics;
pub mod fs;
pub mod gesture;
pub mod input;
pub mod instrumentation;
pub mod intent;
pub mod logs;
pub mod media;
pub mod network;
pub mod notifications;
pub mod sensors;
pub mod sentinel;
pub mod session;
pub mod shell;
pub mod system;
pub mod vision;

/// One Android target is selected for the process. All stateful operations,
/// including background sentinel enforcement, use this lock so they cannot
/// interleave command sequences on that target.
pub static DEVICE_OPERATION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Observation frames are a side effect of an action the caller already asked
/// for, so they are bounded more tightly than an explicitly requested
/// screenshot. Their coordinate space is published alongside them.
const OBSERVATION_MAX_WIDTH: u32 = 720;
const OBSERVATION_JPEG_QUALITY: u8 = 60;

/// Schema fragment for the `wait_ms` argument accepted by every tool that
/// returns an observation frame. Each tool supplies its own default because the
/// settling time for, say, launching an app differs from toggling a sensor.
pub fn wait_ms_property(default_ms: u64) -> Value {
    json!({
        "type": "integer",
        "minimum": 0,
        "maximum": 10000,
        "default": default_ms,
        "description": "Milliseconds to wait after the action before capturing the observation frame. Values above 10000 are clamped."
    })
}

/// Read an argument that the tool cannot proceed without, reporting the
/// omission in one consistent phrasing rather than letting each call site
/// invent its own. Absent and wrong-typed are the same failure to a caller, so
/// they share a message.
pub fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| crate::response::error_payload(format!("Missing required argument: {key}")))
}

/// Merge additional keys into a tool result's `metadata` object without
/// discarding entries an inner call already published, such as `truncation`.
pub fn merge_metadata(result: &mut Value, additions: Value) {
    let Some(additions) = additions.as_object() else {
        return;
    };
    let metadata = result
        .as_object_mut()
        .map(|object| object.entry("metadata").or_insert_with(|| json!({})));
    if let Some(Value::Object(metadata)) = metadata {
        for (key, value) in additions {
            metadata.insert(key.clone(), value.clone());
        }
    }
}

pub struct ToolContext {
    pub stream_manager: Arc<stream::StreamManager>,
}

impl ToolContext {
    /// Run an action and, when the continuous stream has a cached frame,
    /// append that frame without replacing or stringifying the tool's result.
    /// Observation is best-effort and never starts or kills device processes.
    pub async fn run_with_observation<F, Fut>(&self, wait_ms: u64, action: F) -> ToolResult
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = ToolResult> + Send,
    {
        let mut result = action().await?;

        if !self.has_stream_frame() {
            return Ok(result);
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms.min(10_000))).await;

        let stream_image = self
            .stream_manager
            .latest_image
            .lock()
            .ok()
            .and_then(|image| image.clone());

        let Some(image) = stream_image else {
            return Ok(result);
        };

        let encoded = tokio::task::spawn_blocking(move || {
            vision_module::encode_frame(&image, OBSERVATION_MAX_WIDTH, OBSERVATION_JPEG_QUALITY)
        })
        .await
        .map_err(|error| {
            json!({
                "code": -32000,
                "message": format!("Observation encoding task failed: {error}")
            })
        })?;

        // Observation is best-effort: a failed encode must not fail the action
        // that already succeeded on the device.
        let Ok(encoded) = encoded else {
            return Ok(result);
        };

        if let Some(content) = result.get_mut("content").and_then(Value::as_array_mut) {
            if !encoded.data.is_empty() {
                content.push(json!({
                    "type": "image",
                    "data": encoded.data,
                    "mimeType": "image/jpeg"
                }));
                merge_metadata(&mut result, encoded.metadata());
            }
        }

        Ok(result)
    }

    fn has_stream_frame(&self) -> bool {
        let running = self
            .stream_manager
            .running
            .lock()
            .is_ok_and(|running| *running);
        if !running {
            return false;
        }
        self.stream_manager
            .latest_image
            .lock()
            .is_ok_and(|image| image.is_some())
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn schema(&self) -> Value;
    async fn execute(&self, args: &Value, ctx: &ToolContext) -> ToolResult;
    fn needs_unlock(&self, _args: &Value) -> bool {
        false
    }
    /// Long-running read-only pollers must acquire the device lock only around
    /// each individual ADB probe instead of monopolizing it for their lifetime.
    fn holds_device_lock(&self) -> bool {
        true
    }
}

pub struct ToolRegistry {
    tools: BTreeMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
        }
    }

    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        let name = tool.name().to_string();
        assert!(
            self.tools.insert(name.clone(), Box::new(tool)).is_none(),
            "duplicate MCP tool registration: {name}"
        );
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(Box::as_ref)
    }

    /// Lists all registered tools with their schemas for the MCP `tools/list`
    /// response.
    ///
    /// Each `Tool::schema()` already returns the `inputSchema` wrapper:
    ///
    /// ```json
    /// { "inputSchema": { "type": "object", "properties": {} } }
    /// ```
    ///
    /// so its fields are merged into the tool object rather than assigned to a
    /// second `inputSchema` key. Wrapping it again nests the key inside itself,
    /// leaving the outer object without a `type` and making every tool fail
    /// client-side validation with `tools.N.custom.input_schema.type: Field
    /// required`.
    pub fn list_tools(&self) -> Vec<Value> {
        self.tools
            .values()
            .map(|t| {
                // Tool::schema() returns { "inputSchema": { "type": "object", ... } }
                // We MERGE it directly to avoid double-nesting the inputSchema key
                let mut tool_obj = json!({
                    "name": t.name(),
                    "description": t.description()
                });
                if let Value::Object(schema_map) = t.schema() {
                    for (key, value) in schema_map {
                        tool_obj[key] = value;
                    }
                }
                tool_obj
            })
            .collect()
    }
}
