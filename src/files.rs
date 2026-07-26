use crate::adb::Adb;
use crate::response::{self, ToolResult};

pub fn local_read_path(path: &str) -> Result<std::path::PathBuf, String> {
    let root = crate::config::Config::local_file_root()
        .map_err(|error| format!("Failed to resolve local file root: {error}"))?;
    let resolved = std::path::Path::new(path)
        .canonicalize()
        .map_err(|error| format!("Failed to resolve local source: {error}"))?;
    if !resolved.starts_with(&root) || !resolved.is_file() {
        return Err(format!(
            "Local source must be a file beneath {}",
            root.display()
        ));
    }
    Ok(resolved)
}

pub fn local_write_path(path: &str) -> Result<std::path::PathBuf, String> {
    let root = crate::config::Config::local_file_root()
        .map_err(|error| format!("Failed to resolve local file root: {error}"))?;
    let requested = std::path::Path::new(path);
    let resolved = if requested.exists() {
        requested
            .canonicalize()
            .map_err(|error| format!("Failed to resolve local destination: {error}"))?
    } else {
        let parent = requested
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let parent = parent
            .canonicalize()
            .map_err(|error| format!("Failed to resolve local destination parent: {error}"))?;
        let name = requested
            .file_name()
            .ok_or_else(|| "Local destination requires a file name".to_string())?;
        parent.join(name)
    };
    if !resolved.starts_with(&root) {
        return Err(format!(
            "Local destination must be beneath {}",
            root.display()
        ));
    }
    Ok(resolved)
}
pub async fn list_directory(path: &str) -> ToolResult {
    match Adb::shell(&["shell", "ls", "-la", path]).await {
        Ok(output) => response::bounded_text_response(
            output,
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn read_file(path: &str) -> ToolResult {
    match Adb::shell(&["shell", "cat", path]).await {
        Ok(output) => response::bounded_text_response(
            output,
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

/// Push a local file through the same explicitly-selected ADB transport used by
/// every other tool.
pub async fn push_file(local_path: &str, remote_path: &str) -> ToolResult {
    let local_path = match local_read_path(local_path) {
        Ok(path) => path,
        Err(error) => return response::error_response(error),
    };
    let local_path = local_path.to_string_lossy();
    match Adb::shell(&["push", &local_path, remote_path]).await {
        Ok(output) => response::bounded_text_response(
            format!("Pushed {local_path} to {remote_path}: {output}"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(error) => response::error_response(format!("Push failed: {error}")),
    }
}

/// Pull a remote file through the explicitly-selected ADB transport.
pub async fn pull_file(remote_path: &str, local_path: &str) -> ToolResult {
    let local_path = match local_write_path(local_path) {
        Ok(path) => path,
        Err(error) => return response::error_response(error),
    };
    let local_path = local_path.to_string_lossy();
    match Adb::shell(&["pull", remote_path, &local_path]).await {
        Ok(output) => response::bounded_text_response(
            format!("Pulled {remote_path} to {local_path}: {output}"),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(error) => response::error_response(format!("Pull failed: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{local_read_path, local_write_path};

    #[test]
    fn host_file_paths_are_confined_to_the_process_root() {
        assert!(local_read_path("Cargo.toml").is_ok());
        assert!(local_read_path("../README.md").is_err());
        assert!(local_write_path("local-pull-test.bin").is_ok());
        assert!(local_write_path("../escaped-pull.bin").is_err());
    }
}
