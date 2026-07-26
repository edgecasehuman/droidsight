use crate::adb::Adb;
use crate::response::{self, ToolResult};
use serde_json::json;

/// Reports which developer-oriented settings are currently enabled on the
/// device: ADB, Developer Options, and USB debugging. Each one widens the
/// device's attack surface and is readable by any installed app, so the report
/// describes how exposed the device is, not how well it is hidden.
pub async fn check_debug_exposure() -> ToolResult {
    // Query ADB enabled status
    let adb_enabled = Adb::shell(&["shell", "settings", "get", "global", "adb_enabled"])
        .await
        .unwrap_or_default()
        .trim()
        == "1";

    // Query Developer Options status
    let dev_options = Adb::shell(&[
        "shell",
        "settings",
        "get",
        "global",
        "development_settings_enabled",
    ])
    .await
    .unwrap_or_default()
    .trim()
        == "1";

    // Query USB debugging status (more specific than adb_enabled)
    let usb_debug = Adb::shell(&["shell", "getprop", "sys.usb.config"])
        .await
        .unwrap_or_default()
        .contains("adb");

    // Calculate risk level
    let risk_level = match (adb_enabled || usb_debug, dev_options) {
        (true, true) => "HIGH",
        (true, false) | (false, true) => "MEDIUM",
        (false, false) => "LOW",
    };

    // Build recommendations based on findings
    let mut recommendations = Vec::new();
    if dev_options {
        recommendations.push("Developer Options are enabled");
    }
    if adb_enabled {
        recommendations
            .push("ADB is enabled - this is detectable by apps querying Settings.Global");
    }
    if usb_debug {
        recommendations.push("USB config contains 'adb' - visible via sys.usb.config property");
    }
    if recommendations.is_empty() {
        recommendations.push("No debugging-related settings detected");
    }

    response::text_response(
        json!({
            "adb_enabled": adb_enabled,
            "developer_options": dev_options,
            "usb_config_has_adb": usb_debug,
            "risk_level": risk_level,
            "recommendations": recommendations
        })
        .to_string(),
    )
}
