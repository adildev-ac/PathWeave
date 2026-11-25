use std::{sync::Arc, time::Duration};

use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};

use crate::qos::QosEngine;
use crate::scheduler::FlowScheduler;
use crate::{flow::FlowManager, interface::InterfacePool};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportProtocol {
    Tcp,
    Udp,
}

pub struct EngineContext {
    pub interfaces: Arc<InterfacePool>,
    pub scheduler: Arc<FlowScheduler>,
    pub qos: Arc<QosEngine>,
    pub flows: Arc<FlowManager>,
    pub tcp_slots: Arc<Semaphore>,
    pub max_tcp_connections: usize,
    pub udp_slots: Arc<Semaphore>,
    pub connect_timeout: Duration,
    pub udp_idle_timeout: Duration,
    pub udp_max_datagram_bytes: usize,
}

impl EngineContext {
    pub fn acquire_tcp(&self) -> Option<OwnedSemaphorePermit> {
        self.tcp_slots.clone().try_acquire_owned().ok()
    }

    pub fn acquire_udp(&self) -> Option<OwnedSemaphorePermit> {
        self.udp_slots.clone().try_acquire_owned().ok()
    }
}

/// Transport-specific data plane. Each implementation owns its flow semantics
/// while sharing interface health, flow identity, and capacity policy.
pub trait TransportEngine<Session>: Send + Sync {
    fn protocol(&self) -> TransportProtocol;
    fn dispatch(&self, session: Session, context: Arc<EngineContext>, tasks: &mut JoinSet<()>);
}
