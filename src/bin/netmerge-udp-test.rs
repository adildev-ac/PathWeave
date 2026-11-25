use std::{net::SocketAddr, time::Duration};

use anyhow::{Context, Result, bail};
use clap::Parser;
use tokio::{net::UdpSocket, time::timeout};

#[derive(Debug, Parser)]
#[command(
    about = "Verify UDP datagram boundaries and payload integrity against a local echo server"
)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:39092")]
    server: SocketAddr,

    #[arg(long, default_value_t = 1)]
    repetitions: usize,

    #[arg(long, default_value_t = 2000)]
    timeout_ms: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.repetitions == 0 || args.timeout_ms == 0 {
        bail!("--repetitions and --timeout-ms must be greater than zero");
    }
    let socket = UdpSocket::bind(if args.server.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })
    .await?;
    socket.connect(args.server).await?;
    let lengths = [0usize, 1, 12, 512, 1200, 1472];
    let mut received = vec![0u8; 65_535];
    let mut verified = 0usize;
    for repetition in 0..args.repetitions {
        for (case, length) in lengths.iter().copied().enumerate() {
            let payload: Vec<u8> = (0..length)
                .map(|offset| {
                    (offset.wrapping_mul(31).wrapping_add(case + repetition) & 0xff) as u8
                })
                .collect();
            socket.send(&payload).await?;
            let bytes = timeout(
                Duration::from_millis(args.timeout_ms),
                socket.recv(&mut received),
            )
            .await
            .context("UDP echo response timed out")??;
            if received[..bytes] != payload {
                bail!(
                    "datagram mismatch at repetition {repetition}, case {case}: sent {length} bytes, received {bytes}"
                );
            }
            verified += 1;
            println!("verified datagram {verified}: {bytes} bytes");
        }
    }
    println!("passed: {verified} datagrams, including empty and MTU-sized IPv4 payloads");
    Ok(())
}
