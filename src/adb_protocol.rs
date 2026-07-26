use anyhow::{anyhow, Context, Result};
use std::str;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub struct AdbClient;

impl AdbClient {
    /// Helper to format request with 4-byte hex length
    fn format_packet(payload: &str) -> String {
        format!("{:04X}{}", payload.len(), payload)
    }

    /// Connect to ADB, auto-starting server if needed
    async fn connect() -> Result<TcpStream> {
        let connect_fut = TcpStream::connect("127.0.0.1:5037");
        if let Ok(Ok(s)) =
            tokio::time::timeout(std::time::Duration::from_secs(5), connect_fut).await
        {
            Ok(s)
        } else {
            tracing::warn!("ADB Server not reachable. Attempting to start...");
            let _ = crate::adb::Adb::execute_host(
                vec!["start-server".to_string()],
                std::time::Duration::from_secs(5),
            )
            .await;
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            let retry_fut = TcpStream::connect("127.0.0.1:5037");
            match tokio::time::timeout(std::time::Duration::from_secs(5), retry_fut).await {
                Ok(Ok(s)) => Ok(s),
                _ => Err(anyhow!(
                    "Failed to connect to ADB even after restart (timed out)"
                )),
            }
        }
    }

    /// Read exactly 4 bytes and parse OKAY/FAIL
    async fn read_status(stream: &mut TcpStream) -> Result<()> {
        let mut status = [0u8; 4];
        stream
            .read_exact(&mut status)
            .await
            .context("Failed to read ADB status")?;
        match &status {
            b"OKAY" => Ok(()),
            b"FAIL" => {
                // Read failure message length and content
                let len = Self::read_hex_len(stream).await.unwrap_or(0);
                let mut msg = vec![0u8; len];
                let _ = stream.read_exact(&mut msg).await;
                let msg_str = String::from_utf8_lossy(&msg);
                Err(anyhow!("ADB Server Error: {msg_str}"))
            }
            _ => Err(anyhow!("Invalid ADB status: {:?}", str::from_utf8(&status))),
        }
    }

    /// Read 4-byte hex length
    async fn read_hex_len(stream: &mut TcpStream) -> Result<usize> {
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await?;
        let len_str = str::from_utf8(&len_buf).context("Invalid length encoding")?;
        usize::from_str_radix(len_str, 16).context("Failed to parse length")
    }

    /// Helper to switch transport to specific device
    async fn transport(stream: &mut TcpStream, serial: Option<&str>) -> Result<()> {
        // If serial provided, use host:transport:serial, else host:transport-any
        let cmd = if let Some(s) = serial {
            format!("host:transport:{s}")
        } else {
            "host:transport-any".to_string()
        };

        let packet = Self::format_packet(&cmd);
        stream.write_all(packet.as_bytes()).await?;
        Self::read_status(stream).await
    }

    /// Execute command via 'exec:' service (Raw binary, no PTY)
    pub async fn exec(cmd: &str, serial: Option<&str>) -> Result<Vec<u8>> {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let mut stream = Self::connect().await?;

            Self::transport(&mut stream, serial).await?;

            let exec_req = format!("exec:{cmd}");
            let packet = Self::format_packet(&exec_req);
            stream.write_all(packet.as_bytes()).await?;
            Self::read_status(&mut stream).await?;

            // 3. Read raw stream until EOF (with 32MB safety limit)
            let mut output = Vec::new();
            let limit = 32 * 1024 * 1024; // 32MB
            let mut handle = stream.take(limit as u64);

            handle.read_to_end(&mut output).await?;

            if output.len() >= limit {
                tracing::warn!("ADB Exec Output Truncated at 32MB");
            }

            Ok(output)
        })
        .await
        .unwrap_or_else(|_| Err(anyhow!("ADB native exec timed out after 15s")))
    }

    /// Open a raw stream for a command via 'exec:' service (Zero-copy).
    /// Returns the `TcpStream` directly for consumption.
    pub async fn exec_stream(cmd: &str, serial: Option<&str>) -> Result<TcpStream> {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let mut stream = Self::connect().await?;

            Self::transport(&mut stream, serial).await?;

            let exec_req = format!("exec:{cmd}");
            let packet = Self::format_packet(&exec_req);
            stream.write_all(packet.as_bytes()).await?;
            Self::read_status(&mut stream).await?;

            // Return stream ready for reading
            Ok(stream)
        })
        .await
        .unwrap_or_else(|_| Err(anyhow!("ADB native stream setup timed out after 15s")))
    }
}
