use crate::adb::Adb;
use crate::response::{self, ToolResult};

pub async fn launch_app(package: &str, force_stop: bool) -> ToolResult {
    if force_stop {
        let _ = stop_app(package).await;
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
    match Adb::shell(&[
        "shell",
        "monkey",
        "-p",
        package,
        "-c",
        "android.intent.category.LAUNCHER",
        "1",
    ])
    .await
    {
        Ok(_) => response::bounded_text_response(
            format!("Launched {package} (force_stop={force_stop})"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn stop_app(package: &str) -> ToolResult {
    match Adb::shell(&["shell", "am", "force-stop", package]).await {
        Ok(_) => response::bounded_text_response(
            format!("Stopped {package}"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn list_apps(third_party: bool) -> ToolResult {
    let mut args = vec!["shell", "pm", "list", "packages", "--user", "0"];
    if third_party {
        args.push("-3");
    }

    match Adb::shell(&args).await {
        Ok(output) => {
            let packages: Vec<String> = output
                .lines()
                .map(|line| line.trim_start_matches("package:").to_string())
                .collect();
            response::bounded_text_response(
                serde_json::to_string(&packages).unwrap(),
                response::DEFAULT_TEXT_BUDGET_BYTES,
                response::TruncationStrategy::Head,
            )
        }
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn uninstall_app(package: &str) -> ToolResult {
    match Adb::shell(&["shell", "pm", "uninstall", package]).await {
        Ok(_) => response::bounded_text_response(
            format!("Uninstalled {package}"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn clear_app_data(package: &str) -> ToolResult {
    match Adb::shell(&["shell", "pm", "clear", package]).await {
        Ok(_) => response::bounded_text_response(
            format!("Cleared data for {package}"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn set_permission(package: &str, permission: &str, grant: bool) -> ToolResult {
    let action = if grant { "grant" } else { "revoke" };
    match Adb::shell(&["shell", "pm", action, package, permission]).await {
        Ok(_) => response::bounded_text_response(
            format!("{action} {permission} for {package}"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn enable_app(package: &str, enable: bool) -> ToolResult {
    let cmd = if enable {
        vec!["shell", "pm", "enable", package]
    } else {
        // disable-user is safer as it keeps data but disables the app for the current user
        vec!["shell", "pm", "disable-user", "--user", "0", package]
    };

    match Adb::shell(&cmd).await {
        Ok(msg) => response::bounded_text_response(
            format!("Set {package} enabled state to {enable}. Msg: {msg}"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn install_apk(path: &str) -> ToolResult {
    let path = match crate::files::local_read_path(path) {
        Ok(path) => path,
        Err(error) => return response::error_response(error),
    };
    let path = path.to_string_lossy();
    match Adb::shell(&["install", "-r", &path]).await {
        Ok(_) => response::bounded_text_response(
            format!("Installed {path}"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn crash_log(package: &str) -> ToolResult {
    let cmd = format!(
        "dumpsys dropbox data_app_crash --print | grep -F -A 20 -- {}",
        crate::adb::shell_quote(package)
    );
    match Adb::shell(&["shell", &cmd]).await {
        Ok(output) => crash_log_response(output),
        Err(e) => response::error_response(e.to_string()),
    }
}

fn crash_log_response(output: String) -> ToolResult {
    let message = if output.is_empty() {
        "No recent crash found".to_string()
    } else {
        output
    };
    response::bounded_text_response(
        message,
        response::DEFAULT_TEXT_BUDGET_BYTES,
        response::TruncationStrategy::Tail,
    )
}

pub async fn structured_crashes(package: &str, limit: usize, detail: bool) -> ToolResult {
    let output =
        match Adb::shell(&["logcat", "-b", "crash", "-b", "events", "-d", "-t", "4000"]).await {
            Ok(output) => output,
            Err(error) => {
                return response::error_response(format!("Could not read crash log: {error}"))
            }
        };
    let crashes = crate::crash_reports::parse(&output);
    let filtered = crate::crash_reports::filtered(&crashes, package);
    if detail {
        return match filtered.first() {
            Some(crash) => response::bounded_text_response(
                serde_json::to_string(crash).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
                response::DEFAULT_TEXT_BUDGET_BYTES,
                response::TruncationStrategy::Head,
            ),
            None => response::bounded_text_response(
                format!(
                    "No crashes found{}",
                    if package.is_empty() {
                        String::new()
                    } else {
                        format!(" for {package}")
                    }
                ),
                response::DEFAULT_TEXT_BUDGET_BYTES,
                response::TruncationStrategy::Head,
            ),
        };
    }
    let summaries: Vec<_> = filtered
        .into_iter()
        .take(limit)
        .map(|crash| {
            serde_json::json!({
                "type": crash.kind, "timestamp": crash.timestamp, "process": crash.process,
                "summary": crash.summary.chars().take(300).collect::<String>()
            })
        })
        .collect();
    response::bounded_text_response(
        serde_json::json!({"count": summaries.len(), "crashes": summaries}).to_string(),
        response::DEFAULT_TEXT_BUDGET_BYTES,
        response::TruncationStrategy::Head,
    )
}

pub async fn get_foreground_app() -> ToolResult {
    // Retry loop to handle focus/activity transition states.
    for i in 0..3 {
        // Method 1: dumpsys window windows (Parse mCurrentFocus)
        // We execute without grep to avoid exit code 1 if not found immediately
        if let Ok(output) = Adb::shell(&["shell", "dumpsys window windows"]).await {
            for line in output.lines() {
                let line = line.trim();
                if line.starts_with("mCurrentFocus=")
                    || line.starts_with("mFocusedApp=")
                    || line.starts_with("mObscuringWindow=")
                {
                    if let Some(component) = foreground_component(line) {
                        return response::bounded_text_response(
                            component,
                            response::DEFAULT_TEXT_BUDGET_BYTES,
                            response::TruncationStrategy::Head,
                        );
                    }
                }
            }
        }

        // Method 2: dumpsys activity activities (Parse mResumedActivity)
        if let Ok(output) = Adb::shell(&["shell", "dumpsys activity activities"]).await {
            for line in output.lines() {
                let line = line.trim();
                if line.starts_with("mResumedActivity:") || line.starts_with("topResumedActivity=")
                {
                    if let Some(component) = foreground_component(line) {
                        return response::bounded_text_response(
                            component,
                            response::DEFAULT_TEXT_BUDGET_BYTES,
                            response::TruncationStrategy::Head,
                        );
                    }
                }
            }
        }

        // Wait before retry
        if i < 2 {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
    }

    response::error_response("No foreground app found in dumpsys after 3 attempts")
}

fn foreground_component(line: &str) -> Option<String> {
    line.split_whitespace()
        .find(|token| token.contains('/'))
        .map(|token| {
            token
                .trim_matches(|character: char| {
                    matches!(character, '{' | '}' | '[' | ']' | ',' | ';')
                })
                .to_string()
        })
        .filter(|component| !component.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{crash_log_response, foreground_component};
    use crate::response::DEFAULT_TEXT_BUDGET_BYTES;

    #[test]
    fn crash_log_adapter_keeps_newest_tail_and_reports_truncation() {
        let output = format!("{}LATEST CRASH", "x".repeat(DEFAULT_TEXT_BUDGET_BYTES));
        let result = crash_log_response(output).unwrap();
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .ends_with("LATEST CRASH"));
        assert_eq!(result["metadata"]["truncation"]["strategy"], "tail");
    }

    #[test]
    fn foreground_parser_accepts_aosp_and_samsung_markers() {
        assert_eq!(
            foreground_component("mCurrentFocus=Window{abc u0 com.example/.MainActivity}")
                .as_deref(),
            Some("com.example/.MainActivity")
        );
        assert_eq!(
            foreground_component(
                "topResumedActivity=ActivityRecord{abc u0 com.android.settings/.SubSettings} t346}"
            )
            .as_deref(),
            Some("com.android.settings/.SubSettings")
        );
        assert_eq!(
            foreground_component(
                "mObscuringWindow=Window{abc u0 com.android.settings/com.android.settings.SubSettings}"
            )
            .as_deref(),
            Some("com.android.settings/com.android.settings.SubSettings")
        );
    }

    #[test]
    fn empty_crash_log_preserves_existing_message_shape() {
        let result = crash_log_response(String::new()).unwrap();
        assert_eq!(result["content"][0]["text"], "No recent crash found");
        assert!(result.get("metadata").is_none());
    }
}
