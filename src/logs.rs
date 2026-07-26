use crate::adb::Adb;
use crate::response::{self, ToolResult};

pub async fn read_logcat(lines: i32) -> ToolResult {
    match Adb::shell(&["shell", "logcat", "-d", "-t", &lines.to_string()]).await {
        Ok(output) => response::bounded_text_response(
            output,
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Tail,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn clear_logcat() -> ToolResult {
    match Adb::shell(&["shell", "logcat", "-c"]).await {
        Ok(_) => response::text_response("Logs cleared"),
        Err(e) => response::error_response(e.to_string()),
    }
}
