#[path = "../test_protocol.rs"]
mod test_protocol;

use std::{
    net::SocketAddr,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use serde::Serialize;
use sha2::{Digest, Sha256};
use test_protocol::{CHUNK_BYTES, Operation, Request, Response, fill_payload, payload_is_valid};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    task::JoinSet,
    time::{sleep, timeout},
};

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Download,
    Upload,
    Duplex,
    HalfClose,
}

impl From<Mode> for Operation {
    fn from(value: Mode) -> Self {
        match value {
            Mode::Download => Self::Download,
            Mode::Upload => Self::Upload,
            Mode::Duplex => Self::Duplex,
            Mode::HalfClose => Self::HalfClose,
        }
    }
}

#[derive(Debug, Parser)]
#[command(about = "Budgeted TCP test client for the Netmerge TUN path")]
struct Args {
    /// Controlled server address. Route this host through the test TUN for live path tests.
    #[arg(long)]
    server: SocketAddr,
    #[arg(long, value_enum, default_value_t = Mode::Download)]
    mode: Mode,
    /// Fixed transfer size per connection. Mutually exclusive with --duration-secs.
    #[arg(long, conflicts_with = "duration_secs")]
    bytes: Option<u64>,
    /// Duration per connection. Mutually exclusive with --bytes.
    #[arg(long, conflicts_with = "bytes")]
    duration_secs: Option<u64>,
    /// Maximum application payload bytes across the entire run. Required for every run.
    #[arg(long, required = true)]
    max_bytes: u64,
    #[arg(long, default_value_t = 1)]
    connections: usize,
    /// Maximum simultaneous client tasks (lower than connections performs churn).
    #[arg(long, default_value_t = 1)]
    concurrency: usize,
    /// Optional application payload rate cap per direction, in bytes per second.
    #[arg(long, default_value_t = 0)]
    rate_bytes_per_sec: u64,
    /// Optional receive-side throttle for testing TCP backpressure, in bytes per second.
    #[arg(long, default_value_t = 0)]
    receive_rate_bytes_per_sec: u64,
    /// Required for non-loopback server addresses.
    #[arg(long)]
    allow_external: bool,
    /// JSON results output path. Results also print to stdout.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct ConnectionResult {
    connection: usize,
    success: bool,
    budget_limited: bool,
    started_unix_ms: u128,
    connect_ms: u64,
    elapsed_ms: u64,
    uploaded_bytes: u64,
    downloaded_bytes: u64,
    average_upload_bytes_per_sec: u64,
    average_download_bytes_per_sec: u64,
    upload_sha256: Option<String>,
    download_sha256: Option<String>,
    local_endpoint: Option<SocketAddr>,
    error: Option<String>,
}

#[derive(Clone, Copy)]
struct SessionConfig {
    server: SocketAddr,
    mode: Mode,
    bytes: u64,
    duration_ms: u64,
    max_bytes: u64,
    rate_bytes_per_sec: u64,
    receive_rate_bytes_per_sec: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.connections == 0 || args.concurrency == 0 || args.max_bytes == 0 {
        bail!("connections, concurrency, and --max-bytes must be positive");
    }
    if args.bytes.is_none() && args.duration_secs.is_none() {
        bail!("specify --bytes or --duration-secs");
    }
    if args.duration_secs == Some(0) || args.bytes == Some(0) {
        bail!("transfer size and duration must be positive");
    }
    if !args.server.ip().is_loopback() && !args.allow_external {
        bail!(
            "external WAN test blocked; pass --allow-external and set an explicit --max-bytes budget"
        );
    }
    if args.mode == Mode::HalfClose && args.bytes.is_none() {
        bail!("HALF_CLOSE requires --bytes");
    }
    let token = std::env::var("NETMERGE_TEST_TOKEN")
        .context("set NETMERGE_TEST_TOKEN to the same secret used by the test server")?;
    if token.len() < 24 {
        bail!("NETMERGE_TEST_TOKEN must contain at least 24 characters");
    }
    let directions = if matches!(args.mode, Mode::Duplex | Mode::HalfClose) {
        2u64
    } else {
        1
    };
    let requested_total = args.bytes.map(|bytes| {
        bytes
            .saturating_mul(args.connections as u64)
            .saturating_mul(directions)
    });
    if requested_total.is_some_and(|total| total > args.max_bytes) {
        bail!(
            "requested transfer is {} bytes, exceeding the configured total budget of {} bytes",
            requested_total.unwrap(),
            args.max_bytes
        );
    }
    let per_direction_budget = args.max_bytes / (args.connections as u64 * directions);
    if per_direction_budget == 0 {
        bail!("--max-bytes is too small for the selected connection count and mode");
    }
    let expected = requested_total.unwrap_or(args.max_bytes);
    eprintln!(
        "anticipated application payload budget: at most {expected} bytes ({} connections, {:?}, {} direction(s)); cancel with Ctrl+C",
        args.connections, args.mode, directions
    );

    let mut tasks = JoinSet::new();
    let shutdown_signal = tokio::signal::ctrl_c();
    tokio::pin!(shutdown_signal);
    let mut next = 0usize;
    let mut results = Vec::with_capacity(args.connections);
    let started = Instant::now();
    let connection_timeout_secs = args.duration_secs.unwrap_or(600).saturating_add(60);
    while next < args.connections || !tasks.is_empty() {
        while next < args.connections && tasks.len() < args.concurrency {
            let index = next + 1;
            next += 1;
            let config = SessionConfig {
                server: args.server,
                mode: args.mode,
                bytes: args.bytes.unwrap_or(0),
                duration_ms: args.duration_secs.unwrap_or(0).saturating_mul(1000),
                max_bytes: per_direction_budget,
                rate_bytes_per_sec: args.rate_bytes_per_sec,
                receive_rate_bytes_per_sec: args.receive_rate_bytes_per_sec,
            };
            let token = token.clone();
            let deadline = connection_timeout_secs;
            tasks.spawn(async move {
                match timeout(
                    Duration::from_secs(deadline),
                    run_connection(index, config, token),
                )
                .await
                {
                    Ok(result) => result
                        .unwrap_or_else(|error| ConnectionResult::failed(index, error.to_string())),
                    Err(_) => {
                        ConnectionResult::failed(index, "connection deadline exceeded".into())
                    }
                }
            });
        }
        tokio::select! {
            _ = &mut shutdown_signal => {
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                bail!("test cancelled by Ctrl+C; active sockets were closed");
            }
            result = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(result) = result {
                    match result {
                        Ok(record) => results.push(record),
                        Err(error) => results.push(ConnectionResult::failed(0, error.to_string())),
                    }
                }
            }
        }
    }
    let total_elapsed = started.elapsed();
    let uploaded = results.iter().map(|row| row.uploaded_bytes).sum::<u64>();
    let downloaded = results.iter().map(|row| row.downloaded_bytes).sum::<u64>();
    let successful = results.iter().filter(|row| row.success).count();
    let budget_limited = results.iter().any(|row| row.budget_limited);
    let report = serde_json::json!({
        "schema_version":1,
        "server":args.server,
        "mode":args.mode,
        "connections_requested":args.connections,
        "connections_succeeded":successful,
        "elapsed_ms":total_elapsed.as_millis(),
        "uploaded_bytes":uploaded,
        "downloaded_bytes":downloaded,
        "total_payload_bytes":uploaded+downloaded,
        "average_payload_bytes_per_sec":(uploaded+downloaded) as f64 / total_elapsed.as_secs_f64().max(0.001),
        "configured_max_bytes":args.max_bytes,
        "budget_limited":budget_limited,
        "per_direction_budget":per_direction_budget,
        "connections":results,
    });
    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");
    if let Some(path) = args.output {
        std::fs::write(path, &json)?;
    }
    if successful != args.connections {
        bail!(
            "{} of {} test connections failed",
            args.connections - successful,
            args.connections
        );
    }
    Ok(())
}

impl ConnectionResult {
    fn failed(connection: usize, error: String) -> Self {
        Self {
            connection,
            success: false,
            budget_limited: false,
            started_unix_ms: 0,
            connect_ms: 0,
            elapsed_ms: 0,
            uploaded_bytes: 0,
            downloaded_bytes: 0,
            average_upload_bytes_per_sec: 0,
            average_download_bytes_per_sec: 0,
            upload_sha256: None,
            download_sha256: None,
            local_endpoint: None,
            error: Some(error),
        }
    }
}

async fn run_connection(
    index: usize,
    config: SessionConfig,
    token: String,
) -> Result<ConnectionResult> {
    let start = Instant::now();
    let started_unix_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let connect_started = Instant::now();
    let stream = TcpStream::connect(config.server).await?;
    let connect_ms = connect_started.elapsed().as_millis() as u64;
    let local = stream.local_addr().ok();
    let (mut read, mut write) = stream.into_split();
    let request = Request {
        token,
        operation: config.mode.into(),
        max_bytes: config.max_bytes,
        bytes: config.bytes,
        duration_ms: config.duration_ms,
        rate_bytes_per_sec: config.rate_bytes_per_sec,
        receive_rate_bytes_per_sec: config.receive_rate_bytes_per_sec,
    };
    let mut request_line = serde_json::to_vec(&request)?;
    request_line.push(b'\n');
    write.write_all(&request_line).await?;
    write.flush().await?;
    let initial = read_response(&mut read).await?;
    if !initial.ok {
        bail!(
            "server rejected request: {}",
            initial.message.unwrap_or_default()
        );
    }

    let mut uploaded = 0;
    let mut downloaded = 0;
    let mut budget_limited = false;
    let mut upload_hash = None;
    let mut download_hash = None;
    match config.mode {
        Mode::Download => {
            let stats = recv_stream(
                &mut read,
                config.max_bytes,
                config.receive_rate_bytes_per_sec,
            )
            .await?;
            let final_response = read_response(&mut read).await?;
            verify_response(&final_response, &stats.0, &stats.1)?;
            budget_limited = config.duration_ms != 0 && stats.0 >= config.max_bytes;
            downloaded = stats.0;
            download_hash = Some(stats.1);
        }
        Mode::Upload => {
            let stats = send_stream(
                &mut write,
                config.bytes,
                config.duration_ms,
                config.max_bytes,
                config.rate_bytes_per_sec,
            )
            .await?;
            let final_response = read_response(&mut read).await?;
            verify_response(&final_response, &stats.0, &stats.1)?;
            budget_limited = config.duration_ms != 0 && stats.0 >= config.max_bytes;
            uploaded = stats.0;
            upload_hash = Some(stats.1);
        }
        Mode::Duplex => {
            let sender = send_stream(
                &mut write,
                config.bytes,
                config.duration_ms,
                config.max_bytes,
                config.rate_bytes_per_sec,
            );
            let receiver = recv_stream(
                &mut read,
                config.max_bytes,
                config.receive_rate_bytes_per_sec,
            );
            let (sent, received) = tokio::try_join!(sender, receiver)?;
            let final_response = read_response(&mut read).await?;
            if !final_response.ok
                && final_response.message.as_deref()
                    != Some("byte budget reached or upload length mismatch")
            {
                bail!(
                    "server duplex check failed: {}",
                    final_response.message.unwrap_or_default()
                );
            }
            let combined = format!("upload:{};download:{}", sent.1, received.1);
            if final_response.sha256.as_deref() != Some(combined.as_str()) {
                bail!("duplex checksum mismatch");
            }
            budget_limited = config.duration_ms != 0
                && (sent.0 >= config.max_bytes || received.0 >= config.max_bytes);
            uploaded = sent.0;
            upload_hash = Some(sent.1);
            downloaded = received.0;
            download_hash = Some(received.1);
        }
        Mode::HalfClose => {
            send_raw(&mut write, config.bytes).await?;
            write.shutdown().await?;
            let final_response = read_response(&mut read).await?;
            if !final_response.ok {
                bail!("server rejected half-close payload");
            }
            let expected = final_response
                .bytes
                .context("server omitted half-close byte count")?;
            let expected_hash = final_response
                .sha256
                .context("server omitted half-close checksum")?;
            let stats = recv_raw(&mut read, expected, config.receive_rate_bytes_per_sec).await?;
            if expected != config.bytes || stats.1 != expected_hash {
                bail!("half-close response integrity check failed");
            }
            uploaded = config.bytes;
            downloaded = stats.0;
            upload_hash = Some(pattern_hash(config.bytes));
            download_hash = Some(stats.1);
        }
    }
    let elapsed_ms = start.elapsed().as_millis().max(1);
    let average_upload_bytes_per_sec =
        ((uploaded as u128 * 1000) / elapsed_ms).min(u64::MAX as u128) as u64;
    let average_download_bytes_per_sec =
        ((downloaded as u128 * 1000) / elapsed_ms).min(u64::MAX as u128) as u64;
    Ok(ConnectionResult {
        connection: index,
        success: true,
        budget_limited,
        started_unix_ms,
        connect_ms,
        elapsed_ms: elapsed_ms as u64,
        uploaded_bytes: uploaded,
        downloaded_bytes: downloaded,
        average_upload_bytes_per_sec,
        average_download_bytes_per_sec,
        upload_sha256: upload_hash,
        download_sha256: download_hash,
        local_endpoint: local,
        error: None,
    })
}

fn verify_response(response: &Response, bytes: &u64, sha: &str) -> Result<()> {
    if !response.ok && response.message.as_deref() != Some("byte budget reached") {
        bail!(
            "server reported incomplete transfer: {}",
            response.message.clone().unwrap_or_default()
        );
    }
    if response.bytes != Some(*bytes) || response.sha256.as_deref() != Some(sha) {
        bail!("transfer count or SHA-256 mismatch");
    }
    Ok(())
}

async fn read_response<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Response> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while line.len() <= test_protocol::MAX_HEADER_BYTES {
        let n = reader.read(&mut byte).await?;
        if n == 0 {
            bail!("server closed before response header");
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
    }
    if line.len() > test_protocol::MAX_HEADER_BYTES {
        bail!("oversized server response header");
    }
    Ok(serde_json::from_slice(&line)?)
}

async fn send_stream<W: AsyncWrite + Unpin>(
    writer: &mut W,
    bytes: u64,
    duration_ms: u64,
    budget: u64,
    rate: u64,
) -> Result<(u64, String)> {
    let started = Instant::now();
    let duration = (duration_ms != 0).then(|| Duration::from_millis(duration_ms));
    let mut count = 0;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; CHUNK_BYTES];
    while count < budget
        && duration.is_none_or(|limit| started.elapsed() < limit)
        && (bytes == 0 || count < bytes)
    {
        let amount = (buffer.len() as u64)
            .min(budget - count)
            .min(if bytes == 0 { u64::MAX } else { bytes - count }) as usize;
        if amount == 0 {
            break;
        }
        fill_payload(count, &mut buffer[..amount]);
        writer.write_all(&(amount as u32).to_be_bytes()).await?;
        writer.write_all(&buffer[..amount]).await?;
        hash.update(&buffer[..amount]);
        count += amount as u64;
        if rate != 0 {
            let target = Duration::from_secs_f64(count as f64 / rate as f64);
            if target > started.elapsed() {
                sleep(target - started.elapsed()).await;
            }
        }
    }
    writer.write_all(&0u32.to_be_bytes()).await?;
    Ok((count, format!("{:x}", hash.finalize())))
}

async fn recv_stream<R: AsyncRead + Unpin>(
    reader: &mut R,
    limit: u64,
    rate: u64,
) -> Result<(u64, String)> {
    let mut count = 0;
    let started = Instant::now();
    let mut hash = Sha256::new();
    loop {
        let mut frame = [0; 4];
        reader.read_exact(&mut frame).await?;
        let amount = u32::from_be_bytes(frame) as usize;
        if amount == 0 {
            break;
        }
        if amount > CHUNK_BYTES || count + amount as u64 > limit {
            bail!("received data exceeds configured byte budget");
        }
        let mut chunk = vec![0; amount];
        reader.read_exact(&mut chunk).await?;
        if !payload_is_valid(count, &chunk) {
            bail!("download checksum pattern mismatch at byte {count}");
        }
        hash.update(&chunk);
        count += amount as u64;
        if rate != 0 {
            let target = Duration::from_secs_f64(count as f64 / rate as f64);
            if target > started.elapsed() {
                sleep(target - started.elapsed()).await;
            }
        }
    }
    Ok((count, format!("{:x}", hash.finalize())))
}

async fn send_raw<W: AsyncWrite + Unpin>(writer: &mut W, bytes: u64) -> Result<()> {
    let mut count = 0;
    let mut buffer = vec![0; CHUNK_BYTES];
    while count < bytes {
        let amount = (buffer.len() as u64).min(bytes - count) as usize;
        fill_payload(count, &mut buffer[..amount]);
        writer.write_all(&buffer[..amount]).await?;
        count += amount as u64;
    }
    Ok(())
}

async fn recv_raw<R: AsyncRead + Unpin>(
    reader: &mut R,
    bytes: u64,
    rate: u64,
) -> Result<(u64, String)> {
    let mut count = 0;
    let started = Instant::now();
    let mut hash = Sha256::new();
    let mut buffer = vec![0; CHUNK_BYTES];
    while count < bytes {
        let amount = (buffer.len() as u64).min(bytes - count) as usize;
        reader.read_exact(&mut buffer[..amount]).await?;
        if !payload_is_valid(count, &buffer[..amount]) {
            bail!("half-close response payload pattern mismatch");
        }
        hash.update(&buffer[..amount]);
        count += amount as u64;
        if rate != 0 {
            let target = Duration::from_secs_f64(count as f64 / rate as f64);
            if target > started.elapsed() {
                sleep(target - started.elapsed()).await;
            }
        }
    }
    Ok((count, format!("{:x}", hash.finalize())))
}

fn pattern_hash(bytes: u64) -> String {
    let mut hash = Sha256::new();
    let mut offset = 0;
    let mut buffer = vec![0; CHUNK_BYTES];
    while offset < bytes {
        let amount = (buffer.len() as u64).min(bytes - offset) as usize;
        fill_payload(offset, &mut buffer[..amount]);
        hash.update(&buffer[..amount]);
        offset += amount as u64;
    }
    format!("{:x}", hash.finalize())
}
