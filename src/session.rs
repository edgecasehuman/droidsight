use crate::adb::Adb;
use crate::response::{self, ToolResult};

// Stateless session: acknowledges start/stop without holding server-side state.
pub fn start_session(project_path: &str) -> ToolResult {
    response::bounded_text_response(
        format!("Session started for {project_path}"),
        response::DEFAULT_TEXT_BUDGET_BYTES,
        response::TruncationStrategy::Head,
    )
}

pub fn stop_session() -> ToolResult {
    response::text_response("Session stopped")
}

pub async fn run_macro(commands: Vec<String>) -> ToolResult {
    let mut results = Vec::new();
    for (index, cmd) in commands.into_iter().enumerate() {
        match Adb::device_shell(&cmd).await {
            Ok(out) => results.push(format!("Command {} output:\n{}", index + 1, out)),
            Err(e) => {
                return response::error_response(format!(
                    "Macro command {} failed: {}",
                    index + 1,
                    e
                ))
            }
        }
    }
    response::bounded_text_response(
        results.join("\n---\n"),
        response::DEFAULT_TEXT_BUDGET_BYTES,
        response::TruncationStrategy::Head,
    )
}
