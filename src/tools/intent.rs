use crate::intents;
use crate::response::ToolResult;
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct OpenUrlTool;

#[async_trait]
impl Tool for OpenUrlTool {
    fn name(&self) -> &'static str {
        "mcp_android_open_url"
    }

    fn description(&self) -> &'static str {
        "Open URL"
    }

    fn schema(&self) -> Value {
        json!({
             "inputSchema": {
                 "type": "object",
                 "properties": {
                     "url": { "type": "string" },
                     "wait_ms": crate::tools::wait_ms_property(2000)
                 },
                 "required": ["url"]
             }
        })
    }

    async fn execute(&self, args: &Value, ctx: &crate::tools::ToolContext) -> ToolResult {
        let wait_ms = args
            .get("wait_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(2000); // 2s wait for browser/app load

        let args = args.clone();
        ctx.run_with_observation(wait_ms, || async move {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
            intents::open_url(url).await
        })
        .await
    }
}

pub struct StartIntentTool;

#[async_trait]
impl Tool for StartIntentTool {
    fn name(&self) -> &'static str {
        "mcp_android_start_intent"
    }

    fn description(&self) -> &'static str {
        "Start Intent (advanced)"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string" },
                    "uri": { "type": "string" },
                    "package_name": { "type": "string" },
                    "activity_name": { "type": "string" },
                    "mimetype": { "type": "string" },
                    "wait_ms": crate::tools::wait_ms_property(2000)
                }
            }
        })
    }

    async fn execute(&self, args: &Value, ctx: &crate::tools::ToolContext) -> ToolResult {
        let wait_ms = args
            .get("wait_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(2000);

        let args = args.clone();
        ctx.run_with_observation(wait_ms, || async move {
            let action = args.get("action").and_then(|v| v.as_str());
            let uri = args.get("uri").and_then(|v| v.as_str());
            let pkg = args.get("package_name").and_then(|v| v.as_str());
            let act = args.get("activity_name").and_then(|v| v.as_str());
            let mime = args.get("mimetype").and_then(|v| v.as_str());
            intents::start_intent(action, uri, pkg, act, mime).await
        })
        .await
    }
}
