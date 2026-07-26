use crate::adb::Adb;
use crate::response::{self, ToolResult};
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct AppInstrumentationTool;

const WINDOW_FOCUS_COMMAND: &str =
    "dumpsys window windows | grep -E 'mCurrentFocus|mFocusedApp|mObscuringWindow'";

fn diagnostic_response(output: String) -> ToolResult {
    response::bounded_text_response(
        output,
        response::DEFAULT_TEXT_BUDGET_BYTES,
        response::TruncationStrategy::Head,
    )
}

#[async_trait]
impl Tool for AppInstrumentationTool {
    fn name(&self) -> &'static str {
        "mcp_android_app_instrumentation"
    }

    fn description(&self) -> &'static str {
        "Deep inspection of app state (dumpsys activity, window, ps, stack_trace)"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["dump_activity", "dump_window", "ps", "stack_trace"]
                    },
                    "package_name": { "type": "string" },
                    "pid": { "type": "integer", "description": "Required for stack_trace" }
                },
                "required": ["action"]
            }
        })
    }

    async fn execute(&self, args: &Value, _ctx: &crate::tools::ToolContext) -> ToolResult {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let package = args
            .get("package_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match action {
            "dump_activity" => {
                if package.is_empty() {
                    return response::error_response("package_name is required for dump_activity");
                }
                // Returns the raw `dumpsys activity <package>` output, which can
                // be large; the diagnostic response applies a byte budget.
                let cmd = format!("dumpsys activity {}", crate::adb::shell_quote(package));
                match Adb::shell_native(&cmd).await {
                    Ok(out) => diagnostic_response(out),
                    Err(e) => response::error_response(e.to_string()),
                }
            }
            "dump_window" => {
                // Fast focus check: dumpsys window windows filtered to the
                // current/focused window fields (see WINDOW_FOCUS_COMMAND).
                match Adb::shell_native(WINDOW_FOCUS_COMMAND).await {
                    Ok(out) => diagnostic_response(out),
                    Err(e) => response::error_response(e.to_string()),
                }
            }
            "ps" => {
                // ps -A | grep package
                let cmd = if !package.is_empty() {
                    format!("ps -A | grep -F -- {}", crate::adb::shell_quote(package))
                } else {
                    "ps -A".to_string()
                };
                match Adb::shell_native(&cmd).await {
                    Ok(out) => diagnostic_response(out),
                    Err(e) => response::error_response(e.to_string()),
                }
            }
            "stack_trace" => {
                let pid = args.get("pid").and_then(serde_json::Value::as_u64);
                if let Some(pid) = pid {
                    // Send SIGQUIT to make the process dump its stacks, then read
                    // the traces file. Reading /data/anr is often restricted, so
                    // the read may fail even after the signal is delivered.
                    let sig_cmd = format!("kill -3 {pid}");
                    let _ = Adb::shell_native(&sig_cmd).await;

                    // Give the runtime time to write the trace.
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                    match Adb::shell_native("cat /data/anr/traces.txt").await {
                        Ok(out) => diagnostic_response(out),
                        Err(e) => response::error_response(format!(
                            "Signal sent, but failed to read traces (Permission Hint?): {e}"
                        )),
                    }
                } else {
                    response::error_response("pid is required for stack_trace")
                }
            }
            _ => response::error_response(format!("Unknown action: {action}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{diagnostic_response, WINDOW_FOCUS_COMMAND};
    use crate::response::DEFAULT_TEXT_BUDGET_BYTES;

    #[test]
    fn diagnostic_adapter_reports_head_truncation() {
        let output = format!("HEADER:{}", "x".repeat(DEFAULT_TEXT_BUDGET_BYTES));
        let result = diagnostic_response(output).unwrap();
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("HEADER:"));
        assert_eq!(result["metadata"]["truncation"]["strategy"], "head");
        assert_eq!(
            result["metadata"]["truncation"]["limit_bytes"],
            DEFAULT_TEXT_BUDGET_BYTES
        );
    }

    #[test]
    fn window_diagnostic_accepts_samsung_focus_marker() {
        assert!(WINDOW_FOCUS_COMMAND.contains("mObscuringWindow"));
    }
}
