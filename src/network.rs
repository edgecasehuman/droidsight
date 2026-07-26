use crate::adb::shell_quote;
use crate::adb::Adb;
use crate::response::{self, ToolResult};

pub async fn set_wifi(enabled: bool) -> ToolResult {
    let state = if enabled { "enabled" } else { "disabled" };
    match Adb::device_shell(&format!("cmd wifi set-wifi-enabled {state}")).await {
        Ok(_) => response::text_response(format!("WiFi {state}")),
        Err(e) => response::error_response(format!("Failed to set WiFi: {e}")),
    }
}

pub async fn set_mobile_data(enabled: bool) -> ToolResult {
    let state = if enabled { "enable" } else { "disable" };
    match Adb::device_shell(&format!("svc data {state}")).await {
        Ok(_) => response::text_response(format!("Mobile Data {state}d")),
        Err(e) => response::error_response(format!("Failed to set Mobile Data: {e}")),
    }
}

pub async fn scan_wifi() -> ToolResult {
    // Trigger scan
    let _ = Adb::device_shell("cmd wifi start-scan").await;
    match Adb::device_shell("cmd wifi list-scan-results").await {
        Ok(output) => response::bounded_text_response(
            output,
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(format!("Failed to scan WiFi: {e}")),
    }
}

pub async fn connect_wifi(ssid: &str, password: &str) -> ToolResult {
    // cmd wifi connect-network <ssid> <open|owe|wpa2|wpa3> <password>.
    // WPA2 is used as the default security type.
    match Adb::device_shell(&format!(
        "cmd wifi connect-network {} wpa2 {}",
        shell_quote(ssid),
        shell_quote(password)
    ))
    .await
    {
        Ok(res) => bounded_status_response("Connect requested: ", &res),
        Err(e) => response::error_response(format!("Failed to connect WiFi: {e}")),
    }
}

pub async fn forget_wifi(ssid: &str) -> ToolResult {
    match Adb::device_shell(&format!("cmd wifi forget-network {}", shell_quote(ssid))).await {
        Ok(res) => bounded_status_response("Forget requested: ", &res),
        Err(e) => response::error_response(format!("Failed to forget WiFi: {e}")),
    }
}

pub async fn set_proxy(host: &str, port: i32) -> ToolResult {
    let cmd = if port == 0 {
        "settings put global http_proxy :0".to_string()
    } else {
        format!(
            "settings put global http_proxy {}",
            shell_quote(&format!("{host}:{port}"))
        )
    };

    match Adb::device_shell(&cmd).await {
        Ok(_) => response::text_response("Proxy updated"),
        Err(e) => response::error_response(format!("Failed to set proxy: {e}")),
    }
}

pub async fn make_call(number: &str) -> ToolResult {
    match Adb::device_shell(&format!(
        "am start -a android.intent.action.CALL -d {}",
        shell_quote(&format!("tel:{number}"))
    ))
    .await
    {
        Ok(_) => response::bounded_text_response(
            format!("Calling {number}"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(format!("Failed to make call: {e}")),
    }
}

pub async fn send_sms(number: &str, message: &str) -> ToolResult {
    // The direct `service call isms` path is brittle and varies by Android
    // version, so open the SMS composer with the message pre-filled. Sending
    // the message typically still requires a user tap.
    match Adb::device_shell(&format!(
        "am start -a android.intent.action.SENDTO -d {} --es sms_body {}",
        shell_quote(&format!("sms:{number}")),
        shell_quote(message)
    ))
    .await
    {
        Ok(_) => response::bounded_text_response(
            format!("Opened SMS composer for {number}"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(format!("Failed to send SMS: {e}")),
    }
}

pub async fn pair_wireless(host: &str, port: i64, code: &str) -> ToolResult {
    // Wireless ADB pairing via the native `adb pair <host>:<port> <code>`
    // subprocess, which handles the TLS handshake.
    let addr = format!("{host}:{port}");
    tracing::info!("Attempting wireless pairing to {} with code", addr);

    match Adb::execute_host(
        vec!["pair".to_string(), addr, code.to_string()],
        std::time::Duration::from_secs(15),
    )
    .await
    {
        Ok(output) => {
            if output.success {
                let stdout = String::from_utf8_lossy(&output.stdout);
                bounded_status_response("Pairing successful: ", &stdout)
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                response::error_response(format!("Pairing failed ({}): {}", output.status, stderr))
            }
        }
        Err(e) => response::error_response(format!("Failed to execute adb pair: {e}")),
    }
}

fn bounded_status_response(prefix: &str, output: &str) -> ToolResult {
    response::bounded_text_response(
        format!("{prefix}{output}"),
        response::DEFAULT_TEXT_BUDGET_BYTES,
        response::TruncationStrategy::Head,
    )
}

#[cfg(test)]
mod tests {
    use super::bounded_status_response;
    use crate::response::DEFAULT_TEXT_BUDGET_BYTES;

    #[test]
    fn network_status_adapter_retains_context_when_truncated() {
        let result = bounded_status_response(
            "Pairing successful: ",
            &"x".repeat(DEFAULT_TEXT_BUDGET_BYTES),
        )
        .unwrap();
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("Pairing successful: "));
        assert_eq!(result["metadata"]["truncation"]["strategy"], "head");
    }
}
