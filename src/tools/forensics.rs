use crate::forensics;
use crate::response::{self, ToolResult};
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ForensicsControlTool;

#[async_trait]
impl Tool for ForensicsControlTool {
    fn name(&self) -> &'static str {
        "mcp_android_forensics_control"
    }

    fn description(&self) -> &'static str {
        "Forensic tools: query an on-device SQLite database, hash a file, or \
         irreversibly delete an application's data"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["sqlite_query", "file_hash", "clear_app_data"],
                        "description": "clear_app_data runs `pm clear`. It permanently deletes the application's databases, preferences, accounts, and credentials. It is not a cache eviction and cannot be undone."
                    },
                    "path": { "type": "string" },
                    "query": { "type": "string" },
                    "algorithm": { "type": "string", "enum": ["md5", "sha256"] },
                    "package_name": { "type": "string" },
                    "confirm_destructive": {
                        "type": "boolean",
                        "description": "Must be true to run clear_app_data. Ignored by every other action."
                    },
                    "wait_ms": crate::tools::wait_ms_property(200)
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
            .unwrap_or(200);

        let args = args.clone();
        ctx.run_with_observation(wait_ms, || async move {
            match action.as_str() {
                "sqlite_query" => {
                    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                    forensics::sqlite_query(path, query).await
                }
                "file_hash" => {
                    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    let algo = args
                        .get("algorithm")
                        .and_then(|v| v.as_str())
                        .unwrap_or("md5");
                    forensics::file_hash(path, algo).await
                }
                "clear_app_data" => {
                    if args.get("confirm_destructive").and_then(Value::as_bool) != Some(true) {
                        return response::error_response(
                            "clear_app_data permanently deletes the application's data. \
                             Re-issue the call with \"confirm_destructive\": true."
                                .to_string(),
                        );
                    }
                    let pkg = args
                        .get("package_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    forensics::clear_app_data(pkg).await
                }
                _ => response::error_response(format!("Unknown forensics action: {action}")),
            }
        })
        .await
    }
}
