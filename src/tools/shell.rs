use crate::response::ToolResult;
use crate::system;
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &'static str {
        "mcp_android_run_shell"
    }

    fn description(&self) -> &'static str {
        "Run an arbitrary device shell command. No command filtering is applied."
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Command line evaluated by the device shell. Shell metacharacters are interpreted, not escaped." },
                    "wait_ms": crate::tools::wait_ms_property(500)
                },
                "required": ["command"]
            }
        })
    }

    async fn execute(&self, args: &Value, ctx: &crate::tools::ToolContext) -> ToolResult {
        let wait_ms = args
            .get("wait_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(500);

        let args = args.clone();
        ctx.run_with_observation(wait_ms, || async move {
            let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            system::run_shell(cmd).await
        })
        .await
    }
}
