use crate::response::ToolResult;
use crate::session;
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SessionTool;

#[async_trait]
impl Tool for SessionTool {
    fn name(&self) -> &'static str {
        "mcp_android_start_session"
    }

    fn description(&self) -> &'static str {
        "Mark the start of a session. Acknowledges the call without holding server state"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_path": { "type": "string", "description": "Label echoed back in the acknowledgement" },
                    "wait_ms": crate::tools::wait_ms_property(500)
                }
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
            let path = args
                .get("project_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            session::start_session(path)
        })
        .await
    }
}

pub struct StopSessionTool;

#[async_trait]
impl Tool for StopSessionTool {
    fn name(&self) -> &'static str {
        "mcp_android_stop_session"
    }

    fn description(&self) -> &'static str {
        "Mark the end of a session. Acknowledges the call without holding server state"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        })
    }

    async fn execute(&self, _args: &Value, _ctx: &crate::tools::ToolContext) -> ToolResult {
        session::stop_session()
    }
}

pub struct RunMacroTool;

#[async_trait]
impl Tool for RunMacroTool {
    fn name(&self) -> &'static str {
        "mcp_android_run_macro"
    }

    fn description(&self) -> &'static str {
        "Run device shell commands in sequence, stopping at the first failure"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "commands": { "type": "array", "items": { "type": "string" }, "description": "Up to 100 command lines, each evaluated by the device shell in order" },
                    "wait_ms": crate::tools::wait_ms_property(1000)
                },
                "required": ["commands"]
            }
        })
    }

    async fn execute(&self, args: &Value, ctx: &crate::tools::ToolContext) -> ToolResult {
        let wait_ms = args
            .get("wait_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1000);

        let args = args.clone();
        ctx.run_with_observation(wait_ms, || async move {
            let commands = match args.get("commands").and_then(Value::as_array) {
                Some(commands) if commands.len() <= 100 => commands,
                Some(_) => {
                    return crate::response::error_response("A macro is limited to 100 commands")
                }
                None => return crate::response::error_response("commands must be an array"),
            };
            let mut cmds = Vec::with_capacity(commands.len());
            for (index, command) in commands.iter().enumerate() {
                match command.as_str().filter(|command| !command.is_empty()) {
                    Some(command) => cmds.push(command.to_string()),
                    None => {
                        return crate::response::error_response(format!(
                            "commands[{index}] must be a non-empty string"
                        ))
                    }
                }
            }
            session::run_macro(cmds).await
        })
        .await
    }
}
