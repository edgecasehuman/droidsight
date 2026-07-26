use crate::response::{self, ToolResult};
use crate::system;
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SystemControlTool;

#[async_trait]
impl Tool for SystemControlTool {
    fn name(&self) -> &'static str {
        "mcp_android_system_control"
    }

    fn description(&self) -> &'static str {
        "System Settings (accessibility, overlays)"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["set_accessibility", "set_overlay"]
                    },
                    "package_name": { "type": "string", "description": "Package whose overlay permission is changed by set_overlay" },
                    "service": { "type": "string", "description": "Accessibility service to enable or disable, in package/class form" },
                    "enabled": { "type": "boolean", "description": "Target state for the selected action. Defaults to true" },
                    "wait_ms": crate::tools::wait_ms_property(500)
                },
                "required": ["action"]
            }
        })
    }

    async fn execute(&self, args: &Value, ctx: &crate::tools::ToolContext) -> ToolResult {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let wait_ms = args
            .get("wait_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(500);

        let args = args.clone();

        ctx.run_with_observation(wait_ms, || async move {
            match action.as_str() {
                "set_accessibility" => {
                    let service = args.get("service").and_then(|v| v.as_str()).unwrap_or("");
                    let enabled = args
                        .get("enabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true);
                    system::set_accessibility(service, enabled).await
                }
                "set_overlay" => {
                    let pkg = args
                        .get("package_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let enabled = args
                        .get("enabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true);
                    system::set_overlay(pkg, enabled).await
                }
                _ => response::error_response(format!("Unknown system action: {action}")),
            }
        })
        .await
    }
    fn needs_unlock(&self, _args: &Value) -> bool {
        false
    }
}
