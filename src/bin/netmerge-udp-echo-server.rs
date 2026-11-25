use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::net::UdpSocket;

#[derive(Debug, Parser)]
#[command(about = "Loopback UDP echo server for Netmerge datagram tests")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:39092")]
    listen: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let socket = UdpSocket::bind(args.listen)
        .await
        .with_context(|| format!("binding UDP echo server to {}", args.listen))?;
    println!("UDP echo server listening on {}", socket.local_addr()?);
    let mut buffer = vec![0u8; 65_535];
    loop {
        let (bytes, peer) = socket.recv_from(&mut buffer).await?;
        socket.send_to(&buffer[..bytes], peer).await?;
        println!("echoed {bytes} bytes for {peer}");
    }
}
