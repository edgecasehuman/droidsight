use crate::config;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::str;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Mutex;

static ADB_RUNTIME: OnceLock<AdbRuntime> = OnceLock::new();
const ADB_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const ADB_CAPTURE_LIMIT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AdbCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) timeout: Duration,
}

impl AdbCommand {
    pub(crate) fn new(program: String, args: Vec<String>) -> Self {
        Self {
            program,
            args,
            timeout: ADB_COMMAND_TIMEOUT,
        }
    }
}

#[derive(Debug)]
pub(crate) struct AdbCommandOutput {
    pub(crate) success: bool,
    pub(crate) status: String,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) stdout_total_bytes: usize,
    pub(crate) stderr_total_bytes: usize,
    pub(crate) stdout_truncated: bool,
    pub(crate) stderr_truncated: bool,
}

impl AdbCommandOutput {
    fn trace_truncation(&self) {
        if self.stdout_truncated {
            tracing::warn!(
                retained_bytes = self.stdout.len(),
                total_bytes = self.stdout_total_bytes,
                "ADB stdout capture was truncated"
            );
        }
        if self.stderr_truncated {
            tracing::warn!(
                retained_bytes = self.stderr.len(),
                total_bytes = self.stderr_total_bytes,
                "ADB stderr capture was truncated"
            );
        }
    }

    fn truncation_diagnostics(&self) -> String {
        let mut details = Vec::new();
        if self.stdout_truncated {
            details.push(format!(
                "stdout retained {}/{} bytes",
                self.stdout.len(),
                self.stdout_total_bytes
            ));
        }
        if self.stderr_truncated {
            details.push(format!(
                "stderr retained {}/{} bytes",
                self.stderr.len(),
                self.stderr_total_bytes
            ));
        }
        if details.is_empty() {
            String::new()
        } else {
            format!(" | capture truncated: {}", details.join(", "))
        }
    }
}

#[derive(Debug)]
struct CappedCapture {
    bytes: Vec<u8>,
    total_bytes: usize,
}

impl CappedCapture {
    fn truncated(&self) -> bool {
        self.total_bytes > self.bytes.len()
    }
}

async fn collect_capped<R>(mut reader: R, limit: usize) -> std::io::Result<CappedCapture>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut total_bytes = 0usize;
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(read);
        let retained = limit.saturating_sub(bytes.len()).min(read);
        bytes.extend_from_slice(&chunk[..retained]);
    }
    Ok(CappedCapture { bytes, total_bytes })
}

/// Boundary between ADB orchestration and the host subprocess implementation.
///
/// Keeping the command request owned makes this interface straightforward to
/// fake in unit tests and leaves room for a future long-lived or remote ADB
/// implementation without changing command construction and device selection.
#[async_trait]
pub(crate) trait AdbBackend: Send + Sync {
    async fn execute(&self, command: AdbCommand) -> Result<AdbCommandOutput>;
}

pub(crate) struct TokioAdbBackend;

#[async_trait]
impl AdbBackend for TokioAdbBackend {
    async fn execute(&self, request: AdbCommand) -> Result<AdbCommandOutput> {
        let mut command = Command::new(&request.program);
        command
            .args(&request.args)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("ADB command stdout pipe was not available"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("ADB command stderr pipe was not available"))?;
        let mut stdout_task = tokio::spawn(collect_capped(stdout, ADB_CAPTURE_LIMIT_BYTES));
        let mut stderr_task = tokio::spawn(collect_capped(stderr, ADB_CAPTURE_LIMIT_BYTES));

        let completed = tokio::time::timeout(request.timeout, async {
            let status = child.wait().await?;
            let stdout = (&mut stdout_task).await??;
            let stderr = (&mut stderr_task).await??;
            Ok::<_, anyhow::Error>((status, stdout, stderr))
        })
        .await;

        let (status, stdout, stderr) = match completed {
            Ok(Ok(completed)) => completed,
            Ok(Err(error)) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                stdout_task.abort();
                stderr_task.abort();
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(error);
            }
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                stdout_task.abort();
                stderr_task.abort();
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(anyhow!(
                    "ADB command timed out after {}s",
                    request.timeout.as_secs()
                ));
            }
        };

        let output = AdbCommandOutput {
            success: status.success(),
            status: status.to_string(),
            stdout_truncated: stdout.truncated(),
            stderr_truncated: stderr.truncated(),
            stdout_total_bytes: stdout.total_bytes,
            stderr_total_bytes: stderr.total_bytes,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
        };
        output.trace_truncation();
        Ok(output)
    }
}

/// Owns the subprocess implementation and device-selection state for one ADB
/// session. The application uses one process-wide runtime through [`Adb`],
/// while tests can construct isolated runtimes without mutating global state.
pub(crate) struct AdbRuntime {
    backend: Arc<dyn AdbBackend>,
    adb_path: String,
    configured_serial: Option<String>,
    selected_serial: Mutex<Option<String>>,
}

impl AdbRuntime {
    fn production() -> Self {
        Self::new(
            Arc::new(TokioAdbBackend),
            config::resolve_adb_path(),
            config::Config::device_serial(),
        )
    }

    pub(crate) fn new(
        backend: Arc<dyn AdbBackend>,
        adb_path: String,
        configured_serial: Option<String>,
    ) -> Self {
        Self {
            backend,
            adb_path,
            configured_serial,
            selected_serial: Mutex::new(None),
        }
    }

    async fn fetch_serial(&self) -> Result<String> {
        if let Some(serial) = &self.configured_serial {
            return Ok(serial.clone());
        }
        let output = self
            .backend
            .execute(AdbCommand::new(
                self.adb_path.clone(),
                vec!["devices".to_string()],
            ))
            .await?;
        if !output.success {
            return Err(Adb::command_failure(&output));
        }
        select_serial(&output.stdout)
    }

    async fn serial(&self) -> Result<String> {
        // Holding this mutex across discovery intentionally coalesces concurrent
        // first use into one `adb devices` request.
        let mut selected = self.selected_serial.lock().await;
        if let Some(serial) = selected.as_ref() {
            return Ok(serial.clone());
        }
        let serial = self.fetch_serial().await?;
        // The serial identifies the operator's hardware, so it is only emitted
        // at debug level, where file logging is opt-in.
        tracing::debug!("Cached ADB device serial: {}", serial);
        *selected = Some(serial.clone());
        Ok(serial)
    }

    async fn invalidate_serial(&self, failed_serial: &str) {
        if self.configured_serial.is_some() {
            return;
        }
        let mut selected = self.selected_serial.lock().await;
        if selected.as_deref() == Some(failed_serial) {
            *selected = None;
        }
    }

    async fn shell(&self, args: &[&str]) -> Result<String> {
        let output = self.shell_output(args).await?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    async fn shell_output(&self, args: &[&str]) -> Result<AdbCommandOutput> {
        let serial = self.serial().await?;
        let normalized_args = normalized_adb_args(args);
        let mut final_args = vec!["-s".to_string(), serial.clone()];
        final_args.extend(normalized_args);
        Adb::log_cmd(&final_args.iter().map(String::as_str).collect::<Vec<_>>());

        let result = self
            .backend
            .execute(AdbCommand::new(self.adb_path.clone(), final_args))
            .await;
        if result.as_ref().is_err() || result.as_ref().is_ok_and(|output| !output.success) {
            // A transport failure commonly means that a wireless device was
            // replaced or reconnected under another serial. The next command
            // must rediscover instead of pinning stale state forever.
            self.invalidate_serial(&serial).await;
        }
        let output = result?;
        if !output.success {
            return Err(Adb::command_failure(&output));
        }
        Ok(output)
    }

    async fn execute_host(&self, args: Vec<String>, timeout: Duration) -> Result<AdbCommandOutput> {
        self.backend
            .execute(AdbCommand {
                program: self.adb_path.clone(),
                args,
                timeout,
            })
            .await
    }
}

/// Quote one value for Android's POSIX-compatible remote shell.
///
/// `adb shell` ultimately evaluates a command string on the device. Values
/// interpolated into that string must not be able to terminate their argument.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn normalized_adb_args(args: &[&str]) -> Vec<String> {
    if args.first() == Some(&"shell") && args.len() > 2 {
        vec![
            "shell".to_string(),
            args[1..]
                .iter()
                .map(|arg| shell_quote(arg))
                .collect::<Vec<_>>()
                .join(" "),
        ]
    } else {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }
}

fn select_serial(devices_output: &[u8]) -> Result<String> {
    let out = str::from_utf8(devices_output)?;
    let mut devices = Vec::new();
    for line in out.lines().skip(1) {
        if let Some((serial, status)) = line.split_once('\t') {
            if status.split_whitespace().next() == Some("device") {
                devices.push(serial.to_string());
            }
        } else if let Some((serial, status)) = line.split_once(' ') {
            if status.split_whitespace().next() == Some("device") {
                devices.push(serial.to_string());
            }
        }
    }
    match devices.as_slice() {
        [] => Err(anyhow!("No authorized ADB device found")),
        [serial] => Ok(serial.clone()),
        _ => Err(anyhow!(
            "Multiple ADB devices are connected; set DROIDSIGHT_DEVICE_SERIAL explicitly"
        )),
    }
}

pub struct Adb;

impl Adb {
    fn runtime() -> &'static AdbRuntime {
        ADB_RUNTIME.get_or_init(AdbRuntime::production)
    }

    pub fn get_adb_path() -> String {
        Self::runtime().adb_path.clone()
    }

    fn log_cmd(args: &[&str]) {
        // Remote-shell arguments routinely contain credentials and user data.
        tracing::debug!("[ADB CMD] executing adb with {} argument(s)", args.len());
    }

    #[cfg(test)]
    async fn fetch_serial_with_backend(
        backend: &dyn AdbBackend,
        adb_path: String,
        configured_serial: Option<String>,
    ) -> Result<String> {
        if let Some(serial) = configured_serial {
            return Ok(serial);
        }

        let output = backend
            .execute(AdbCommand::new(adb_path, vec!["devices".to_string()]))
            .await?;
        if !output.success {
            return Err(Self::command_failure(&output));
        }
        select_serial(&output.stdout)
    }

    /// Executes an ADB command and returns stdout as a string.
    pub async fn shell(args: &[&str]) -> Result<String> {
        Self::runtime().shell(args).await
    }

    /// Executes an ADB command while retaining separate stdout and stderr.
    /// This is reserved for protocols whose commands report semantic failures
    /// on stderr despite exiting successfully (notably Android Activity Manager).
    pub(crate) async fn shell_output(args: &[&str]) -> Result<AdbCommandOutput> {
        Self::runtime().shell_output(args).await
    }

    #[cfg(test)]
    pub(crate) async fn shell_with_backend(
        backend: &dyn AdbBackend,
        adb_path: String,
        serial: String,
        args: &[&str],
    ) -> Result<String> {
        let normalized_args = normalized_adb_args(args);
        let mut final_args = vec!["-s".to_string(), serial];
        final_args.extend(normalized_args);

        Self::log_cmd(&final_args.iter().map(String::as_str).collect::<Vec<_>>());

        let output = backend
            .execute(AdbCommand::new(adb_path, final_args))
            .await?;

        if !output.success {
            return Err(Self::command_failure(&output));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn command_failure(output: &AdbCommandOutput) -> anyhow::Error {
        let err = String::from_utf8_lossy(&output.stderr);
        let out = String::from_utf8_lossy(&output.stdout);
        anyhow!(
            "ADB Error (Exit {}): {} | stdout: {}{}",
            output.status,
            err,
            out,
            output.truncation_diagnostics()
        )
    }

    /// Executes an ADB shell command on the connected device.
    pub async fn device_shell(cmd: &str) -> Result<String> {
        Self::shell(&["shell", cmd]).await
    }

    /// Execute a remote shell command through the canonical subprocess path.
    ///
    /// Despite the name, this does not use the raw `shell:` ADB service. That
    /// service does not expose the remote exit status, so a failed command
    /// would be reported as a successful tool call.
    pub async fn shell_native(cmd: &str) -> Result<String> {
        Self::shell(&["shell", cmd]).await
    }

    /// Execute a remote command over the raw ADB exec service and return its
    /// unmodified bytes. This is the path for binary payloads such as
    /// `screencap`, which must not pass through text handling.
    pub async fn exec_out_native(cmd: &str) -> Result<Vec<u8>> {
        let runtime = Self::runtime();
        let serial = runtime.serial().await?;
        let result = crate::adb_protocol::AdbClient::exec(cmd, Some(&serial)).await;
        if result.is_err() {
            runtime.invalidate_serial(&serial).await;
        }
        result
    }

    /// Resolve the selected serial for native streaming code.
    pub async fn selected_serial() -> Result<String> {
        Self::runtime().serial().await
    }

    /// Execute an unscoped host-side ADB operation through the same bounded
    /// backend used by normal commands (for example `start-server` and `pair`).
    pub(crate) async fn execute_host(
        args: Vec<String>,
        timeout: Duration,
    ) -> Result<AdbCommandOutput> {
        Self::runtime().execute_host(args, timeout).await
    }

    /// Construct the sole deliberately long-running ADB process. Event
    /// monitoring owns its child, reader thread, and shutdown lifecycle.
    pub(crate) fn event_monitor_command(serial: &str) -> std::process::Command {
        let mut command = std::process::Command::new(Self::get_adb_path());
        command.args(["-s", serial, "logcat", "-v", "threadtime"]);
        command
    }
}

#[cfg(test)]
mod tests {
    use super::{
        normalized_adb_args, select_serial, shell_quote, Adb, AdbBackend, AdbCommand,
        AdbCommandOutput, AdbRuntime,
    };
    // These back the two tests below that drive a real child process through a
    // POSIX shell. Importing them unconditionally makes strict Clippy fail on
    // targets where those tests are not compiled.
    #[cfg(unix)]
    use super::{TokioAdbBackend, ADB_CAPTURE_LIMIT_BYTES};
    use anyhow::{anyhow, Result};
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    #[cfg(unix)]
    use std::time::Instant;

    enum MockResponse {
        Output {
            success: bool,
            status: &'static str,
            stdout: &'static [u8],
            stderr: &'static [u8],
        },
        Error(&'static str),
    }

    struct MockBackend {
        requests: Mutex<Vec<AdbCommand>>,
        response: MockResponse,
    }

    impl MockBackend {
        fn output(stdout: &'static [u8]) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                response: MockResponse::Output {
                    success: true,
                    status: "exit status: 0",
                    stdout,
                    stderr: b"",
                },
            }
        }
    }

    #[async_trait]
    impl AdbBackend for MockBackend {
        async fn execute(&self, command: AdbCommand) -> Result<AdbCommandOutput> {
            self.requests.lock().unwrap().push(command);
            match self.response {
                MockResponse::Output {
                    success,
                    status,
                    stdout,
                    stderr,
                } => Ok(AdbCommandOutput {
                    success,
                    status: status.to_string(),
                    stdout: stdout.to_vec(),
                    stderr: stderr.to_vec(),
                    stdout_total_bytes: stdout.len(),
                    stderr_total_bytes: stderr.len(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                }),
                MockResponse::Error(message) => Err(anyhow!(message)),
            }
        }
    }

    struct SequenceBackend {
        requests: Mutex<Vec<AdbCommand>>,
        responses: Mutex<VecDeque<MockResponse>>,
    }

    impl SequenceBackend {
        fn new(responses: impl IntoIterator<Item = MockResponse>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into_iter().collect()),
            }
        }
    }

    #[async_trait]
    impl AdbBackend for SequenceBackend {
        async fn execute(&self, command: AdbCommand) -> Result<AdbCommandOutput> {
            self.requests.lock().unwrap().push(command);
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected backend request");
            match response {
                MockResponse::Output {
                    success,
                    status,
                    stdout,
                    stderr,
                } => Ok(AdbCommandOutput {
                    success,
                    status: status.to_string(),
                    stdout: stdout.to_vec(),
                    stderr: stderr.to_vec(),
                    stdout_total_bytes: stdout.len(),
                    stderr_total_bytes: stderr.len(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                }),
                MockResponse::Error(message) => Err(anyhow!(message)),
            }
        }
    }

    fn success(stdout: &'static [u8]) -> MockResponse {
        MockResponse::Output {
            success: true,
            status: "exit status: 0",
            stdout,
            stderr: b"",
        }
    }

    fn failure(stderr: &'static [u8]) -> MockResponse {
        MockResponse::Output {
            success: false,
            status: "exit status: 1",
            stdout: b"",
            stderr,
        }
    }

    #[test]
    fn shell_quote_contains_metacharacters_in_one_argument() {
        assert_eq!(shell_quote("plain value"), "'plain value'");
        assert_eq!(
            shell_quote("a'; rm -rf /; echo 'b"),
            "'a'\\''; rm -rf /; echo '\\''b'"
        );
    }

    #[test]
    fn structured_remote_shell_arguments_are_quoted_once() {
        assert_eq!(
            normalized_adb_args(&["shell", "pm", "clear", "pkg; reboot"]),
            vec!["shell", "'pm' 'clear' 'pkg; reboot'"]
        );
        assert_eq!(
            normalized_adb_args(&["shell", "echo $HOME | wc -c"]),
            vec!["shell", "echo $HOME | wc -c"]
        );
        assert_eq!(
            normalized_adb_args(&["install", "-r", "some app.apk"]),
            vec!["install", "-r", "some app.apk"]
        );
    }

    #[test]
    fn device_selection_ignores_unauthorized_and_offline_entries() {
        assert_eq!(
            select_serial(
                b"List of devices attached\nready\tdevice\nlocked\tunauthorized\nold offline\n"
            )
            .unwrap(),
            "ready"
        );
        assert_eq!(
            select_serial(b"List of devices attached\nlocked\tunauthorized\n")
                .unwrap_err()
                .to_string(),
            "No authorized ADB device found"
        );
    }

    #[test]
    fn device_selection_refuses_ambiguous_authorized_devices() {
        let error = select_serial(
            b"List of devices attached\nfirst\tdevice product:x\nsecond\tdevice product:y\n",
        )
        .unwrap_err();
        assert!(error.to_string().contains("Multiple ADB devices"));
    }

    #[tokio::test]
    async fn backend_receives_complete_normalized_command() {
        let backend = MockBackend::output(b" done \n");
        let output = Adb::shell_with_backend(
            &backend,
            "test-adb".to_string(),
            "serial-1".to_string(),
            &["shell", "pm", "clear", "pkg; reboot"],
        )
        .await
        .unwrap();

        assert_eq!(output, "done");
        assert_eq!(
            *backend.requests.lock().unwrap(),
            vec![AdbCommand {
                program: "test-adb".to_string(),
                args: vec![
                    "-s".to_string(),
                    "serial-1".to_string(),
                    "shell".to_string(),
                    "'pm' 'clear' 'pkg; reboot'".to_string(),
                ],
                timeout: Duration::from_secs(15),
            }]
        );
    }

    #[tokio::test]
    async fn backend_and_nonzero_exit_errors_are_propagated() {
        let backend = MockBackend {
            requests: Mutex::new(Vec::new()),
            response: MockResponse::Error("could not spawn adb"),
        };
        let error = Adb::shell_with_backend(
            &backend,
            "missing-adb".to_string(),
            "serial-1".to_string(),
            &["get-state"],
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "could not spawn adb");

        let backend = MockBackend {
            requests: Mutex::new(Vec::new()),
            response: MockResponse::Output {
                success: false,
                status: "exit status: 17",
                stdout: b"partial output",
                stderr: b"device disappeared",
            },
        };
        let error = Adb::shell_with_backend(
            &backend,
            "test-adb".to_string(),
            "serial-1".to_string(),
            &["get-state"],
        )
        .await
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("exit status: 17"));
        assert!(message.contains("device disappeared"));
        assert!(message.contains("partial output"));
    }

    #[test]
    fn command_failure_reports_capture_truncation() {
        let error = Adb::command_failure(&AdbCommandOutput {
            success: false,
            status: "exit status: 9".to_string(),
            stdout: b"partial".to_vec(),
            stderr: b"warning".to_vec(),
            stdout_total_bytes: 99,
            stderr_total_bytes: 101,
            stdout_truncated: true,
            stderr_truncated: true,
        });
        let message = error.to_string();
        assert!(message.contains("stdout retained 7/99 bytes"));
        assert!(message.contains("stderr retained 7/101 bytes"));
    }

    #[tokio::test]
    async fn configured_serial_bypasses_device_discovery() {
        let backend = MockBackend::output(b"this must not be parsed");
        let serial = Adb::fetch_serial_with_backend(
            &backend,
            "test-adb".to_string(),
            Some("explicit-serial".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(serial, "explicit-serial");
        assert!(backend.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn device_discovery_uses_unscoped_adb_devices_command() {
        let backend = MockBackend::output(b"List of devices attached\nserial-2\tdevice\n");
        let serial = Adb::fetch_serial_with_backend(&backend, "test-adb".to_string(), None)
            .await
            .unwrap();

        assert_eq!(serial, "serial-2");
        assert_eq!(
            *backend.requests.lock().unwrap(),
            vec![AdbCommand {
                program: "test-adb".to_string(),
                args: vec!["devices".to_string()],
                timeout: Duration::from_secs(15),
            }]
        );
    }

    #[tokio::test]
    async fn failed_initial_discovery_is_not_cached() {
        let backend = Arc::new(SequenceBackend::new([
            success(b"List of devices attached\n"),
            success(b"List of devices attached\nconnected\tdevice\n"),
        ]));
        let runtime = AdbRuntime::new(backend.clone(), "test-adb".to_string(), None);

        assert_eq!(
            runtime.serial().await.unwrap_err().to_string(),
            "No authorized ADB device found"
        );
        assert_eq!(runtime.serial().await.unwrap(), "connected");
        assert_eq!(backend.requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn concurrent_first_discovery_is_coalesced() {
        let backend = Arc::new(SequenceBackend::new([success(
            b"List of devices attached\nonly-one\tdevice\n",
        )]));
        let runtime = AdbRuntime::new(backend.clone(), "test-adb".to_string(), None);

        let (first, second, third) =
            tokio::join!(runtime.serial(), runtime.serial(), runtime.serial());
        assert_eq!(first.unwrap(), "only-one");
        assert_eq!(second.unwrap(), "only-one");
        assert_eq!(third.unwrap(), "only-one");
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn failed_command_invalidates_discovered_serial_for_reconnect() {
        let backend = Arc::new(SequenceBackend::new([
            success(b"List of devices attached\nold\tdevice\n"),
            failure(b"device offline"),
            success(b"List of devices attached\nnew\tdevice\n"),
            success(b"ready\n"),
        ]));
        let runtime = AdbRuntime::new(backend.clone(), "test-adb".to_string(), None);

        assert!(runtime.shell(&["get-state"]).await.is_err());
        assert_eq!(runtime.shell(&["get-state"]).await.unwrap(), "ready");
        let requests = backend.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[1].args[0..2], ["-s", "old"]);
        assert_eq!(requests[3].args[0..2], ["-s", "new"]);
    }

    #[tokio::test]
    async fn configured_serial_is_instance_owned_and_stable_after_failure() {
        let first_backend = Arc::new(SequenceBackend::new([
            failure(b"temporary failure"),
            success(b"recovered"),
        ]));
        let first = AdbRuntime::new(
            first_backend.clone(),
            "test-adb".to_string(),
            Some("explicit-a".to_string()),
        );
        let second_backend = Arc::new(SequenceBackend::new([success(b"ok")]));
        let second = AdbRuntime::new(
            second_backend.clone(),
            "test-adb".to_string(),
            Some("explicit-b".to_string()),
        );

        assert!(first.shell(&["get-state"]).await.is_err());
        assert_eq!(first.shell(&["get-state"]).await.unwrap(), "recovered");
        assert_eq!(second.shell(&["get-state"]).await.unwrap(), "ok");
        let first_requests = first_backend.requests.lock().unwrap();
        assert_eq!(first_requests.len(), 2);
        assert!(first_requests
            .iter()
            .all(|request| request.args[0..2] == ["-s", "explicit-a"]));
        let second_requests = second_backend.requests.lock().unwrap();
        assert_eq!(second_requests.len(), 1);
        assert_eq!(second_requests[0].args[0..2], ["-s", "explicit-b"]);
    }

    #[tokio::test]
    async fn unscoped_host_operations_share_the_owned_backend_and_path() {
        let backend = Arc::new(SequenceBackend::new([success(b"started")]));
        let runtime = AdbRuntime::new(backend.clone(), "owned-adb".to_string(), None);

        let output = runtime
            .execute_host(vec!["start-server".to_string()], Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(output.stdout, b"started");
        assert_eq!(
            *backend.requests.lock().unwrap(),
            vec![AdbCommand {
                program: "owned-adb".to_string(),
                args: vec!["start-server".to_string()],
                timeout: Duration::from_secs(5),
            }]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn subprocess_backend_caps_and_counts_both_output_streams() {
        let stdout_bytes = ADB_CAPTURE_LIMIT_BYTES + 17;
        let stderr_bytes = ADB_CAPTURE_LIMIT_BYTES + 29;
        let script =
            format!("head -c {stdout_bytes} /dev/zero; head -c {stderr_bytes} /dev/zero >&2");
        let output = TokioAdbBackend
            .execute(AdbCommand {
                program: "sh".to_string(),
                args: vec!["-c".to_string(), script],
                timeout: Duration::from_secs(5),
            })
            .await
            .unwrap();

        assert!(output.success);
        assert_eq!(output.stdout.len(), ADB_CAPTURE_LIMIT_BYTES);
        assert_eq!(output.stderr.len(), ADB_CAPTURE_LIMIT_BYTES);
        assert_eq!(output.stdout_total_bytes, stdout_bytes);
        assert_eq!(output.stderr_total_bytes, stderr_bytes);
        assert!(output.stdout_truncated);
        assert!(output.stderr_truncated);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn subprocess_backend_enforces_its_request_deadline() {
        let started = Instant::now();
        let error = TokioAdbBackend
            .execute(AdbCommand {
                program: "sh".to_string(),
                args: vec!["-c".to_string(), "exec sleep 5".to_string()],
                timeout: Duration::from_millis(50),
            })
            .await
            .unwrap_err();

        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
