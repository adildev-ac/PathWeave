#[path = "../test_protocol.rs"]
mod test_protocol;

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde_json::json;
use sha2::{Digest, Sha256};
use test_protocol::{CHUNK_BYTES, Operation, Request, Response, fill_payload, payload_is_valid};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    task::JoinSet,
    time::{sleep, timeout},
};

#[derive(Debug, Parser)]
#[command(about = "Authenticated bounded TCP server for Pathweave soak tests")]
struct Args {
    /// Bind address. Defaults to loopback; use a VPN or SSH tunnel for remote testing.
    #[arg(long, default_value = "127.0.0.1:47800")]
    listen: SocketAddr,
    /// IP literals allowed to connect. An empty list allows any source permitted by the firewall.
    #[arg(long, value_delimiter = ',')]
    allow_source_ip: Vec<IpAddr>,
    /// Concurrent client limit.
    #[arg(long, default_value_t = 16)]
    max_connections: usize,
    /// Maximum bytes per transfer direction.
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    max_bytes: u64,
    /// Maximum lifetime of one session.
    #[arg(long, default_value_t = 600)]
    max_session_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.max_connections == 0 || args.max_bytes == 0 || args.max_session_secs == 0 {
        bail!("connection, byte, and duration limits must be positive");
    }
    if !args.listen.ip().is_loopback() && args.allow_source_ip.is_empty() {
        bail!("non-loopback binding requires at least one --allow-source-ip restriction");
    }
    let token = std::env::var("PATHWEAVE_TEST_TOKEN")
        .context("set PATHWEAVE_TEST_TOKEN in the environment before starting the test server")?;
    if token.len() < 24 {
        bail!("PATHWEAVE_TEST_TOKEN must contain at least 24 characters");
    }

    let listener = TcpListener::bind(args.listen).await?;
    println!(
        "{}",
        json!({"event":"listening", "address":listener.local_addr()?, "max_connections":args.max_connections, "max_bytes":args.max_bytes, "max_session_secs":args.max_session_secs})
    );
    let slots = Arc::new(Semaphore::new(args.max_connections));
    let allow = Arc::new(args.allow_source_ip);
    let mut tasks = JoinSet::new();
    let mut shutdown = Box::pin(tokio::signal::ctrl_c());
    loop {
        tokio::select! {
            signal = &mut shutdown => {
                signal?;
                println!("{}", json!({"event":"shutdown_requested"}));
                break;
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                if !allow.is_empty() && !allow.contains(&peer.ip()) {
                    println!("{}", json!({"event":"rejected", "peer":peer, "reason":"source_not_allowed"}));
                    drop(stream);
                    continue;
                }
                let permit = match slots.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        println!("{}", json!({"event":"rejected", "peer":peer, "reason":"connection_limit"}));
                        drop(stream);
                        continue;
                    }
                };
                let token = token.clone();
                let max_bytes = args.max_bytes;
                let max_session = Duration::from_secs(args.max_session_secs);
                tasks.spawn(async move {
                    let _permit = permit;
                    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let started = Instant::now();
                    let started_unix_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
                    let result = timeout(max_session, handle(stream, peer, id, &token, max_bytes)).await;
                    match result {
                        Ok(Ok(stats)) => {
                            let duration_ms=started.elapsed().as_millis().max(1) as u64;
                            let total_bytes=stats.0+stats.1;
                            println!("{}", json!({"event":"connection_finished", "connection_id":id, "source_ip":peer.ip(), "source_port":peer.port(), "started_unix_ms":started_unix_ms, "duration_ms":duration_ms, "bytes_received":stats.0, "bytes_transmitted":stats.1, "payload_bytes_per_sec":(total_bytes*1000)/duration_ms, "reason":"completed"}));
                        },
                        Ok(Err(error)) => println!("{}", json!({"event":"connection_finished", "connection_id":id, "source_ip":peer.ip(), "source_port":peer.port(), "started_unix_ms":started_unix_ms, "duration_ms":started.elapsed().as_millis(), "reason":"error", "error":error.to_string()})),
                        Err(_) => println!("{}", json!({"event":"connection_finished", "connection_id":id, "source_ip":peer.ip(), "source_port":peer.port(), "started_unix_ms":started_unix_ms, "duration_ms":started.elapsed().as_millis(), "reason":"session_timeout"})),
                    }
                });
            }
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(error) = result { eprintln!("test server task failed: {error}"); }
            }
        }
    }
    if timeout(Duration::from_secs(5), async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    Ok(())
}

static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

async fn handle(
    stream: TcpStream,
    peer: SocketAddr,
    id: u64,
    token: &str,
    max_bytes: u64,
) -> Result<(u64, u64)> {
    let mut read_buffer = Vec::new();
    let mut read_half = stream;
    let mut byte = [0u8; 1];
    while read_buffer.len() <= test_protocol::MAX_HEADER_BYTES {
        let n = read_half.read(&mut byte).await?;
        if n == 0 || byte[0] == b'\n' {
            break;
        }
        read_buffer.push(byte[0]);
    }
    if read_buffer.len() > test_protocol::MAX_HEADER_BYTES {
        bail!("empty or oversized request header");
    }
    let request: Request = match serde_json::from_slice(&read_buffer) {
        Ok(request) => request,
        Err(error) => {
            write_response(&mut read_half, &error_response("invalid request header")).await?;
            return Err(error.into());
        }
    };
    if request.token.as_bytes() != token.as_bytes() {
        write_response(&mut read_half, &error_response("authentication failed")).await?;
        bail!("authentication failed for {peer}");
    }
    if request.max_bytes == 0 || request.max_bytes > max_bytes {
        write_response(
            &mut read_half,
            &error_response("invalid or excessive client byte budget"),
        )
        .await?;
        bail!("client byte budget exceeds server limit");
    }
    if (request.bytes == 0) == (request.duration_ms == 0) {
        write_response(
            &mut read_half,
            &error_response("specify exactly one of bytes or duration_ms"),
        )
        .await?;
        bail!("request must have either a byte limit or a duration");
    }
    if request.bytes > request.max_bytes {
        write_response(
            &mut read_half,
            &error_response("requested bytes exceed client budget"),
        )
        .await?;
        bail!("requested bytes exceed client budget");
    }
    if request.operation == Operation::HalfClose && request.duration_ms != 0 {
        write_response(
            &mut read_half,
            &error_response("HALF_CLOSE requires a fixed byte count"),
        )
        .await?;
        bail!("HALF_CLOSE requires a fixed byte count");
    }

    let destination_port = read_half.local_addr()?.port();
    println!(
        "{}",
        json!({"event":"connection_started", "connection_id":id, "source_ip":peer.ip(), "source_port":peer.port(), "destination_port":destination_port, "operation":request.operation, "byte_limit":request.max_bytes})
    );
    let (mut read, mut write) = read_half.into_split();
    let session_start = Instant::now();
    match request.operation {
        Operation::Download => {
            write_response(
                &mut write,
                &Response {
                    ok: true,
                    operation: Some(Operation::Download),
                    bytes: None,
                    sha256: None,
                    elapsed_ms: None,
                    message: None,
                },
            )
            .await?;
            let stats = send_stream(
                &mut write,
                request.bytes,
                request.duration_ms,
                request.max_bytes,
                request.rate_bytes_per_sec,
            )
            .await?;
            let complete = if request.bytes != 0 {
                stats.0 == request.bytes
            } else {
                stats.0 < request.max_bytes
            };
            write_response(
                &mut write,
                &Response {
                    ok: complete,
                    operation: Some(Operation::Download),
                    bytes: Some(stats.0),
                    sha256: Some(stats.1.clone()),
                    elapsed_ms: Some(session_start.elapsed().as_millis() as u64),
                    message: (!complete).then(|| "byte budget reached".to_string()),
                },
            )
            .await?;
            write.shutdown().await?;
            Ok((0, stats.0))
        }
        Operation::Upload => {
            write_response(
                &mut write,
                &Response {
                    ok: true,
                    operation: Some(Operation::Upload),
                    bytes: None,
                    sha256: None,
                    elapsed_ms: None,
                    message: None,
                },
            )
            .await?;
            let stats = recv_stream(
                &mut read,
                request.max_bytes,
                request.receive_rate_bytes_per_sec,
            )
            .await?;
            let expected_ok = request.duration_ms != 0 || stats.0 == request.bytes;
            write_response(
                &mut write,
                &Response {
                    ok: expected_ok,
                    operation: Some(Operation::Upload),
                    bytes: Some(stats.0),
                    sha256: Some(stats.1.clone()),
                    elapsed_ms: Some(session_start.elapsed().as_millis() as u64),
                    message: (!expected_ok)
                        .then(|| "received byte count did not match request".to_string()),
                },
            )
            .await?;
            write.shutdown().await?;
            Ok((stats.0, 0))
        }
        Operation::Duplex => {
            write_response(
                &mut write,
                &Response {
                    ok: true,
                    operation: Some(Operation::Duplex),
                    bytes: None,
                    sha256: None,
                    elapsed_ms: None,
                    message: None,
                },
            )
            .await?;
            let sender = send_stream(
                &mut write,
                request.bytes,
                request.duration_ms,
                request.max_bytes,
                request.rate_bytes_per_sec,
            );
            let receiver = recv_stream(
                &mut read,
                request.max_bytes,
                request.receive_rate_bytes_per_sec,
            );
            let (sent, received) = tokio::try_join!(sender, receiver)?;
            let upload_ok = request.duration_ms != 0 || received.0 == request.bytes;
            let download_ok = if request.bytes != 0 {
                sent.0 == request.bytes
            } else {
                sent.0 < request.max_bytes
            };
            write_response(
                &mut write,
                &Response {
                    ok: upload_ok && download_ok,
                    operation: Some(Operation::Duplex),
                    bytes: Some(sent.0 + received.0),
                    sha256: Some(format!("upload:{};download:{}", received.1, sent.1)),
                    elapsed_ms: Some(session_start.elapsed().as_millis() as u64),
                    message: (!upload_ok || !download_ok)
                        .then(|| "byte budget reached or upload length mismatch".to_string()),
                },
            )
            .await?;
            write.shutdown().await?;
            Ok((received.0, sent.0))
        }
        Operation::HalfClose => {
            write_response(
                &mut write,
                &Response {
                    ok: true,
                    operation: Some(Operation::HalfClose),
                    bytes: None,
                    sha256: None,
                    elapsed_ms: None,
                    message: None,
                },
            )
            .await?;
            let (uploaded, upload_hash) = recv_exact_raw(
                &mut read,
                request.bytes,
                request.max_bytes,
                request.receive_rate_bytes_per_sec,
            )
            .await?;
            let mut eof_probe = [0u8; 1];
            if read.read(&mut eof_probe).await? != 0 {
                bail!("expected client FIN after half-close payload");
            }
            let response = Response {
                ok: uploaded == request.bytes,
                operation: Some(Operation::HalfClose),
                bytes: Some(request.bytes),
                sha256: Some(sha256_of_pattern(request.bytes)),
                elapsed_ms: Some(session_start.elapsed().as_millis() as u64),
                message: None,
            };
            write_response(&mut write, &response).await?;
            let sent = send_raw(&mut write, request.bytes).await?;
            write.shutdown().await?;
            println!(
                "{}",
                json!({"event":"half_close_verified", "connection_id":id, "upload_bytes":uploaded, "upload_sha256":upload_hash, "download_bytes":sent})
            );
            Ok((uploaded, sent))
        }
    }
}

async fn write_response<W: AsyncWrite + Unpin>(writer: &mut W, response: &Response) -> Result<()> {
    let mut line = serde_json::to_vec(response)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await?;
    Ok(())
}

fn error_response(message: &str) -> Response {
    Response {
        ok: false,
        operation: None,
        bytes: None,
        sha256: None,
        elapsed_ms: None,
        message: Some(message.to_string()),
    }
}

async fn send_stream<W: AsyncWrite + Unpin>(
    mut writer: W,
    bytes: u64,
    duration_ms: u64,
    budget: u64,
    rate: u64,
) -> Result<(u64, String)> {
    let started = Instant::now();
    let duration = (duration_ms != 0).then(|| Duration::from_millis(duration_ms));
    let mut count = 0u64;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; CHUNK_BYTES];
    while count < budget
        && (duration.is_none_or(|limit| started.elapsed() < limit))
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
    mut reader: R,
    limit: u64,
    rate: u64,
) -> Result<(u64, String)> {
    let mut count = 0u64;
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
            bail!("incoming frame exceeds configured byte limit");
        }
        let mut chunk = vec![0; amount];
        reader.read_exact(&mut chunk).await?;
        if !payload_is_valid(count, &chunk) {
            bail!("incoming payload pattern validation failed at byte {count}");
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

async fn recv_exact_raw<R: AsyncRead + Unpin>(
    mut reader: R,
    bytes: u64,
    limit: u64,
    rate: u64,
) -> Result<(u64, String)> {
    if bytes > limit {
        bail!("half-close payload exceeds byte limit");
    }
    let mut count = 0;
    let started = Instant::now();
    let mut hash = Sha256::new();
    let mut buffer = vec![0; CHUNK_BYTES];
    while count < bytes {
        let amount = (buffer.len() as u64).min(bytes - count) as usize;
        reader.read_exact(&mut buffer[..amount]).await?;
        if !payload_is_valid(count, &buffer[..amount]) {
            bail!("half-close payload failed pattern validation");
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

async fn send_raw<W: AsyncWrite + Unpin>(mut writer: W, bytes: u64) -> Result<u64> {
    let mut count = 0;
    let mut buffer = vec![0; CHUNK_BYTES];
    while count < bytes {
        let amount = (buffer.len() as u64).min(bytes - count) as usize;
        fill_payload(count, &mut buffer[..amount]);
        writer.write_all(&buffer[..amount]).await?;
        count += amount as u64;
    }
    Ok(count)
}

fn sha256_of_pattern(bytes: u64) -> String {
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
