use std::env;
use std::path::PathBuf;

/// Centralized configuration for the Android MCP server.
/// All paths are resolved at runtime via environment variables or discovery.
pub struct Config;

impl Config {
    /// Optional explicit ADB serial. Production and hardware-test setups should
    /// set this whenever more than one device may be attached.
    pub fn device_serial() -> Option<String> {
        env::var("DROIDSIGHT_DEVICE_SERIAL")
            .ok()
            .map(|serial| serial.trim().to_string())
            .filter(|serial| !serial.is_empty())
    }

    /// Host-side file tools are confined to this root. This prevents an MCP
    /// caller from turning Android push/pull/install into arbitrary host file
    /// read or overwrite primitives.
    pub fn local_file_root() -> std::io::Result<PathBuf> {
        let configured =
            env::var("DROIDSIGHT_LOCAL_ROOT").map_or(env::current_dir()?, PathBuf::from);
        configured.canonicalize()
    }

    pub fn allow_arbitrary_shell() -> bool {
        env::var("DROIDSIGHT_ALLOW_SHELL").as_deref() == Ok("1")
    }

    /// PIN used by opt-in automatic unlock middleware.
    ///
    /// Credentials must be supplied at runtime; they must never be compiled into
    /// the binary or committed to source control.
    pub fn device_pin() -> Option<String> {
        env::var("DROIDSIGHT_DEVICE_PIN")
            .ok()
            .filter(|pin| !pin.is_empty() && pin.chars().all(|c| c.is_ascii_digit()))
    }

    /// Get the configured debug log path, if any.
    ///
    /// Persistent file logging is strictly opt-in: it is enabled only by an
    /// explicit `DROIDSIGHT_DEBUG_LOG` path. The server never selects a log
    /// destination on its own, because device logs, hierarchy dumps, and OCR
    /// text can all reach the log file and the operator must choose where that
    /// material is written.
    pub fn debug_log_path() -> Option<PathBuf> {
        let path = env::var("DROIDSIGHT_DEBUG_LOG").ok()?;
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(PathBuf::from(trimmed))
    }
}

/// Resolve the ADB executable path using environment variables and common locations.
/// Returns "adb" as fallback if no specific path is found (relies on PATH).
pub fn resolve_adb_path() -> String {
    tracing::debug!("Resolving ADB path...");

    if let Ok(path) = env::var("DROIDSIGHT_ADB_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path.to_string_lossy().to_string();
        }
        tracing::warn!("DROIDSIGHT_ADB_PATH does not name a file");
    }

    let mut candidates = Vec::new();
    if let Ok(root) = env::var("ANDROID_SDK_ROOT") {
        candidates.push(
            PathBuf::from(root)
                .join("platform-tools")
                .join(adb_executable()),
        );
    }
    if let Ok(home) = env::var("ANDROID_HOME") {
        candidates.push(
            PathBuf::from(home)
                .join("platform-tools")
                .join(adb_executable()),
        );
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(local_app_data) = env::var("LOCALAPPDATA") {
            candidates.push(
                PathBuf::from(local_app_data)
                    .join("Android")
                    .join("Sdk")
                    .join("platform-tools")
                    .join("adb.exe"),
            );
        }
    }

    for path in candidates {
        tracing::debug!("Checking candidate: {:?}", path);
        if path.exists() {
            let path_str = path.to_string_lossy().to_string();
            tracing::info!("Found ADB at: {}", path_str);
            return path_str;
        }
    }

    tracing::debug!("Using 'adb' from PATH");
    "adb".to_string()
}

/// Get the platform-specific ADB executable name
fn adb_executable() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "adb.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "adb"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_adb_resolution_fallback() {
        // Clear env vars to test fallback to "adb"
        env::remove_var("ANDROID_HOME");
        env::remove_var("ANDROID_SDK_ROOT");

        // With no SDK env vars set, resolution should fall back to a plain
        // "adb" or an SDK path ending in adb/adb.exe.
        let path = resolve_adb_path();
        assert!(
            path == "adb" || path.ends_with("adb") || path.ends_with("adb.exe"),
            "Path '{path}' should end with adb"
        );
    }

    #[test]
    fn debug_log_is_opt_in_only() {
        // Persistent logging must never turn itself on because some unrelated
        // directory happens to exist on the host.
        env::remove_var("DROIDSIGHT_DEBUG_LOG");
        assert_eq!(Config::debug_log_path(), None);

        env::set_var("DROIDSIGHT_DEBUG_LOG", "   ");
        assert_eq!(
            Config::debug_log_path(),
            None,
            "a blank path must not enable logging"
        );

        env::set_var("DROIDSIGHT_DEBUG_LOG", "mcp_debug.log");
        assert_eq!(
            Config::debug_log_path(),
            Some(std::path::PathBuf::from("mcp_debug.log"))
        );

        env::remove_var("DROIDSIGHT_DEBUG_LOG");
    }
}
