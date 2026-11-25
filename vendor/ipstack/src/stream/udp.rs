use crate::{
    IpStackError, PacketSender, TTL,
    packet::{IpHeader, NetworkPacket, TransportHeader},
};
use etherparse::{IpNumber, Ipv4Header, Ipv6FlowLabel, Ipv6Header, UdpHeader};
use std::{future::Future, net::SocketAddr, pin::Pin, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
    time::Sleep,
};

/// A UDP flow in the IP stack.
///
/// Prefer [`recv_datagram`](Self::recv_datagram) and
/// [`send_datagram`](Self::send_datagram) to preserve datagram boundaries,
/// including empty datagrams. The `AsyncRead` / `AsyncWrite` implementations
/// are compatibility helpers and do not provide UDP byte-stream semantics or
/// IP fragmentation.
///
/// # Examples
///
/// ```no_run
/// use ipstack::{IpStack, IpStackConfig, IpStackStream};
/// # async fn example(mut ip_stack: IpStack) -> Result<(), Box<dyn std::error::Error>> {
/// if let IpStackStream::Udp(mut udp_stream) = ip_stack.accept().await? {
///     println!("New UDP stream from {}", udp_stream.peer_addr());
///     
///     // Read and write whole datagrams.
///     if let Some(datagram) = udp_stream.recv_datagram().await? {
///         udp_stream.send_datagram(&datagram)?;
///     }
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct IpStackUdpStream {
    src_addr: SocketAddr,
    dst_addr: SocketAddr,
    stream_sender: mpsc::Sender<NetworkPacket>,
    stream_receiver: mpsc::Receiver<NetworkPacket>,
    up_pkt_sender: PacketSender,
    first_payload: Option<Vec<u8>>,
    timeout: Pin<Box<Sleep>>,
    timeout_interval: Duration,
    mtu: u16,
    destroy_messenger: DestroyNotifier,
}

#[derive(Debug)]
struct DestroyNotifier(Option<::tokio::sync::oneshot::Sender<()>>);

impl Drop for DestroyNotifier {
    fn drop(&mut self) {
        if let Some(messenger) = self.0.take() {
            let _ = messenger.send(());
        }
    }
}

/// Receive-side handle for a UDP flow, used when upload and download need
/// independent pacing.
#[derive(Debug)]
pub struct IpStackUdpReceiver {
    receiver: mpsc::Receiver<NetworkPacket>,
    first_payload: Option<Vec<u8>>,
    _destroy_messenger: DestroyNotifier,
}

impl IpStackUdpReceiver {
    pub async fn recv_datagram(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        if let Some(payload) = self.first_payload.take() {
            return Ok(Some(payload));
        }
        Ok(self.receiver.recv().await.map(|packet| packet.payload.unwrap_or_default()))
    }
}

/// Send-side handle for a UDP flow, used when upload and download need
/// independent pacing.
#[derive(Debug, Clone)]
pub struct IpStackUdpSender {
    src_addr: SocketAddr,
    dst_addr: SocketAddr,
    up_pkt_sender: PacketSender,
    mtu: u16,
}

impl IpStackUdpSender {
    pub fn send_datagram(&self, payload: &[u8]) -> std::io::Result<()> {
        let packet = create_reverse_packet(self.src_addr, self.dst_addr, self.mtu, TTL, payload.to_vec())?;
        self.up_pkt_sender
            .send(packet)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }
}

impl IpStackUdpStream {
    pub fn new(
        src_addr: SocketAddr,
        dst_addr: SocketAddr,
        payload: Vec<u8>,
        up_pkt_sender: PacketSender,
        mtu: u16,
        timeout_interval: Duration,
        queue_capacity: usize,
        destroy_messenger: Option<::tokio::sync::oneshot::Sender<()>>,
    ) -> Self {
        let (stream_sender, stream_receiver) = mpsc::channel::<NetworkPacket>(queue_capacity.max(1));
        let deadline = tokio::time::Instant::now() + timeout_interval;
        IpStackUdpStream {
            src_addr,
            dst_addr,
            stream_sender,
            stream_receiver,
            up_pkt_sender,
            first_payload: Some(payload),
            timeout: Box::pin(tokio::time::sleep_until(deadline)),
            timeout_interval,
            mtu,
            destroy_messenger: DestroyNotifier(destroy_messenger),
        }
    }

    pub(crate) fn stream_sender(&self) -> mpsc::Sender<NetworkPacket> {
        self.stream_sender.clone()
    }

    /// Receive one whole UDP datagram. Empty datagrams are `Some(Vec::new())`.
    pub async fn recv_datagram(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        let payload = if let Some(payload) = self.first_payload.take() {
            payload
        } else {
            tokio::select! {
                _ = &mut self.timeout => {
                    return Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
                }
                packet = self.stream_receiver.recv() => match packet {
                    Some(packet) => packet.payload.unwrap_or_default(),
                    None => return Ok(None),
                }
            }
        };
        self.reset_timeout();
        Ok(Some(payload))
    }

    /// Return one whole UDP datagram through the original TUN flow.
    /// Oversized replies are rejected instead of being silently truncated.
    pub fn send_datagram(&mut self, payload: &[u8]) -> std::io::Result<()> {
        let packet = self.create_rev_packet(TTL, payload.to_vec())?;
        self.up_pkt_sender
            .send(packet)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
        self.reset_timeout();
        Ok(())
    }

    pub fn into_datagram_halves(self) -> (IpStackUdpReceiver, IpStackUdpSender) {
        let Self {
            src_addr,
            dst_addr,
            stream_sender: _,
            stream_receiver,
            up_pkt_sender,
            first_payload,
            timeout: _,
            timeout_interval: _,
            mtu,
            destroy_messenger,
        } = self;
        (
            IpStackUdpReceiver {
                receiver: stream_receiver,
                first_payload,
                _destroy_messenger: destroy_messenger,
            },
            IpStackUdpSender {
                src_addr,
                dst_addr,
                up_pkt_sender,
                mtu,
            },
        )
    }

    fn create_rev_packet(&self, ttl: u8, payload: Vec<u8>) -> std::io::Result<NetworkPacket> {
        create_reverse_packet(self.src_addr, self.dst_addr, self.mtu, ttl, payload)
    }

    /// Returns the local socket address of the UDP stream.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use ipstack::IpStackUdpStream;
    /// # fn example(udp_stream: &IpStackUdpStream) {
    /// let local_addr = udp_stream.local_addr();
    /// println!("Local address: {}", local_addr);
    /// # }
    /// ```
    pub fn local_addr(&self) -> SocketAddr {
        self.src_addr
    }

    /// Returns the remote socket address of the UDP stream.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use ipstack::IpStackUdpStream;
    /// # fn example(udp_stream: &IpStackUdpStream) {
    /// let peer_addr = udp_stream.peer_addr();
    /// println!("Peer address: {}", peer_addr);
    /// # }
    /// ```
    pub fn peer_addr(&self) -> SocketAddr {
        self.dst_addr
    }

    fn reset_timeout(&mut self) {
        let deadline = tokio::time::Instant::now() + self.timeout_interval;
        self.timeout.as_mut().reset(deadline);
    }
}

impl AsyncRead for IpStackUdpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if let Some(p) = self.first_payload.take() {
            if p.is_empty() || p.len() > buf.remaining() {
                return std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "empty or oversized datagram requires recv_datagram",
                )));
            }
            buf.put_slice(&p);
            return std::task::Poll::Ready(Ok(()));
        }
        if matches!(self.timeout.as_mut().poll(cx), std::task::Poll::Ready(_)) {
            return std::task::Poll::Ready(Err(std::io::Error::from(std::io::ErrorKind::TimedOut)));
        }

        self.reset_timeout();

        match self.stream_receiver.poll_recv(cx) {
            std::task::Poll::Ready(Some(p)) => {
                let payload = p.payload.unwrap_or_default();
                if payload.is_empty() || payload.len() > buf.remaining() {
                    return std::task::Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "empty or oversized datagram requires recv_datagram",
                    )));
                }
                buf.put_slice(&payload);
                std::task::Poll::Ready(Ok(()))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(Ok(())),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl AsyncWrite for IpStackUdpStream {
    fn poll_write(mut self: Pin<&mut Self>, _cx: &mut std::task::Context<'_>, buf: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
        self.reset_timeout();
        let packet = self.create_rev_packet(TTL, buf.to_vec())?;
        let payload_len = packet.payload.as_ref().map(|p| p.len()).unwrap_or(0);
        self.up_pkt_sender.send(packet).or(Err(std::io::ErrorKind::UnexpectedEof))?;
        std::task::Poll::Ready(Ok(payload_len))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

fn create_reverse_packet(
    src_addr: SocketAddr,
    dst_addr: SocketAddr,
    mtu: u16,
    ttl: u8,
    payload: Vec<u8>,
) -> std::io::Result<NetworkPacket> {
    const UHS: usize = 8;
    match (dst_addr.ip(), src_addr.ip()) {
        (std::net::IpAddr::V4(dst), std::net::IpAddr::V4(src)) => {
            let mut ip_h = Ipv4Header::new(0, ttl, IpNumber::UDP, dst.octets(), src.octets()).map_err(IpStackError::from)?;
            if ip_h.header_len() + UHS + payload.len() > mtu as usize {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "UDP datagram exceeds TUN MTU; IP fragmentation is not implemented",
                ));
            }
            ip_h.set_payload_len(payload.len() + UHS).map_err(IpStackError::from)?;
            let udp_header =
                UdpHeader::with_ipv4_checksum(dst_addr.port(), src_addr.port(), &ip_h, &payload).map_err(IpStackError::from)?;
            Ok(NetworkPacket {
                ip: IpHeader::Ipv4(ip_h),
                transport: TransportHeader::Udp(udp_header),
                payload: Some(payload),
            })
        }
        (std::net::IpAddr::V6(dst), std::net::IpAddr::V6(src)) => {
            let mut ip_h = Ipv6Header {
                traffic_class: 0,
                flow_label: Ipv6FlowLabel::ZERO,
                payload_length: 0,
                next_header: IpNumber::UDP,
                hop_limit: ttl,
                source: dst.octets(),
                destination: src.octets(),
            };
            if ip_h.header_len() + UHS + payload.len() > mtu as usize {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "UDP datagram exceeds TUN MTU; IPv6 fragmentation is not implemented",
                ));
            }
            ip_h.payload_length = (payload.len() + UHS) as u16;
            let udp_header =
                UdpHeader::with_ipv6_checksum(dst_addr.port(), src_addr.port(), &ip_h, &payload).map_err(IpStackError::from)?;
            Ok(NetworkPacket {
                ip: IpHeader::Ipv6(ip_h),
                transport: TransportHeader::Udp(udp_header),
                payload: Some(payload),
            })
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    fn stream(first_payload: Vec<u8>) -> IpStackUdpStream {
        let (up_tx, _up_rx) = mpsc::unbounded_channel();
        IpStackUdpStream::new(
            "127.0.0.1:1234".parse().unwrap(),
            "127.0.0.1:53".parse().unwrap(),
            first_payload,
            up_tx,
            1500,
            Duration::from_secs(30),
            8,
            None,
        )
    }

    #[tokio::test]
    async fn datagram_api_preserves_empty_and_consecutive_datagrams() {
        let mut s = stream(Vec::new());
        assert_eq!(s.recv_datagram().await.unwrap(), Some(Vec::new()));

        let first = s.create_rev_packet(64, b"one".to_vec()).unwrap();
        let second = s.create_rev_packet(64, b"two".to_vec()).unwrap();
        s.stream_sender().try_send(first).unwrap();
        s.stream_sender().try_send(second).unwrap();
        assert_eq!(s.recv_datagram().await.unwrap(), Some(b"one".to_vec()));
        assert_eq!(s.recv_datagram().await.unwrap(), Some(b"two".to_vec()));
    }

    #[tokio::test]
    async fn datagram_async_read_rejects_truncation_and_empty_is_not_eof() {
        let mut s = stream(vec![7u8; 1250]);
        let mut small = [0u8; 502];
        assert_eq!(s.read(&mut small).await.unwrap_err().kind(), std::io::ErrorKind::InvalidData);

        let mut empty = stream(Vec::new());
        assert_eq!(empty.read(&mut small).await.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn stream_write_rejects_payload_larger_than_tun_mtu() {
        let mut s = stream(Vec::new());
        let error = s.send_datagram(&vec![9u8; 1473]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn bounded_inbound_queue_applies_backpressure() {
        let mut s = stream(Vec::new());
        for _ in 0..8 {
            let packet = s.create_rev_packet(64, vec![1]).unwrap();
            s.stream_sender().try_send(packet).unwrap();
        }
        let packet = s.create_rev_packet(64, vec![2]).unwrap();
        assert!(s.stream_sender().try_send(packet).is_err());
    }

    #[tokio::test]
    async fn send_datagram_preserves_empty_datagram_boundary() {
        let (up_tx, mut up_rx) = mpsc::unbounded_channel();
        let mut s = IpStackUdpStream::new(
            "127.0.0.1:1234".parse().unwrap(),
            "127.0.0.1:53".parse().unwrap(),
            Vec::new(),
            up_tx,
            1500,
            Duration::from_secs(30),
            8,
            None,
        );
        s.send_datagram(&[]).unwrap();
        s.send_datagram(b"reply").unwrap();
        assert_eq!(up_rx.recv().await.unwrap().payload, Some(Vec::new()));
        assert_eq!(up_rx.recv().await.unwrap().payload, Some(b"reply".to_vec()));
    }
}
