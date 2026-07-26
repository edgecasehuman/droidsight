use crate::input;
use crate::response::{self, ToolResult};
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct InputActTool;

#[async_trait]
impl Tool for InputActTool {
    fn name(&self) -> &'static str {
        "mcp_android_input_act"
    }

    fn description(&self) -> &'static str {
        "Perform inputs (tap, text, key, swipe, smart_tap, ime)"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["tap", "text", "key", "swipe", "smart_tap", "ime_set"]
                    },
                    "x": { "type": "integer" },
                    "y": { "type": "integer" },
                    "duration": { "type": "integer" },
                    "text": { "type": "string" },
                    "mode": { "type": "string", "enum": ["auto", "ascii", "unicode", "clipboard"], "default": "auto" },
                    "keycode": { "type": "string" },
                    "force": { "type": "boolean" },
                    "x1": { "type": "integer" },
                    "y1": { "type": "integer" },
                    "x2": { "type": "integer" },
                    "y2": { "type": "integer" },
                        // Smart Tap args
                    "resource_id": { "type": "string" },
                    "content_desc": { "type": "string" },
                    // IME args
                    "ime_id": { "type": "string" }
                },
                "required": ["action"]
            }
        })
    }

    async fn execute(&self, args: &Value, ctx: &crate::tools::ToolContext) -> ToolResult {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

        match action {
            "tap" => {
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if args.get("x").is_none() && args.get("y").is_none() && !text.is_empty() {
                    // Pass 'text' as both text AND content_desc to find matches in either field
                    crate::automation::tap_element(text, "", text).await
                } else {
                    let Some(x) = args
                        .get("x")
                        .and_then(Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok())
                    else {
                        return response::error_response("tap requires x as a 32-bit integer");
                    };
                    let Some(y) = args
                        .get("y")
                        .and_then(Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok())
                    else {
                        return response::error_response("tap requires y as a 32-bit integer");
                    };
                    input::tap(&ctx.stream_manager, x, y).await
                }
            }
            "text" => {
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if text.is_empty() {
                    return response::error_response("text is required for the text action");
                }
                let mode = args.get("mode").and_then(Value::as_str).unwrap_or("auto");
                let Some(mode) = input::TextMode::parse(mode) else {
                    return response::error_response(
                        "mode must be auto, ascii, unicode, or clipboard",
                    );
                };
                input::text_with_mode(&ctx.stream_manager, text, mode).await
            }
            "key" => {
                let keycode = args.get("keycode").and_then(|v| v.as_str()).unwrap_or("");
                if keycode.is_empty() {
                    return response::error_response("keycode is required for the key action");
                }
                let force = args
                    .get("force")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                input::key(&ctx.stream_manager, keycode, force).await
            }
            "swipe" => {
                let coordinate = |name: &str| {
                    args.get(name)
                        .and_then(Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok())
                };
                let (Some(x1), Some(y1), Some(x2), Some(y2)) = (
                    coordinate("x1"),
                    coordinate("y1"),
                    coordinate("x2"),
                    coordinate("y2"),
                ) else {
                    return response::error_response(
                        "swipe requires x1, y1, x2, and y2 as 32-bit integers",
                    );
                };
                let duration = match args.get("duration").and_then(Value::as_i64).unwrap_or(500) {
                    value @ 1..=10_000 => value as i32,
                    _ => {
                        return response::error_response(
                            "duration must be between 1 and 10000 milliseconds",
                        )
                    }
                };
                input::swipe(&ctx.stream_manager, x1, y1, x2, y2, duration).await
            }
            "smart_tap" => {
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let rid = args
                    .get("resource_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let desc = args
                    .get("content_desc")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if text.is_empty() && rid.is_empty() && desc.is_empty() {
                    return response::error_response(
                        "smart_tap requires text, resource_id, or content_desc",
                    );
                }
                crate::automation::tap_element(text, rid, desc).await
            }
            "ime_set" => {
                let ime_id = args.get("ime_id").and_then(|v| v.as_str()).unwrap_or("");
                if ime_id.is_empty() {
                    return response::error_response("ime_id is required for ime_set");
                }
                input::set_ime(&ctx.stream_manager, ime_id).await
            }
            _ => response::error_response(format!("Unknown action: {action}")),
        }
    }

    fn needs_unlock(&self, _args: &Value) -> bool {
        true
    }
}
