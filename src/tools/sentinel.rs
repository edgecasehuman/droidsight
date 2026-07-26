use crate::response::{self, ToolResult};
use crate::sentinel;
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SentinelControlTool;

#[async_trait]
impl Tool for SentinelControlTool {
    fn name(&self) -> &'static str {
        "mcp_android_sentinel_control"
    }

    fn description(&self) -> &'static str {
        "Watch apps and re-apply permissions, accessibility services, and overlay access"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["watch", "forget", "list"] },
                    "package_name": { "type": "string", "description": "Package to start or stop watching" },
                    "service": { "type": "string", "description": "Accessibility service to keep enabled, in package/class form" },
                    "overlay": { "type": "boolean", "description": "Keep SYSTEM_ALERT_WINDOW (draw over other apps) granted to the package" },
                    "permissions": { "type": "array", "items": { "type": "string" }, "description": "Runtime permissions to keep granted to the package" },
                    "keep_awake": { "type": "boolean", "description": "Wake and unlock the device whenever the screen is found off" },
                    "pin": { "type": "string", "description": "Digits used to unlock the device when keep_awake is set. Stored in memory for the process lifetime" }
                },
                "required": ["action"]
            }
        })
    }

    async fn execute(&self, args: &Value, _ctx: &crate::tools::ToolContext) -> ToolResult {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

        match action {
            "watch" => {
                let pkg = args
                    .get("package_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if pkg.is_empty() {
                    return response::error_response("package_name is required for watch");
                }
                let svc = args
                    .get("service")
                    .and_then(|v| v.as_str())
                    .map(std::string::ToString::to_string);
                let overlay = args
                    .get("overlay")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let keep_awake = args
                    .get("keep_awake")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let pin = args
                    .get("pin")
                    .and_then(|v| v.as_str())
                    .map(std::string::ToString::to_string);
                if pin
                    .as_deref()
                    .is_some_and(|pin| pin.is_empty() || !pin.chars().all(|ch| ch.is_ascii_digit()))
                {
                    return response::error_response("pin must contain ASCII digits only");
                }
                let mut perms = Vec::new();
                if let Some(values) = args.get("permissions") {
                    let values = match values.as_array() {
                        Some(values) if values.len() <= 100 => values,
                        Some(_) => {
                            return response::error_response(
                                "permissions is limited to 100 entries",
                            )
                        }
                        None => return response::error_response("permissions must be an array"),
                    };
                    for (index, value) in values.iter().enumerate() {
                        match value.as_str().filter(|value| !value.is_empty()) {
                            Some(value) => perms.push(value.to_string()),
                            None => {
                                return response::error_response(format!(
                                    "permissions[{index}] must be a non-empty string"
                                ))
                            }
                        }
                    }
                }
                sentinel::add_watch(pkg.to_string(), svc, overlay, perms, keep_awake, pin)
            }
            "forget" => {
                let pkg = args
                    .get("package_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if pkg.is_empty() {
                    return response::error_response("package_name is required for forget");
                }
                sentinel::remove_watch(pkg.to_string())
            }
            "list" => sentinel::list_watches(),
            _ => response::error_response(format!("Unknown sentinel action: {action}")),
        }
    }
}
