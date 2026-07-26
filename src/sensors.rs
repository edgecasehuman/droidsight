use crate::adb::Adb;
use crate::response::{self, ToolResult};

pub async fn set_gps(lat: f64, lng: f64) -> ToolResult {
    // Uses 'adb emu' which talks to the emulator console.
    // 'device_shell' runs 'adb shell <cmd>', but 'emu' is an adb host command, not a device shell command.
    match Adb::shell(&["emu", "geo", "fix", &lng.to_string(), &lat.to_string()]).await {
        Ok(res) => {
             if res.contains("error") || res.contains("ko") {
                 response::error_response(format!("GPS Mock Failed: {res}. (Note: This tool only works on Android Emulators, not physical devices)"))
             } else {
                 response::bounded_text_response(
                     format!("GPS set to {lat}, {lng}: {res}"),
                     response::DEFAULT_TEXT_BUDGET_BYTES,
                     response::TruncationStrategy::Head,
                 )
             }
        },
        Err(e) => response::error_response(format!("Failed to set GPS: {e}. (Note: This tool only works on Android Emulators, not physical devices)"))
    }
}

pub async fn set_battery(level: i32, status: Option<&str>) -> ToolResult {
    let safe_level = level.clamp(0, 100);
    if let Err(error) = Adb::device_shell(&format!("dumpsys battery set level {safe_level}")).await
    {
        return response::error_response(format!("Failed to set battery level: {error}"));
    }
    if let Some(s) = status {
        let status_val = match s {
            "charging" => 2,
            "discharging" => 3,
            "not-charging" => 4,
            "full" => 5,
            _ => 1,
        };
        if let Err(error) =
            Adb::device_shell(&format!("dumpsys battery set status {status_val}")).await
        {
            return response::error_response(format!(
                "Battery level changed, but status update failed: {error}"
            ));
        }
    }
    response::text_response(format!("Battery set to {safe_level}%"))
}

pub async fn reset_battery() -> ToolResult {
    match Adb::device_shell("dumpsys battery reset").await {
        Ok(_) => response::text_response("Battery mocking reset"),
        Err(e) => response::error_response(format!("Failed to reset battery: {e}")),
    }
}
