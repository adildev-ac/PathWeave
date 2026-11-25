use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use tokio::io::{
    AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf, copy, split,
};

use crate::{
    flow::{FlowCounters, TrafficDirection},
    qos::{QosDirection, QosSession},
};

struct CountingWriter<W> {
    inner: W,
    counters: Arc<FlowCounters>,
    direction: TrafficDirection,
}

impl<W> CountingWriter<W> {
    fn new(inner: W, counters: Arc<FlowCounters>, direction: TrafficDirection) -> Self {
        Self {
            inner,
            counters,
            direction,
        }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for CountingWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Ready(Ok(bytes)) if bytes > 0 => {
                self.counters.record_written(self.direction, bytes);
                Poll::Ready(Ok(bytes))
            }
            other => other,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

async fn copy_then_half_close<R, W>(reader: &mut R, writer: &mut W) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let bytes = copy(reader, writer).await?;
    writer.shutdown().await?;
    Ok(bytes)
}

pub async fn relay_bidirectional<C, R>(
    client: C,
    remote: R,
    counters: Arc<FlowCounters>,
) -> io::Result<(u64, u64)>
where
    C: AsyncRead + AsyncWrite + Unpin,
    R: AsyncRead + AsyncWrite + Unpin,
{
    let (mut client_reader, client_writer): (ReadHalf<C>, WriteHalf<C>) = split(client);
    let (mut remote_reader, remote_writer): (ReadHalf<R>, WriteHalf<R>) = split(remote);
    let mut remote_writer =
        CountingWriter::new(remote_writer, counters.clone(), TrafficDirection::ToNetwork);
    let mut client_writer =
        CountingWriter::new(client_writer, counters, TrafficDirection::ToClient);

    tokio::try_join!(
        copy_then_half_close(&mut client_reader, &mut remote_writer),
        copy_then_half_close(&mut remote_reader, &mut client_writer),
    )
}

pub async fn relay_bidirectional_shaped<C, R>(
    client: C,
    remote: R,
    counters: Arc<FlowCounters>,
    qos: QosSession,
) -> io::Result<(u64, u64)>
where
    C: AsyncRead + AsyncWrite + Unpin,
    R: AsyncRead + AsyncWrite + Unpin,
{
    let (mut client_reader, client_writer): (ReadHalf<C>, WriteHalf<C>) = split(client);
    let (mut remote_reader, remote_writer): (ReadHalf<R>, WriteHalf<R>) = split(remote);
    let mut remote_writer =
        CountingWriter::new(remote_writer, counters.clone(), TrafficDirection::ToNetwork);
    let mut client_writer =
        CountingWriter::new(client_writer, counters, TrafficDirection::ToClient);
    tokio::try_join!(
        copy_shaped(
            &mut client_reader,
            &mut remote_writer,
            qos.clone(),
            QosDirection::Upload,
        ),
        copy_shaped(
            &mut remote_reader,
            &mut client_writer,
            qos,
            QosDirection::Download,
        ),
    )
}

async fn copy_shaped<R, W>(
    reader: &mut R,
    writer: &mut W,
    qos: QosSession,
    direction: QosDirection,
) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = [0u8; 16 * 1024];
    let mut total = 0u64;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            writer.shutdown().await?;
            return Ok(total);
        }
        let mut offset = 0;
        while offset < read {
            let requested = (read - offset).min(qos.tcp_chunk_limit(direction));
            let permit = qos.acquire_tcp(direction, requested).await?;
            let allowed = permit.bytes.min(read - offset);
            writer.write_all(&buffer[offset..offset + allowed]).await?;
            total = total.saturating_add(allowed as u64);
            drop(permit);
            offset += allowed;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    use crate::{
        flow::{FlowCounters, TrafficDirection},
        qos::{QosConfig, QosEngine},
        scheduler::TrafficClass,
    };

    use super::{relay_bidirectional, relay_bidirectional_shaped};

    #[tokio::test]
    async fn shaped_relay_preserves_full_duplex_and_half_close() {
        let (mut client, proxy_client) = duplex(128);
        let (proxy_remote, mut server) = duplex(128);
        let counters = Arc::new(FlowCounters::new());
        let qos = QosEngine::new(QosConfig {
            mode: crate::qos::QosMode::MonitorOnly,
            ..QosConfig::default()
        })
        .unwrap()
        .session(77, 8, TrafficClass::Balanced);
        let relay = tokio::spawn(async move {
            relay_bidirectional_shaped(proxy_client, proxy_remote, counters, qos).await
        });

        client.write_all(b"request").await.unwrap();
        client.shutdown().await.unwrap();
        let mut request = Vec::new();
        server.read_to_end(&mut request).await.unwrap();
        assert_eq!(request, b"request");
        server.write_all(b"response after FIN").await.unwrap();
        server.shutdown().await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, b"response after FIN");
        assert_eq!(relay.await.unwrap().unwrap(), (7, 18));
    }

    #[tokio::test]
    async fn relay_preserves_tcp_half_close_and_counts_forwarded_bytes() {
        let (mut client, proxy_client) = duplex(128);
        let (proxy_remote, mut server) = duplex(128);
        let counters = Arc::new(FlowCounters::new());
        let relay_counters = counters.clone();
        let relay = tokio::spawn(async move {
            relay_bidirectional(proxy_client, proxy_remote, relay_counters).await
        });

        client.write_all(b"request").await.unwrap();
        client.shutdown().await.unwrap();
        let mut request = Vec::new();
        server.read_to_end(&mut request).await.unwrap();
        assert_eq!(request, b"request");

        server
            .write_all(b"response after client FIN")
            .await
            .unwrap();
        server.shutdown().await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, b"response after client FIN");

        let (to_network, to_client) = relay.await.unwrap().unwrap();
        assert_eq!(to_network, 7);
        assert_eq!(to_client, 25);
        assert_eq!(counters.byte_counts(), (7, 25));
        assert!(counters.last_activity_unix_ms() > 0);
        counters.record_written(TrafficDirection::ToNetwork, 1);
        assert_eq!(counters.byte_counts(), (8, 25));
    }
    #[tokio::test]
    async fn relay_remains_bounded_and_moves_data_through_small_buffers() {
        let payload = vec![0x5a; 256 * 1024];
        let (mut client, proxy_client) = duplex(128);
        let (proxy_remote, mut server) = duplex(128);
        let counters = Arc::new(FlowCounters::new());
        let relay = {
            let counters = counters.clone();
            tokio::spawn(
                async move { relay_bidirectional(proxy_client, proxy_remote, counters).await },
            )
        };
        let server_payload = payload.clone();
        let server_task = tokio::spawn(async move {
            let mut request = Vec::new();
            server.read_to_end(&mut request).await.unwrap();
            server.write_all(&server_payload).await.unwrap();
            server.shutdown().await.unwrap();
            request
        });

        client.write_all(&payload).await.unwrap();
        client.shutdown().await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        let request = server_task.await.unwrap();
        let (to_network, to_client) = relay.await.unwrap().unwrap();

        assert_eq!(request, payload);
        assert_eq!(response.len(), 256 * 1024);
        assert_eq!(to_network, 256 * 1024);
        assert_eq!(to_client, 256 * 1024);
        assert_eq!(counters.byte_counts(), (256 * 1024, 256 * 1024));
    }
}
