use crate::adb::Adb;
use crate::response::{self, ToolResult};
use anyhow::Result;
use serde_json::json;

pub async fn get_device_info() -> ToolResult {
    let model = match Adb::shell(&["shell", "getprop", "ro.product.model"]).await {
        Ok(value) => value,
        Err(error) => {
            return response::error_response(format!("Failed to read device model: {error}"))
        }
    };
    let serial = match Adb::shell(&["get-serialno"]).await {
        Ok(value) => value,
        Err(error) => {
            return response::error_response(format!("Failed to read device serial: {error}"))
        }
    };
    let android_version = match Adb::shell(&["shell", "getprop", "ro.build.version.release"]).await
    {
        Ok(value) => value,
        Err(error) => {
            return response::error_response(format!("Failed to read Android version: {error}"))
        }
    };

    // We construct a nested JSON object as the text content
    let locked = is_locked().await;
    let info = json!({
        "model": model,
        "serial": serial,
        "android_version": android_version,
        "is_locked": locked
    });
    response::bounded_text_response(
        info.to_string(),
        response::DEFAULT_TEXT_BUDGET_BYTES,
        response::TruncationStrategy::Head,
    )
}

pub async fn get_battery_status() -> ToolResult {
    match Adb::shell(&["shell", "dumpsys", "battery"]).await {
        Ok(output) => response::bounded_text_response(
            output,
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn run_shell(command: &str) -> ToolResult {
    if command.trim().is_empty() {
        return response::error_response("command must not be empty");
    }
    match Adb::shell(&["shell", command]).await {
        Ok(output) => response::bounded_text_response(
            output,
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn is_screen_on() -> bool {
    match Adb::shell(&["shell", "dumpsys", "power"]).await {
        Ok(output) => output.contains("mWakefulness=Awake"),
        Err(_) => false,
    }
}

pub async fn ensure_ready() -> Result<()> {
    // Robust check using Window Policy
    let locked = is_locked().await;

    // Also check power state as a backup (screen off = locked implies)
    let pwr = Adb::shell(&["shell", "dumpsys", "power"])
        .await
        .unwrap_or_default();
    let screen_off = !pwr.contains("mWakefulness=Awake");

    if locked || screen_off {
        let pin = crate::config::Config::device_pin();
        unlock_device(pin).await.map_err(|error| {
            anyhow::anyhow!(
                "Device could not be prepared: {}",
                error
                    .get("message")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unlock failed")
            )
        })?;

        // Post-unlock stability delay: Allow UI to fully render before tool executes.
        // This prevents race conditions where screenshot/hierarchy capture stale content.
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
    }
    Ok(())
}

pub async fn is_locked() -> bool {
    // Fetch policy and parse in Rust to avoid grep exit-code ambiguity. Android
    // vendors expose both legacy one-line fields and the newer indented
    // KeyguardServiceDelegate / KeyguardStateMonitor state.
    match Adb::shell(&["shell", "dumpsys window policy"]).await {
        Ok(output) => policy_reports_locked(&output),
        Err(e) => {
            tracing::warn!("Error checking lock state: {}", e);
            // UI actions must fail closed when the keyguard state cannot be
            // established. Assuming unlocked here can inject input into an
            // unknown screen or falsely report a successful unlock.
            true
        }
    }
}

fn policy_reports_locked(output: &str) -> bool {
    output.lines().any(|line| {
        matches!(
            line.trim(),
            "mKeyguardShowing=true"
                | "mDreamingLockscreen=true"
                | "showing=true"
                | "mIsShowing=true"
        )
    })
}

pub async fn unlock_device(pin: Option<String>) -> ToolResult {
    // Method A: parallel wake and keyguard dismiss in a single shell call.
    let _ = Adb::shell(&["shell", "input keyevent 224 & wm dismiss-keyguard"]).await;

    // Quick verification that the screen turned on.
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    if !is_screen_on().await {
        // Fallback B: sequential wake then dismiss, for devices where the
        // parallel path does not reliably turn the screen on.
        // 1. Wake (Power key)
        let _ = Adb::shell(&["shell", "input", "keyevent", "26"]).await;
        // 2. Wait
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        // 3. Wake (ensure on)
        let _ = Adb::shell(&["shell", "input", "keyevent", "224"]).await;
        // 4. Dismiss
        let _ = Adb::shell(&["shell", "wm", "dismiss-keyguard"]).await;
    }

    if let Some(p) = pin {
        // The screen-on check above already waited 100ms after wake; add a
        // small buffer so the input system is ready before sending keycodes.
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Map PIN chars to keycodes
        let mut keycodes = Vec::new();
        for c in p.chars() {
            let k = match c {
                '0' => "7",
                '1' => "8",
                '2' => "9",
                '3' => "10",
                '4' => "11",
                '5' => "12",
                '6' => "13",
                '7' => "14",
                '8' => "15",
                '9' => "16",
                _ => continue,
            };
            keycodes.push(k);
        }

        if !keycodes.is_empty() {
            keycodes.push("66"); // ENTER

            // Fast path: inject all keycodes in one `cmd input` call.
            let cmd = format!("cmd input keyevent {}", keycodes.join(" "));
            match Adb::shell(&["shell", &cmd]).await {
                Ok(_) => {}
                Err(_) => {
                    // Fallback: send them one at a time via `input keyevent`.
                    for k in keycodes {
                        let _ = Adb::shell(&["shell", "input", "keyevent", k]).await;
                    }
                }
            }
        }
    }

    // Poll until the device reports the new state, or the deadline passes.
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(4);

    // Poll fast initially
    let mut attempt = 0;
    while start.elapsed() < timeout {
        attempt += 1;
        let locked = is_locked().await;

        if !locked {
            return response::text_response(format!(
                "Device Unlocked (Verified in attempt {attempt})"
            ));
        }

        // Exponential backoff or simple sleep
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        // If we are halfway through and still locked, try dismissing again
        if attempt == 5 {
            let _ = Adb::shell(&["shell", "wm", "dismiss-keyguard"]).await;
        }
    }

    // Final check
    if is_locked().await {
        return response::error_response(format!(
            "Unlock Timed Out (Attempts: {attempt}). Device remains locked."
        ));
    }

    response::text_response("Device Unlocked (Verified Late)")
}

pub async fn set_accessibility(service: &str, enabled: bool) -> ToolResult {
    if service.is_empty() || !service.contains('/') {
        return response::error_response(
            "service must be an Android component in package/class form",
        );
    }
    let current = match Adb::shell(&[
        "shell",
        "settings",
        "get",
        "secure",
        "enabled_accessibility_services",
    ])
    .await
    {
        Ok(value) => value.trim().to_string(),
        Err(error) => {
            return response::error_response(format!(
                "Failed to read accessibility settings: {error}"
            ))
        }
    };

    let mut services: Vec<&str> = if current == "null" || current.is_empty() {
        Vec::new()
    } else {
        current.split(':').collect()
    };

    let needs_update = if enabled {
        if !services.contains(&service) {
            services.push(service);
            true
        } else {
            false
        }
    } else {
        if let Some(pos) = services.iter().position(|&x| x == service) {
            services.remove(pos);
            true
        } else {
            false
        }
    };

    if needs_update {
        let new_val = services.join(":");
        // Preferred path: write the service list through `settings put`.
        if let Err(error) = Adb::shell(&[
            "shell",
            "settings",
            "put",
            "secure",
            "enabled_accessibility_services",
            &new_val,
        ])
        .await
        {
            return response::error_response(format!(
                "Failed to update accessibility services: {error}"
            ));
        }
        if enabled {
            if let Err(error) = Adb::shell(&[
                "shell",
                "settings",
                "put",
                "secure",
                "accessibility_enabled",
                "1",
            ])
            .await
            {
                return response::error_response(format!(
                    "Service list changed, but accessibility enable failed: {error}"
                ));
            }
        }

        // Confirm the write landed; fall back to the alternate path if not.
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        let verify = match Adb::shell(&[
            "shell",
            "settings",
            "get",
            "secure",
            "enabled_accessibility_services",
        ])
        .await
        {
            Ok(value) => value,
            Err(error) => {
                return response::error_response(format!(
                    "Accessibility changed, but verification failed: {error}"
                ))
            }
        };

        if enabled && !verify.contains(service) {
            // Fallback: Force stop the app to clear cached settings state, then retry
            if let Some(pkg) = service.split('/').next() {
                if let Err(error) = Adb::shell(&["shell", "am", "force-stop", pkg]).await {
                    return response::error_response(format!(
                        "Accessibility verification failed; fallback force-stop also failed: {error}"
                    ));
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                if let Err(error) = Adb::shell(&[
                    "shell",
                    "settings",
                    "put",
                    "secure",
                    "enabled_accessibility_services",
                    &new_val,
                ])
                .await
                {
                    return response::error_response(format!(
                        "Accessibility fallback update failed: {error}"
                    ));
                }
            }
        }

        let final_value = match Adb::shell(&[
            "shell",
            "settings",
            "get",
            "secure",
            "enabled_accessibility_services",
        ])
        .await
        {
            Ok(value) => value,
            Err(error) => {
                return response::error_response(format!(
                    "Accessibility final verification failed: {error}"
                ))
            }
        };
        let present = final_value.split(':').any(|value| value.trim() == service);
        if present != enabled {
            return response::error_response(
                "Accessibility setting did not reach the requested state",
            );
        }
    }

    response::bounded_text_response(
        format!(
            "Accessibility {} for {service}",
            if enabled { "enabled" } else { "disabled" }
        ),
        response::DEFAULT_TEXT_BUDGET_BYTES,
        response::TruncationStrategy::Head,
    )
}

pub async fn set_overlay(package: &str, allowed: bool) -> ToolResult {
    let mode = if allowed { "allow" } else { "deny" };
    match Adb::shell(&[
        "shell",
        "appops",
        "set",
        package,
        "SYSTEM_ALERT_WINDOW",
        mode,
    ])
    .await
    {
        Ok(_) => response::bounded_text_response(
            format!("Set overlay for {package} to {mode}"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn set_orientation(mode: &str) -> ToolResult {
    // 0 = Portrait, 1 = Landscape
    let user_rotation = if mode == "landscape" { "1" } else { "0" };

    // Disable auto-rotate first
    if let Err(error) = Adb::shell(&[
        "shell",
        "content",
        "insert",
        "--uri",
        "content://settings/system",
        "--bind",
        "name:s:accelerometer_rotation",
        "--bind",
        "value:i:0",
    ])
    .await
    {
        return response::error_response(format!("Failed to disable automatic rotation: {error}"));
    }

    // Set rotation
    match Adb::shell(&[
        "shell",
        "content",
        "insert",
        "--uri",
        "content://settings/system",
        "--bind",
        "name:s:user_rotation",
        "--bind",
        &format!("value:i:{user_rotation}"),
    ])
    .await
    {
        Ok(_) => response::text_response(format!("Set orientation to {mode}")),
        Err(e) => response::error_response(e.to_string()),
    }
}

#[cfg(test)]
mod lock_state_tests {
    use super::policy_reports_locked;

    #[test]
    fn recognizes_legacy_aosp_keyguard_fields() {
        assert!(policy_reports_locked("mKeyguardShowing=true"));
        assert!(policy_reports_locked("mDreamingLockscreen=true"));
    }

    #[test]
    fn recognizes_modern_samsung_keyguard_fields() {
        let policy = r"
            KeyguardServiceDelegate
              showing=true
              showingAndNotOccluded=true
              secure=true
              dreaming=true
              KeyguardStateMonitor
                mIsShowing=true
        ";
        assert!(policy_reports_locked(policy));
    }

    #[test]
    fn ignores_unlocked_and_unrelated_showing_fields() {
        let policy = r"
            KeyguardServiceDelegate
              showing=false
              showingAndNotOccluded=true
              secure=true
              dreaming=false
              KeyguardStateMonitor
                mIsShowing=false
            unrelatedShowing=true
        ";
        assert!(!policy_reports_locked(policy));
    }
}
