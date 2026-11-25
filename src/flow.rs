use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use tokio::sync::RwLock;

static NEXT_FLOW_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FlowProtocol {
    Tcp,
    Udp,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FlowKey {
    pub source: SocketAddr,
    pub destination: SocketAddr,
    pub protocol: FlowProtocol,
}

impl FlowKey {
    pub fn tcp(source: SocketAddr, destination: SocketAddr) -> Self {
        Self {
            source,
            destination,
            protocol: FlowProtocol::Tcp,
        }
    }

    pub fn udp(source: SocketAddr, destination: SocketAddr) -> Self {
        Self {
            source,
            destination,
            protocol: FlowProtocol::Udp,
        }
    }

    pub fn address_family(self) -> &'static str {
        if self.source.is_ipv4() {
            "IPv4"
        } else {
            "IPv6"
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FlowState {
    Accepted = 0,
    Connecting = 1,
    Relaying = 2,
    Idle = 3,
    Closing = 4,
    Closed = 5,
    Failed = 6,
}

impl FlowState {
    fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Accepted,
            1 => Self::Connecting,
            2 => Self::Relaying,
            3 => Self::Idle,
            4 => Self::Closing,
            5 => Self::Closed,
            _ => Self::Failed,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TrafficDirection {
    ToNetwork,
    ToClient,
}

#[derive(Debug)]
pub struct FlowCounters {
    created: Instant,
    bytes_to_network: AtomicU64,
    bytes_to_client: AtomicU64,
    last_activity_unix_ms: AtomicU64,
    datagrams_to_network: AtomicU64,
    datagrams_to_client: AtomicU64,
    dropped_datagrams: AtomicU64,
    socket_errors: AtomicU64,
}

impl FlowCounters {
    pub(crate) fn new() -> Self {
        Self {
            created: Instant::now(),
            bytes_to_network: AtomicU64::new(0),
            bytes_to_client: AtomicU64::new(0),
            last_activity_unix_ms: AtomicU64::new(0),
            datagrams_to_network: AtomicU64::new(0),
            datagrams_to_client: AtomicU64::new(0),
            dropped_datagrams: AtomicU64::new(0),
            socket_errors: AtomicU64::new(0),
        }
    }

    pub fn record_written(&self, direction: TrafficDirection, bytes: usize) {
        let target = match direction {
            TrafficDirection::ToNetwork => &self.bytes_to_network,
            TrafficDirection::ToClient => &self.bytes_to_client,
        };
        target.fetch_add(bytes as u64, Ordering::Relaxed);
        self.last_activity_unix_ms.store(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    pub fn byte_counts(&self) -> (u64, u64) {
        (
            self.bytes_to_network.load(Ordering::Relaxed),
            self.bytes_to_client.load(Ordering::Relaxed),
        )
    }

    pub fn record_datagram(&self, direction: TrafficDirection, bytes: usize) {
        self.record_written(direction, bytes);
        let target = match direction {
            TrafficDirection::ToNetwork => &self.datagrams_to_network,
            TrafficDirection::ToClient => &self.datagrams_to_client,
        };
        target.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_drop(&self) {
        self.dropped_datagrams.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_socket_error(&self) {
        self.socket_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn datagram_counts(&self) -> (u64, u64) {
        (
            self.datagrams_to_network.load(Ordering::Relaxed),
            self.datagrams_to_client.load(Ordering::Relaxed),
        )
    }

    pub fn dropped_datagrams(&self) -> u64 {
        self.dropped_datagrams.load(Ordering::Relaxed)
    }

    pub fn socket_errors(&self) -> u64 {
        self.socket_errors.load(Ordering::Relaxed)
    }

    pub fn last_activity_unix_ms(&self) -> u64 {
        self.last_activity_unix_ms.load(Ordering::Relaxed)
    }

    pub fn duration(&self) -> std::time::Duration {
        self.created.elapsed()
    }
}

#[derive(Debug)]
pub struct FlowRecord {
    pub id: u64,
    pub key: FlowKey,
    pub interface_index: u32,
    pub created_at: SystemTime,
    pub counters: Arc<FlowCounters>,
    state: AtomicU8,
}

impl FlowRecord {
    pub fn new(key: FlowKey, interface_index: u32) -> Arc<Self> {
        Arc::new(Self {
            id: NEXT_FLOW_ID.fetch_add(1, Ordering::Relaxed),
            key,
            interface_index,
            created_at: SystemTime::now(),
            counters: Arc::new(FlowCounters::new()),
            state: AtomicU8::new(FlowState::Accepted as u8),
        })
    }

    pub fn set_state(&self, state: FlowState) {
        self.state.store(state as u8, Ordering::Relaxed);
    }

    pub fn state(&self) -> FlowState {
        FlowState::from_byte(self.state.load(Ordering::Relaxed))
    }
}

#[derive(Default)]
pub struct FlowManager {
    active: RwLock<HashMap<FlowKey, Arc<FlowRecord>>>,
}

impl FlowManager {
    pub async fn insert(&self, record: Arc<FlowRecord>) -> Result<(), Arc<FlowRecord>> {
        let mut active = self.active.write().await;
        if active.contains_key(&record.key) {
            return Err(record);
        }
        active.insert(record.key, record);
        Ok(())
    }

    pub async fn remove(&self, key: FlowKey, id: u64) {
        let mut active = self.active.write().await;
        if active.get(&key).is_some_and(|record| record.id == id) {
            active.remove(&key);
        }
    }

    pub async fn active_count(&self) -> usize {
        self.active.read().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(ip: &str, port: u16) -> SocketAddr {
        format!("{ip}:{port}").parse().unwrap()
    }

    #[test]
    fn five_tuple_identity_includes_both_endpoints_and_protocol() {
        let first = FlowKey::tcp(endpoint("192.0.2.1", 50000), endpoint("198.51.100.1", 443));
        let different_destination =
            FlowKey::tcp(endpoint("192.0.2.1", 50000), endpoint("198.51.100.2", 443));
        assert_ne!(first, different_destination);
        assert_eq!(first.address_family(), "IPv4");
    }

    #[tokio::test]
    async fn flow_manager_tracks_and_removes_records_by_id() {
        let key = FlowKey::tcp(endpoint("192.0.2.1", 50000), endpoint("198.51.100.1", 443));
        let record = FlowRecord::new(key, 8);
        let manager = FlowManager::default();
        manager.insert(record.clone()).await.unwrap();
        assert_eq!(manager.active_count().await, 1);
        manager.remove(key, record.id + 1).await;
        assert_eq!(manager.active_count().await, 1);
        manager.remove(key, record.id).await;
        assert_eq!(manager.active_count().await, 0);
    }
}
