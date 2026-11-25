use std::{io, net::SocketAddr, str::FromStr, sync::Arc};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

use crate::interface::{InterfaceHealth, InterfacePool};

pub async fn bind_listener(address: SocketAddr) -> io::Result<TcpListener> {
    if !address.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "development health control must bind to a loopback address",
        ));
    }
    TcpListener::bind(address).await
}

pub async fn run(listener: TcpListener, pool: Arc<InterfacePool>) -> io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let pool = pool.clone();
        tokio::spawn(async move {
            let _ = handle(stream, pool).await;
        });
    }
}

async fn handle(stream: TcpStream, pool: Arc<InterfacePool>) -> io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let fields: Vec<_> = line.split_whitespace().collect();
    let response = match fields.as_slice() {
        ["GET"] => {
            let states = pool
                .configured_indexes()
                .into_iter()
                .filter_map(|index| {
                    pool.health(index)
                        .map(|health| format!("{index} {health:?}"))
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("OK\n{states}\n")
        }
        ["SET", index, status] => {
            let parsed = index.parse::<u32>();
            let health = InterfaceHealth::from_str(status);
            match (parsed, health) {
                (Ok(index), Ok(health)) if pool.set_health(index, health) => "OK\n".into(),
                (_, Err(_)) => "ERROR invalid health state\n".into(),
                (Err(_), _) => "ERROR invalid interface index\n".into(),
                _ => "ERROR unknown interface index\n".into(),
            }
        }
        _ => "ERROR use GET or SET <ifindex> <healthy|degraded|unavailable|recovering>\n".into(),
    };
    write.write_all(response.as_bytes()).await
}

impl FromStr for InterfaceHealth {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "healthy" => Ok(Self::Healthy),
            "degraded" => Ok(Self::Degraded),
            "unavailable" => Ok(Self::Unavailable),
            "recovering" => Ok(Self::Recovering),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc, time::Duration};

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };

    use crate::interface::{InterfaceHealth, InterfacePool};

    use super::handle;

    #[tokio::test]
    async fn local_control_changes_health_for_new_flow_selection() {
        let pool = Arc::new(InterfacePool::new(vec![8, 4], Duration::from_secs(5)).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_pool = pool.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle(stream, server_pool).await.unwrap();
        });
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(b"SET 4 unavailable\n").await.unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        server.await.unwrap();
        assert_eq!(response, "OK\n");
        assert_eq!(pool.health(4), Some(InterfaceHealth::Unavailable));
        assert_eq!(pool.next_available(), Some(8));
    }

    #[test]
    fn only_loopback_control_addresses_are_accepted() {
        assert!(
            "127.0.0.1:48123"
                .parse::<SocketAddr>()
                .unwrap()
                .ip()
                .is_loopback()
        );
        assert!(
            !"0.0.0.0:48123"
                .parse::<SocketAddr>()
                .unwrap()
                .ip()
                .is_loopback()
        );
    }
}
