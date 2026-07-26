use crate::notifications;
use crate::response::ToolResult;
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct GetNotificationsTool;

#[async_trait]
impl Tool for GetNotificationsTool {
    fn name(&self) -> &'static str {
        "mcp_android_get_notifications"
    }

    fn description(&self) -> &'static str {
        "Dump posted notifications, including their unredacted message content"
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
        notifications::get_notifications().await
    }
}
