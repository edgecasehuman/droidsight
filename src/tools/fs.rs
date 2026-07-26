use crate::files;
use crate::response::{self, ToolResult};
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct FileSystemTool;

#[async_trait]
impl Tool for FileSystemTool {
    fn name(&self) -> &'static str {
        "mcp_android_file_system"
    }

    fn description(&self) -> &'static str {
        "File operations (list, read, push, pull)"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "read", "push", "pull"]
                    },
                    "path": { "type": "string", "description": "Device path for list/read, or remote path for push/pull" },
                    "local_path": { "type": "string", "description": "Local path for push (source) or pull (destination)" }
                },
                "required": ["action", "path"]
            }
        })
    }

    async fn execute(&self, args: &Value, _ctx: &crate::tools::ToolContext) -> ToolResult {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let local_path = args
            .get("local_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match action.as_str() {
            "list" => files::list_directory(&path).await,
            "read" => files::read_file(&path).await,
            "push" => {
                if local_path.is_empty() {
                    response::error_response("local_path required for push")
                } else {
                    files::push_file(&local_path, &path).await
                }
            }
            "pull" => {
                if local_path.is_empty() {
                    response::error_response("local_path required for pull")
                } else {
                    files::pull_file(&path, &local_path).await
                }
            }
            _ => response::error_response(format!("Unknown file action: {action}")),
        }
    }
}
