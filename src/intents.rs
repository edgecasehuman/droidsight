use crate::adb::Adb;
use crate::response::{self, ToolResult};

fn intent_start_error(output: &str) -> Option<&str> {
    output.lines().find_map(|line| {
        let line = line.trim();
        if line.starts_with("Error:")
            || line.starts_with("Exception occurred while executing")
            || line.contains("Activity not started, unable to resolve Intent")
        {
            Some(line)
        } else {
            None
        }
    })
}

fn started_response(prefix: &str, output: String) -> ToolResult {
    if let Some(error) = intent_start_error(&output) {
        return response::error_response(error);
    }

    response::bounded_text_response(
        format!("{prefix}{output}"),
        response::DEFAULT_TEXT_BUDGET_BYTES,
        response::TruncationStrategy::Head,
    )
}

async fn run_activity_manager(args: &[&str]) -> anyhow::Result<String> {
    let output = Adb::shell_output(args).await?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if let Some(error) = intent_start_error(&stderr).or_else(|| intent_start_error(&stdout)) {
        anyhow::bail!(error.to_string());
    }
    Ok(stdout)
}

pub async fn open_url(url: &str) -> ToolResult {
    match run_activity_manager(&[
        "shell",
        "am",
        "start",
        "-a",
        "android.intent.action.VIEW",
        "-d",
        url,
    ])
    .await
    {
        Ok(_) => response::bounded_text_response(
            format!("Opened {url}"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn start_intent(
    action: Option<&str>,
    uri: Option<&str>,
    package: Option<&str>,
    activity: Option<&str>,
    mimetype: Option<&str>,
) -> ToolResult {
    let mut distinct_cmd: Vec<String> =
        vec!["shell".to_string(), "am".to_string(), "start".to_string()];

    if let Some(a) = action {
        distinct_cmd.push("-a".to_string());
        distinct_cmd.push(a.to_string());
    }

    if let Some(u) = uri {
        distinct_cmd.push("-d".to_string());
        distinct_cmd.push(u.to_string());
    }

    if let Some(t) = mimetype {
        distinct_cmd.push("-t".to_string());
        distinct_cmd.push(t.to_string());
    }

    if let (Some(pkg), Some(act)) = (package, activity) {
        let component = format!("{pkg}/{act}");
        distinct_cmd.push("-n".to_string());
        distinct_cmd.push(component);
    } else if let Some(pkg) = package {
        // Package only, no action or URI: just launch the app.
        if action.is_none() && uri.is_none() {
            return crate::app::launch_app(pkg, false).await;
        }
        // With an action, scope the intent to this package.
        distinct_cmd.push("-p".to_string());
        distinct_cmd.push(pkg.to_string());
    }

    // Convert to Vec<&str> for Adb
    let args: Vec<&str> = distinct_cmd
        .iter()
        .map(std::string::String::as_str)
        .collect();

    match run_activity_manager(&args).await {
        Ok(out) => started_response("Intent Started: ", out),
        Err(e) => response::error_response(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{intent_start_error, started_response};

    #[test]
    fn recognizes_android_activity_manager_errors_on_successful_process_exit() {
        let output = "Starting: Intent { act=invalid }\nError: Activity not started, unable to resolve Intent { act=invalid }";
        assert_eq!(
            intent_start_error(output),
            Some("Error: Activity not started, unable to resolve Intent { act=invalid }")
        );
        assert!(started_response("Intent Started: ", output.to_string()).is_err());
    }

    #[test]
    fn accepts_normal_activity_manager_output() {
        let output = "Starting: Intent { act=android.settings.SETTINGS }";
        assert_eq!(intent_start_error(output), None);
        assert!(started_response("Intent Started: ", output.to_string()).is_ok());
    }
}
