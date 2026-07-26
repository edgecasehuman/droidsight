use crate::app;
use crate::response::{self, ToolResult};
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

/// Actions on this tool that modify or remove application state (data,
/// installation, granted permissions, or availability) and therefore require an
/// explicit `confirm_destructive: true` before they run.
const DESTRUCTIVE_ACTIONS: [&str; 5] =
    ["uninstall", "clear_data", "permission", "enable", "disable"];

/// Returns a refusal result when `action` is destructive and the caller did not
/// pass `confirm_destructive: true`, mirroring the guard used by
/// `mcp_android_forensics_control`. Returns `None` when the action is either
/// non-destructive or explicitly confirmed, so execution may proceed.
fn destructive_confirmation_error(action: &str, args: &Value) -> Option<ToolResult> {
    if DESTRUCTIVE_ACTIONS.contains(&action)
        && args.get("confirm_destructive").and_then(Value::as_bool) != Some(true)
    {
        return Some(response::error_response(format!(
            "The '{action}' action modifies or removes application data, installation, \
             permissions, or availability. Re-issue the call with \"confirm_destructive\": true."
        )));
    }
    None
}

pub struct AppManageTool;

#[async_trait]
impl Tool for AppManageTool {
    fn name(&self) -> &'static str {
        "mcp_android_app_manage"
    }

    fn description(&self) -> &'static str {
        "Manage apps (launch, stop, list, install, crash_log, uninstall, clear_data, permission, enable, disable)"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["launch", "stop", "list", "install", "crash_log", "list_crashes", "get_crash", "uninstall", "clear_data", "permission", "enable", "disable", "get_foreground"],
                        "description": "The action to perform on the app"
                    },
                    "package_name": { "type": "string" },
                    "force_stop": { "type": "boolean" },
                    "path": { "type": "string" },
                    "third_party": { "type": "boolean" },
                    "permission": { "type": "string" },
                    "grant": { "type": "boolean" }
                    ,"confirm_destructive": {
                        "type": "boolean",
                        "description": "Must be true to run destructive actions (uninstall, clear_data, permission, enable, disable). Ignored by every other action."
                    }
                    ,"limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                    ,"wait_ms": crate::tools::wait_ms_property(1000)
                },
                "required": ["action"]
            }
        })
    }

    async fn execute(&self, args: &Value, ctx: &crate::tools::ToolContext) -> ToolResult {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let wait_ms = args
            .get("wait_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1000);

        let args = args.clone(); // Clone args to move into closure

        ctx.run_with_observation(wait_ms, || async move {
            if let Some(refusal) = destructive_confirmation_error(action.as_str(), &args) {
                return refusal;
            }
            match action.as_str() {
                "launch" => {
                    let pkg = crate::tools::required_str(&args, "package_name")?;
                    let force_stop = args
                        .get("force_stop")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    app::launch_app(pkg, force_stop).await
                }
                "stop" => {
                    let pkg = crate::tools::required_str(&args, "package_name")?;
                    app::stop_app(pkg).await
                }
                "list" => {
                    let third_party = args
                        .get("third_party")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    app::list_apps(third_party).await
                }
                "install" => {
                    let path = crate::tools::required_str(&args, "path")?;
                    app::install_apk(path).await
                }
                "crash_log" => {
                    let pkg = crate::tools::required_str(&args, "package_name")?;
                    app::crash_log(pkg).await
                }
                "list_crashes" => {
                    let pkg = args
                        .get("package_name")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let limit = args
                        .get("limit")
                        .and_then(Value::as_u64)
                        .unwrap_or(10)
                        .clamp(1, 100) as usize;
                    app::structured_crashes(pkg, limit, false).await
                }
                "get_crash" => {
                    let pkg = args
                        .get("package_name")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    app::structured_crashes(pkg, 1, true).await
                }
                "uninstall" => {
                    let pkg = crate::tools::required_str(&args, "package_name")?;
                    app::uninstall_app(pkg).await
                }
                "clear_data" => {
                    let pkg = crate::tools::required_str(&args, "package_name")?;
                    app::clear_app_data(pkg).await
                }
                "permission" => {
                    let pkg = crate::tools::required_str(&args, "package_name")?;
                    let perm = crate::tools::required_str(&args, "permission")?;
                    let grant = args
                        .get("grant")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true);
                    app::set_permission(pkg, perm, grant).await
                }
                "enable" => {
                    let pkg = crate::tools::required_str(&args, "package_name")?;
                    app::enable_app(pkg, true).await
                }
                "disable" => {
                    let pkg = crate::tools::required_str(&args, "package_name")?;
                    app::enable_app(pkg, false).await
                }
                "get_foreground" => app::get_foreground_app().await,
                _ => response::error_response(format!("Unknown action: {action}")),
            }
        })
        .await
    }
    fn needs_unlock(&self, args: &Value) -> bool {
        // A package listing, crash-log read, stop, install, or permission change
        // does not justify waking and unlocking the user's screen.
        matches!(args.get("action").and_then(Value::as_str), Some("launch"))
    }
}

#[cfg(test)]
mod tests {
    use super::{destructive_confirmation_error, AppManageTool, DESTRUCTIVE_ACTIONS};
    use crate::stream::StreamManager;
    use crate::tools::{Tool, ToolContext};
    use serde_json::json;
    use std::sync::Arc;

    fn ctx() -> ToolContext {
        ToolContext {
            stream_manager: Arc::new(StreamManager::new()),
        }
    }

    #[test]
    fn destructive_actions_are_refused_without_confirmation() {
        for action in DESTRUCTIVE_ACTIONS {
            let args = json!({ "action": action, "package_name": "com.example" });
            let refusal = destructive_confirmation_error(action, &args)
                .unwrap_or_else(|| panic!("{action} must require confirmation"));
            let error = refusal.expect_err("refusal must be an error result");
            let message = error["message"].as_str().unwrap_or_default();
            assert!(
                message.contains(action) && message.contains("confirm_destructive"),
                "refusal for {action} should name the action and the flag, got: {message}"
            );
        }
    }

    #[test]
    fn destructive_actions_proceed_once_confirmed() {
        for action in DESTRUCTIVE_ACTIONS {
            let args = json!({
                "action": action,
                "package_name": "com.example",
                "confirm_destructive": true
            });
            assert!(
                destructive_confirmation_error(action, &args).is_none(),
                "{action} with confirm_destructive:true must not be gated"
            );
        }
    }

    #[test]
    fn non_destructive_actions_are_never_gated() {
        for action in [
            "launch",
            "stop",
            "list",
            "install",
            "crash_log",
            "get_foreground",
        ] {
            let args = json!({ "action": action, "package_name": "com.example" });
            assert!(
                destructive_confirmation_error(action, &args).is_none(),
                "{action} is read-only/benign and must not require confirmation"
            );
        }
    }

    #[tokio::test]
    async fn execute_refuses_unconfirmed_clear_data_before_touching_device() {
        // The guard short-circuits with an error before any ADB call, so this
        // exercises the wired refusal path without a connected device.
        let tool = AppManageTool;
        let args = json!({ "action": "clear_data", "package_name": "com.example" });
        let result = tool.execute(&args, &ctx()).await;
        let error = result.expect_err("unconfirmed clear_data must return an error");
        assert!(error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("confirm_destructive"));
    }
}
