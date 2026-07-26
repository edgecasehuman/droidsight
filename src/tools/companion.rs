use crate::adb::Adb;
use crate::response::{self, ToolResult};
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct CompanionTool;

#[async_trait]
impl Tool for CompanionTool {
    fn name(&self) -> &'static str {
        "mcp_android_companion"
    }

    fn description(&self) -> &'static str {
        "Surface something to the person holding the device: post a notification, \
         show a transient message, or open a URL in the browser"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["say", "show_url", "toast"]
                    },
                    "text": { "type": "string", "description": "Text content for say/toast" },
                    "title": { "type": "string", "description": "Title for notification" },
                    "url": { "type": "string", "description": "URL for show_url" }
                },
                "required": ["action"]
            }
        })
    }

    async fn execute(&self, args: &Value, _ctx: &crate::tools::ToolContext) -> ToolResult {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

        match action {
            "say" => {
                // Post a persistent, visible notification via `cmd notification`,
                // which requires Android 13+.
                let title = args
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("MCP Agent");
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("...");

                let cmd = format!(
                    "cmd notification post -S bigtext -t {} {} {}",
                    crate::adb::shell_quote(title),
                    crate::adb::shell_quote("droidsight"),
                    crate::adb::shell_quote(text),
                );

                match Adb::shell_native(&cmd).await {
                    Ok(out) => {
                        if out.contains("Error") || out.contains("exception") {
                            response::error_response(format!(
                                "Notification failed (Android <13?): {out}"
                            ))
                        } else {
                            response::bounded_text_response(
                                format!("Notification Posted: {out}"),
                                response::DEFAULT_TEXT_BUDGET_BYTES,
                                response::TruncationStrategy::Head,
                            )
                        }
                    }
                    Err(e) => response::error_response(e.to_string()),
                }
            }
            "toast" => {
                // Real toasts require an on-device app, so approximate a toast
                // with a lightweight notification labeled as such.
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("...");
                let cmd = format!(
                    "cmd notification post -S text -t {} {} {}",
                    crate::adb::shell_quote("Toast"),
                    crate::adb::shell_quote("droidsight"),
                    crate::adb::shell_quote(text),
                );
                let _ = Adb::shell_native(&cmd).await;
                response::text_response("Toast (simulated via Notification) sent")
            }
            "show_url" => {
                // Opening some arbitrary default would be a silent wrong action
                // on the user's device, so a missing url is an error.
                let Some(url) = args.get("url").and_then(|v| v.as_str()) else {
                    return response::error_response("show_url requires a `url` argument");
                };
                let cmd = format!(
                    "am start -a android.intent.action.VIEW -d {}",
                    crate::adb::shell_quote(url)
                );
                match Adb::shell_native(&cmd).await {
                    Ok(out) => response::bounded_text_response(
                        format!("Browser launched: {out}"),
                        response::DEFAULT_TEXT_BUDGET_BYTES,
                        response::TruncationStrategy::Head,
                    ),
                    Err(e) => response::error_response(e.to_string()),
                }
            }
            _ => response::error_response(format!("Unknown action: {action}")),
        }
    }
}
