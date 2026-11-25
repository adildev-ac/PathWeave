use std::{io, net::SocketAddr, sync::Arc};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};

use crate::{
    interface::InterfacePool,
    qos::{QosDirection, QosPolicyManager},
};

pub async fn bind_listener(address: SocketAddr) -> io::Result<TcpListener> {
    if !address.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "QoS control must bind to a loopback address",
        ));
    }
    TcpListener::bind(address).await
}

pub async fn run(
    listener: TcpListener,
    policy: Arc<QosPolicyManager>,
    interfaces: Arc<InterfacePool>,
    token: Arc<String>,
) -> io::Result<()> {
    let slots = Arc::new(Semaphore::new(8));
    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let policy = policy.clone();
        let interfaces = interfaces.clone();
        let token = token.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let _ = handle(stream, policy, interfaces, token).await;
        });
    }
}

async fn handle(
    stream: TcpStream,
    policy: Arc<QosPolicyManager>,
    interfaces: Arc<InterfacePool>,
    token: Arc<String>,
) -> io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read).take(1024);
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        reader.read_line(&mut line),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "QoS control request timed out"))??;
    let mut fields: Vec<_> = line.split_whitespace().collect();
    let authenticated = fields.len() >= 3
        && fields[0] == "AUTH"
        && constant_time_eq(fields[1].as_bytes(), token.as_bytes());
    if fields.first() == Some(&"AUTH") {
        if !authenticated {
            write.write_all(b"ERROR authentication failed\n").await?;
            return Ok(());
        }
        fields.drain(..2);
    }
    let mutating = matches!(
        fields.first(),
        Some(&"ROLLBACK" | &"SET-GUID" | &"SET-INDEX")
    );
    if mutating && !authenticated {
        write.write_all(b"ERROR authentication required\n").await?;
        return Ok(());
    }
    let response = match fields.as_slice() {
        ["GET"] => format!("{}\n", serde_json::to_string(&policy.snapshot()).unwrap_or_else(|_| "{}".into())),
        ["ROLLBACK"] => match policy.rollback().await {
            Ok(version) => format!("OK {version}\n"),
            Err(error) => format!("ERROR {error}\n"),
        },
        ["SET-GUID", guid, direction, rate] => match parse_direction(direction).and_then(|direction| parse_rate(rate).map(|rate| (direction, rate))) {
            Ok((direction, rate)) => match policy.set_interface_rate(guid, direction, rate).await {
                Ok(version) => format!("OK {version}\n"),
                Err(error) => format!("ERROR {error}\n"),
            },
            Err(error) => format!("ERROR {error}\n"),
        },
        ["SET-INDEX", index, direction, rate] => {
            let result = (|| {
                let index: u32 = index.parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid interface index"))?;
                let guid = interfaces.guid_for_index(index).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown interface index"))?;
                Ok::<_, io::Error>((guid, parse_direction(direction)?, parse_rate(rate)?))
            })();
            match result {
                Ok((guid, direction, rate)) => match policy.set_interface_rate(&guid, direction, rate).await {
                    Ok(version) => format!("OK {version} RESOLVED_GUID {guid}; index overrides are temporary\n"),
                    Err(error) => format!("ERROR {error}\n"),
                },
                Err(error) => format!("ERROR {error}\n"),
            }
        }
        _ => "ERROR use GET, SET-GUID <guid> <upload|download> <mbps>, SET-INDEX <index> <upload|download> <mbps>, or ROLLBACK\n".into(),
    };
    write.write_all(response.as_bytes()).await
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

fn parse_direction(value: &str) -> io::Result<QosDirection> {
    match value.to_ascii_lowercase().as_str() {
        "upload" => Ok(QosDirection::Upload),
        "download" => Ok(QosDirection::Download),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "direction must be upload or download",
        )),
    }
}

fn parse_rate(value: &str) -> io::Result<Option<u64>> {
    let mbps: f64 = value
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "rate must be numeric Mbps"))?;
    if !mbps.is_finite() || mbps < 0.0 || mbps > u64::MAX as f64 * 8.0 / 1_000_000.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "rate must be finite and non-negative",
        ));
    }
    Ok((mbps > 0.0).then_some((mbps * 1_000_000.0 / 8.0).ceil() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_listener_accepts_loopback_only() {
        assert!(
            "127.0.0.1:48124"
                .parse::<SocketAddr>()
                .unwrap()
                .ip()
                .is_loopback()
        );
        assert!(
            !"0.0.0.0:48124"
                .parse::<SocketAddr>()
                .unwrap()
                .ip()
                .is_loopback()
        );
        assert_eq!(parse_rate("0").unwrap(), None);
        assert_eq!(parse_rate("8").unwrap(), Some(1_000_000));
        assert!(parse_rate("NaN").is_err());
        assert_eq!(parse_direction("DOWNLOAD").unwrap(), QosDirection::Download);
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreu"));
        assert!(!constant_time_eq(b"secret", b"short"));
    }
}
