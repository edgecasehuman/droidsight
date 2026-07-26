use crate::recording;
use crate::response::ToolResult;
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct StartRecordingTool;

#[async_trait]
impl Tool for StartRecordingTool {
    fn name(&self) -> &'static str {
        "mcp_android_start_recording"
    }

    fn description(&self) -> &'static str {
        "Start a screen recording on the device (max 180s, written to /sdcard/mcp_rec.mp4)"
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
        recording::start_recording().await
    }
}

pub struct StopRecordingTool;

#[async_trait]
impl Tool for StopRecordingTool {
    fn name(&self) -> &'static str {
        "mcp_android_stop_recording"
    }

    fn description(&self) -> &'static str {
        "Stop the screen recording started by this server. The file remains on the device"
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
        recording::stop_recording().await
    }
}
