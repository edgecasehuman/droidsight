use crate::adb::Adb;
use crate::events;
use crate::logs;
use crate::response::{self, ToolResult};
use crate::tools::Tool;
use async_trait::async_trait;
use regex;
use serde_json::{json, Value};

pub struct LogStreamTool;

#[async_trait]
impl Tool for LogStreamTool {
    fn name(&self) -> &'static str {
        "mcp_android_diagnostic_stream"
    }

    fn description(&self) -> &'static str {
        "Read logs and events"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["logcat", "clear", "recent_events", "semantic_events"]
                    },
                    "lines": { "type": "integer" },
                    "limit": { "type": "integer" }
                },
                "required": ["action"]
            }
        })
    }

    async fn execute(&self, args: &Value, _ctx: &crate::tools::ToolContext) -> ToolResult {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

        match action {
            "logcat" => {
                let lines = args
                    .get("lines")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(100)
                    .clamp(1, 10_000) as i32;
                logs::read_logcat(lines).await
            }
            "clear" => logs::clear_logcat().await,
            "recent_events" => {
                let limit = args
                    .get("limit")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(50)
                    .clamp(1, 10_000) as i32;
                // Reads an in-memory buffer, so it does not block the runtime.
                events::read_recent_events(limit)
            }
            "semantic_events" => {
                let limit = args
                    .get("limit")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(50)
                    .clamp(1, 10_000) as i32;
                events::read_semantic_events(limit)
            }
            _ => response::error_response(format!("Unknown diagnostic action: {action}")),
        }
    }
}

pub struct ReadRecentEventsTool;

#[async_trait]
impl Tool for ReadRecentEventsTool {
    fn name(&self) -> &'static str {
        "mcp_android_read_recent_events"
    }

    fn description(&self) -> &'static str {
        "Get buffered events"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer" }
                }
            }
        })
    }

    async fn execute(&self, args: &Value, _ctx: &crate::tools::ToolContext) -> ToolResult {
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(50)
            .clamp(1, 10_000) as i32;
        events::read_recent_events(limit)
    }
}

pub struct LogFilterTool;

#[async_trait]
impl Tool for LogFilterTool {
    fn name(&self) -> &'static str {
        "mcp_android_log_filter"
    }

    fn description(&self) -> &'static str {
        "Filter logs by regex, tag, or priority"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filter_regex": { "type": "string", "description": "Regex pattern to match message content" },
                    "tag": { "type": "string", "description": "Exact tag to match" },
                    "min_priority": { "type": "string", "enum": ["V", "D", "I", "W", "E", "F"] },
                    "pid": { "type": "integer", "description": "Filter by Process ID" },
                    "lines": { "type": "integer", "default": 200, "description": "Number of recent lines to scan" }
                }
            }
        })
    }

    async fn execute(&self, args: &Value, _ctx: &crate::tools::ToolContext) -> ToolResult {
        let lines_count = args
            .get("lines")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(200)
            .clamp(1, 10_000) as i32;
        let filter_regex = args.get("filter_regex").and_then(|v| v.as_str());
        let tag_filter = args.get("tag").and_then(|v| v.as_str());
        let pid_filter = args.get("pid").and_then(serde_json::Value::as_i64);
        let min_priority = args.get("min_priority").and_then(|v| v.as_str());

        // Run ADb logcat
        // logcat -d -t N -v threadtime
        // Format: DATE TIME PID TID LEVEL TAG: MSG
        let output = match Adb::shell(&[
            "shell",
            "logcat",
            "-d",
            "-t",
            &lines_count.to_string(),
            "-v",
            "brief",
        ])
        .await
        {
            Ok(o) => o,
            Err(e) => return response::error_response(e.to_string()),
        };

        let mut filtered_lines = Vec::new();
        let re = if let Some(pattern) = filter_regex {
            match regex::Regex::new(pattern) {
                Ok(r) => Some(r),
                Err(e) => return response::error_response(format!("Invalid Regex: {e}")),
            }
        } else {
            None
        };

        let priority_map = |c: char| -> i32 {
            match c {
                'V' => 1,
                'D' => 2,
                'I' => 3,
                'W' => 4,
                'E' => 5,
                'F' => 6,
                _ => 0,
            }
        };

        let min_prio_val =
            min_priority.map_or(0, |p| priority_map(p.chars().next().unwrap_or('V')));

        for line in output.lines() {
            if line.trim().is_empty() {
                continue;
            }

            // Brief format: P/TAG(PID): Msg
            // e.g. I/ActivityManager( 1234): Start proc...

            if min_prio_val > 0 {
                if let Some(first_char) = line.chars().next() {
                    if priority_map(first_char) < min_prio_val {
                        continue;
                    }
                }
            }

            if let Some(tag) = tag_filter {
                // Match the "/TAG" segment of the brief logcat format.
                let prefix_check = format!("/{tag}");
                if !line.contains(&prefix_check) {
                    continue;
                }
            }

            // PID Filter
            if let Some(pid) = pid_filter {
                let pid_marker = format!("({pid})"); // "( 123)" or "(123)" - brief format usually has "( PID)"
                if !line.contains(&pid_marker) {
                    continue;
                }
            }

            if let Some(ref r) = re {
                if !r.is_match(line) {
                    continue;
                }
            }

            filtered_lines.push(line);
        }

        if filtered_lines.is_empty() {
            return response::text_response("No matching logs found in recent buffer.");
        }

        response::bounded_text_response(
            filtered_lines.join("\n"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Tail,
        )
    }
}
