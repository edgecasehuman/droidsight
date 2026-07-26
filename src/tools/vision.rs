use crate::automation;
use crate::response;
use crate::tools::Tool;
use crate::vision;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;

static A11Y_DIFF_BASELINE: Mutex<Option<crate::vision::UiNode>> = Mutex::const_new(None);
const SCREENSHOT_SEQUENCE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
const MAX_SEQUENCE_ENCODED_BYTES: usize = 24 * 1024 * 1024;

fn bounded_u32(
    args: &Value,
    name: &str,
    default: u32,
    range: std::ops::RangeInclusive<u32>,
) -> Result<u32, String> {
    let value = match args.get(name) {
        None => default,
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| format!("{name} must be an unsigned 32-bit integer"))?,
    };
    if !range.contains(&value) {
        return Err(format!(
            "{name} must be between {} and {}",
            range.start(),
            range.end()
        ));
    }
    Ok(value)
}

fn encoding_options(args: &Value) -> Result<(u32, u8), String> {
    let max_width = bounded_u32(args, "max_width", 720, 64..=1440)?;
    let quality = bounded_u32(args, "quality", 75, 1..=95)?;
    let quality = u8::try_from(quality).map_err(|_| "quality is out of range".to_string())?;
    Ok((max_width, quality))
}

fn required_u32(args: &Value, name: &str, minimum: u32) -> Result<u32, String> {
    if args.get(name).is_none() {
        return Err(format!("{name} is required"));
    }
    bounded_u32(args, name, minimum, minimum..=u32::MAX)
}

pub struct VisionQueryTool;

#[async_trait]
impl Tool for VisionQueryTool {
    fn name(&self) -> &'static str {
        "mcp_android_vision_query"
    }

    fn description(&self) -> &'static str {
        "See the screen (screenshot, hierarchy, OCR, and element/template search)"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["screenshot", "screenshot_cropped", "screenshot_annotated", "screenshot_sequence", "hierarchy", "elements", "tap_element", "hierarchy_diff", "ocr", "find_text", "find_element", "find_template"]
                    },
                    "query": { "type": "string" },
                    "format": { "type": "string" },
                    // find_element args
                    "text": { "type": "string" },
                    "resource_id": { "type": "string" },
                    "content_desc": { "type": "string" },
                    // find_template args
                    "template": { "type": "string" }, // base64 encoded image
                    "threshold": { "type": "number" }
                    ,"snapshot_id": { "type": "string" }
                    ,"index": { "type": "integer", "minimum": 0 }
                    ,"x": {"type":"integer","minimum":0}, "y":{"type":"integer","minimum":0}, "width":{"type":"integer","minimum":1}, "height":{"type":"integer","minimum":1}
                    ,"max_width":{"type":"integer","minimum":64,"maximum":1440,"default":720}, "quality":{"type":"integer","minimum":1,"maximum":95,"default":75}
                    ,"clickable_only":{"type":"boolean","default":true}, "count":{"type":"integer","minimum":1,"maximum":10}, "interval_ms":{"type":"integer","minimum":100,"maximum":2000}
                },
                "required": ["action"]
            }
        })
    }

    async fn execute(&self, args: &Value, ctx: &crate::tools::ToolContext) -> response::ToolResult {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

        // Some vision actions need stream access
        match action {
            "screenshot" => {
                let (max_width, quality) = match encoding_options(args) {
                    Ok(options) => options,
                    Err(error) => return response::error_response(error),
                };

                // Use the continuously maintained stream frame when available.
                let img_opt = vision::get_latest_image_raw(&ctx.stream_manager);

                let encoded = if let Some(img) = img_opt {
                    // Offload CPU-bound encoding to blocking thread
                    tokio::task::spawn_blocking(move || {
                        vision::encode_frame(&img, max_width, quality)
                    })
                    .await
                    .map_err(|e| {
                        json!({
                            "code": -32000,
                            "message": format!("Join error: {}", e)
                        })
                    })?
                } else {
                    // Fallback to native screenshot
                    match vision::screenshot().await {
                        Ok(bytes) => {
                            // Offload CPU-bound decoding AND encoding
                            tokio::task::spawn_blocking(move || {
                                vision::encode_full(&bytes, max_width, quality)
                            })
                            .await
                            .map_err(|e| {
                                json!({
                                    "code": -32000,
                                    "message": format!("Join error: {}", e)
                                })
                            })?
                        }
                        Err(e) => return response::error_response(e.to_string()),
                    }
                };

                match encoded {
                    Ok(encoded) => Ok(json!({
                        "content": [{
                            "type": "image",
                            "data": encoded.data,
                            "mimeType": "image/jpeg"
                        }],
                        "metadata": encoded.metadata()
                    })),
                    Err(error) => response::error_response(error.to_string()),
                }
            }
            "screenshot_cropped" => {
                let x = match required_u32(args, "x", 0) {
                    Ok(value) => value,
                    Err(error) => return response::error_response(error),
                };
                let y = match required_u32(args, "y", 0) {
                    Ok(value) => value,
                    Err(error) => return response::error_response(error),
                };
                let width = match required_u32(args, "width", 1) {
                    Ok(value) => value,
                    Err(error) => return response::error_response(error),
                };
                let height = match required_u32(args, "height", 1) {
                    Ok(value) => value,
                    Err(error) => return response::error_response(error),
                };
                let (max_width, quality) = match encoding_options(args) {
                    Ok(options) => options,
                    Err(error) => return response::error_response(error),
                };
                let bytes = match vision::screenshot().await {
                    Ok(v) => v,
                    Err(e) => return response::error_response(e.to_string()),
                };
                match tokio::task::spawn_blocking(move || {
                    vision::encode_crop(&bytes, x, y, width, height, max_width, quality)
                })
                .await
                {
                    Ok(Ok(encoded)) => Ok(json!({
                        "content": [{"type":"image","data":encoded.data,"mimeType":"image/jpeg"}],
                        "metadata": encoded.metadata()
                    })),
                    Ok(Err(error)) => response::error_response(error.to_string()),
                    Err(error) => {
                        response::error_response(format!("crop encoding task failed: {error}"))
                    }
                }
            }
            "screenshot_annotated" => {
                let (max_width, quality) = match encoding_options(args) {
                    Ok(options) => options,
                    Err(error) => return response::error_response(error),
                };
                let clickable_only = args
                    .get("clickable_only")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let bytes = match vision::screenshot().await {
                    Ok(v) => v,
                    Err(e) => return response::error_response(e.to_string()),
                };
                let root = match vision::fetch_parsed_hierarchy().await {
                    Ok(v) => v,
                    Err(e) => return response::error_response(e.to_string()),
                };
                match tokio::task::spawn_blocking(move || {
                    vision::encode_annotated(&bytes, &root, clickable_only, max_width, quality)
                })
                .await
                {
                    Ok(Ok(encoded)) => Ok(json!({
                        "content": [{"type":"image","data":encoded.data,"mimeType":"image/jpeg"}],
                        "metadata": encoded.metadata()
                    })),
                    Ok(Err(error)) => response::error_response(error.to_string()),
                    Err(error) => response::error_response(format!(
                        "annotation encoding task failed: {error}"
                    )),
                }
            }
            "screenshot_sequence" => {
                let count = match bounded_u32(args, "count", 3, 1..=10) {
                    Ok(value) => value,
                    Err(error) => return response::error_response(error),
                };
                let interval = match bounded_u32(args, "interval_ms", 500, 100..=2000) {
                    Ok(value) => value,
                    Err(error) => return response::error_response(error),
                };
                let (max_width, quality) = match encoding_options(args) {
                    Ok(options) => options,
                    Err(error) => return response::error_response(error),
                };
                let deadline = tokio::time::Instant::now() + SCREENSHOT_SEQUENCE_DEADLINE;
                match tokio::time::timeout_at(deadline, async move {
                    let mut content = Vec::with_capacity(count as usize);
                    let mut metadata = None;
                    let mut encoded_bytes = 0usize;
                    for index in 0..count {
                        let bytes = vision::screenshot()
                            .await
                            .map_err(|error| format!("frame {index}: {error}"))?;
                        let encoded = tokio::task::spawn_blocking(move || {
                            vision::encode_full(&bytes, max_width, quality)
                        })
                        .await
                        .map_err(|error| format!("frame {index} encoding task failed: {error}"))?
                        .map_err(|error| format!("frame {index}: {error}"))?;
                        encoded_bytes = encoded_bytes
                            .checked_add(encoded.data.len())
                            .ok_or_else(|| "sequence response size overflow".to_string())?;
                        if encoded_bytes > MAX_SEQUENCE_ENCODED_BYTES {
                            return Err(format!(
                                "sequence exceeds the {} MiB encoded response limit",
                                MAX_SEQUENCE_ENCODED_BYTES / (1024 * 1024)
                            ));
                        }
                        // Every frame in a sequence shares one coordinate space.
                        metadata.get_or_insert_with(|| encoded.metadata());
                        content.push(json!({
                            "type": "image",
                            "data": encoded.data,
                            "mimeType": "image/jpeg"
                        }));
                        if index + 1 < count {
                            tokio::time::sleep(tokio::time::Duration::from_millis(interval.into()))
                                .await;
                        }
                    }
                    Ok::<_, String>((content, metadata))
                })
                .await
                {
                    Ok(Ok((content, metadata))) => Ok(json!({
                        "content": content,
                        "metadata": metadata.unwrap_or_else(|| json!({}))
                    })),
                    Ok(Err(error)) => response::error_response(error),
                    Err(_) => response::error_response(
                        "screenshot sequence exceeded its 10 second deadline",
                    ),
                }
            }
            "hierarchy" => vision::get_view_hierarchy().await,
            "elements" => match vision::fetch_parsed_hierarchy().await {
                Ok(root) => match serde_json::to_string(&crate::element_snapshots::build(&root)) {
                    Ok(value) => response::bounded_text_response(
                        value,
                        response::DEFAULT_TEXT_BUDGET_BYTES,
                        response::TruncationStrategy::Head,
                    ),
                    Err(error) => {
                        response::error_response(format!("Element serialization failed: {error}"))
                    }
                },
                Err(error) => response::error_response(error.to_string()),
            },
            "tap_element" => {
                let snapshot_id = args
                    .get("snapshot_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if snapshot_id.is_empty() {
                    return response::error_response("tap_element requires a snapshot_id");
                }
                // Bind the index before the await below, so the check that it is
                // present cannot drift away from the use.
                let Some(index) = args
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|v| usize::try_from(v).ok())
                else {
                    return response::error_response(
                        "tap_element requires a non-negative integer index",
                    );
                };
                let root = match vision::fetch_parsed_hierarchy().await {
                    Ok(root) => root,
                    Err(error) => return response::error_response(error.to_string()),
                };
                let current = crate::element_snapshots::build(&root);
                let element = match crate::element_snapshots::select(&current, snapshot_id, index) {
                    Ok(element) => element,
                    Err(error) => return response::error_response(error),
                };
                crate::input::tap(&ctx.stream_manager, element.center.x, element.center.y).await
            }
            "hierarchy_diff" => {
                let current = match vision::fetch_parsed_hierarchy().await {
                    Ok(current) => current,
                    Err(error) => return response::error_response(error.to_string()),
                };
                let mut baseline = A11Y_DIFF_BASELINE.lock().await;
                let diff = crate::a11y_diff::diff(baseline.as_ref(), &current);
                *baseline = Some(current);
                match serde_json::to_string(&diff) {
                    Ok(serialized) => response::bounded_text_response(
                        serialized,
                        response::DEFAULT_TEXT_BUDGET_BYTES,
                        response::TruncationStrategy::Head,
                    ),
                    Err(error) => {
                        response::error_response(format!("Diff serialization failed: {error}"))
                    }
                }
            }
            "ocr" => vision::ocr().await,
            "find_text" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query.is_empty() {
                    return response::error_response("query is required for find_text");
                }
                vision::find_text(query).await
            }
            "find_element" => {
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let id = args
                    .get("resource_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let desc = args
                    .get("content_desc")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if text.is_empty() && id.is_empty() && desc.is_empty() {
                    return response::error_response(
                        "text, resource_id, or content_desc is required for find_element",
                    );
                }
                automation::find_element(text, id, desc).await
            }
            "find_template" => {
                let template_b64 = args.get("template").and_then(|v| v.as_str()).unwrap_or("");
                let threshold = match args
                    .get("threshold")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.8)
                {
                    value if value.is_finite() && (0.0..=1.0).contains(&value) => value as f32,
                    _ => {
                        return response::error_response(
                            "threshold must be a finite number between 0 and 1",
                        )
                    }
                };

                if template_b64.is_empty() {
                    return response::error_response("Template argument required (base64 string)");
                }
                if template_b64.len() > 8 * 1024 * 1024 {
                    return response::error_response(
                        "Template exceeds the 8 MiB encoded-size limit",
                    );
                }

                // Decode the base64 template into the raw bytes `find_template` expects.
                use base64::{engine::general_purpose, Engine as _};
                match general_purpose::STANDARD.decode(template_b64) {
                    Ok(data) => vision::find_template(&ctx.stream_manager, &data, threshold).await,
                    Err(e) => response::error_response(format!("Invalid base64 template: {e}")),
                }
            }
            _ => response::error_response(format!("Unknown vision action: {action}")),
        }
    }
}

pub struct VisionStreamTool;

#[async_trait]
impl Tool for VisionStreamTool {
    fn name(&self) -> &'static str {
        "mcp_android_vision_stream"
    }

    fn description(&self) -> &'static str {
        "Start/Stop/Read H.264 Stream"
    }

    fn schema(&self) -> Value {
        json!({
             "inputSchema": {
                "type": "object",
                "properties": {
                     "action": { "type": "string", "enum": ["start", "stop", "read"] }
                },
                "required": ["action"]
            }
        })
    }

    async fn execute(&self, args: &Value, ctx: &crate::tools::ToolContext) -> response::ToolResult {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        vision::vision_stream(action, &ctx.stream_manager)
    }
}

pub struct GetViewHierarchyTool;

#[async_trait]
impl Tool for GetViewHierarchyTool {
    fn name(&self) -> &'static str {
        "mcp_android_get_view_hierarchy"
    }

    fn description(&self) -> &'static str {
        "Get structured UI tree"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        })
    }

    async fn execute(
        &self,
        _args: &Value,
        _ctx: &crate::tools::ToolContext,
    ) -> response::ToolResult {
        vision::get_view_hierarchy().await
    }
}

#[cfg(test)]
mod screenshot_option_tests {
    use super::*;

    #[test]
    fn checked_options_reject_wrong_types_overflow_and_schema_violations() {
        assert_eq!(encoding_options(&json!({})).unwrap(), (720, 75));
        assert!(encoding_options(&json!({"quality": 331})).is_err());
        assert!(encoding_options(&json!({"quality": "75"})).is_err());
        assert!(encoding_options(&json!({"max_width": u64::MAX})).is_err());
        assert!(encoding_options(&json!({"max_width": 63})).is_err());
    }

    #[test]
    fn crop_coordinates_are_required_and_checked() {
        assert!(required_u32(&json!({}), "x", 0).is_err());
        assert_eq!(required_u32(&json!({"x": 0}), "x", 0).unwrap(), 0);
        assert!(required_u32(&json!({"width": 0}), "width", 1).is_err());
        assert!(required_u32(&json!({"x": -1}), "x", 0).is_err());
    }
}
