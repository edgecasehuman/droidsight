use crate::adb::Adb;
use crate::response::{self, ToolResult};

pub async fn sqlite_query(path: &str, query: &str) -> ToolResult {
    // Wraps the on-device `sqlite3` binary, which is not present on every
    // Android build.
    let cmd = format!(
        "sqlite3 {} {}",
        crate::adb::shell_quote(path),
        crate::adb::shell_quote(query)
    );
    match Adb::device_shell(&cmd).await {
        Ok(out) => response::bounded_text_response(
            out,
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(format!("SQLite error: {e}")),
    }
}

pub async fn file_hash(path: &str, algo: &str) -> ToolResult {
    let bin = match algo {
        "sha256" => "sha256sum",
        _ => "md5sum",
    };
    match Adb::device_shell(&format!("{} {}", bin, crate::adb::shell_quote(path))).await {
        Ok(out) => response::text_response(out),
        Err(e) => response::error_response(format!("Hash error: {e}")),
    }
}

/// Run `pm clear`, which deletes **all** of an application's data.
///
/// This is not a cache eviction. It removes databases, shared preferences,
/// accounts, and credentials, and it cannot be undone. The tool is named for
/// what it does so a caller cannot reach it expecting a cheap cleanup, and the
/// tool layer gates it behind an explicit confirmation argument.
pub async fn clear_app_data(pkg: &str) -> ToolResult {
    if pkg.trim().is_empty() {
        return response::error_response("package_name is required".to_string());
    }
    match Adb::device_shell(&format!("pm clear {}", crate::adb::shell_quote(pkg))).await {
        Ok(out) => response::bounded_text_response(
            format!("Deleted all application data for {pkg}: {out}"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(format!("Failed to clear application data: {e}")),
    }
}
