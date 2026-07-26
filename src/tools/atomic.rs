use crate::input;
use crate::response::{self, ToolResult};
use crate::tools::{Tool, ToolContext};
use crate::vision;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub struct AtomicTapTextTool;

#[async_trait]
impl Tool for AtomicTapTextTool {
    fn name(&self) -> &'static str {
        "mcp_android_tap_text"
    }

    fn description(&self) -> &'static str {
        "Scans for text and taps it immediately when found. Retries until timeout. Use this for button clicks to avoid latency misses and drift."
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Text to find and tap"
                    },
                    "timeout": {
                        "type": "number",
                        "description": "Timeout in seconds (default 10.0)"
                    }
                },
                "required": ["text"]
            }
        })
    }

    async fn execute(&self, args: &Value, _ctx: &ToolContext) -> ToolResult {
        let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let timeout_secs = args
            .get("timeout")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(10.0);

        if text.is_empty() {
            return response::error_response("text argument is required");
        }
        if !timeout_secs.is_finite() || timeout_secs <= 0.0 || timeout_secs > 120.0 {
            return response::error_response("timeout must be finite and in (0, 120] seconds");
        }

        let start = Instant::now();
        let timeout = Duration::from_secs_f64(timeout_secs);
        let mut attempts = 0;

        tracing::info!(
            "AtomicTapText: Starting scan (query redacted), timeout={}s",
            timeout_secs
        );

        loop {
            if start.elapsed() > timeout {
                return response::error_response(format!(
                    "Timeout waiting for text: '{text}' after {attempts} attempts"
                ));
            }
            attempts += 1;

            // 1. Scan: single native screenshot -> Tesseract TSV
            match vision::find_text(text).await {
                Ok(resp) => {
                    // resp is { "content": [ { "type": "text", "text": "..." } ], "isError": false }
                    if let Some(content) = resp.get("content").and_then(|c| c.as_array()) {
                        if let Some(first_block) = content.first() {
                            if let Some(match_json_str) =
                                first_block.get("text").and_then(|t| t.as_str())
                            {
                                // Parse the inner JSON array
                                if let Ok(matches) =
                                    serde_json::from_str::<Vec<Value>>(match_json_str)
                                {
                                    if let Some(first_match) = matches.first() {
                                        let x = first_match
                                            .get("x")
                                            .and_then(serde_json::Value::as_i64)
                                            .unwrap_or(0)
                                            as i32;
                                        let y = first_match
                                            .get("y")
                                            .and_then(serde_json::Value::as_i64)
                                            .unwrap_or(0)
                                            as i32;
                                        let w = first_match
                                            .get("w")
                                            .and_then(serde_json::Value::as_i64)
                                            .unwrap_or(0)
                                            as i32;
                                        let h = first_match
                                            .get("h")
                                            .and_then(serde_json::Value::as_i64)
                                            .unwrap_or(0)
                                            as i32;

                                        // Calculate center
                                        let cx = x + w / 2;
                                        let cy = y + h / 2;

                                        tracing::info!(
                                            "AtomicTapText: Found query at {},{}. Tapping...",
                                            cx,
                                            cy
                                        );

                                        // 2. Act Immediately
                                        // Using Native source because OCR currently uses `screencap -p` (native resolution)
                                        return input::tap_raw(
                                            cx,
                                            cy,
                                            crate::device_metrics::CoordinateSource::Native,
                                        )
                                        .await;
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("AtomicTapText: OCR error: {:?}", e);
                }
            }

            // Sleep a bit to avoid thrashing CPU/ADB
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}
