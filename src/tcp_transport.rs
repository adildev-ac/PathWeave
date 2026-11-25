use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use ipstack::IpStackTcpStream;
use tokio::task::JoinSet;
use tracing::{error, info, warn};

use crate::{
    flow::{FlowKey, FlowRecord, FlowState},
    interface::connect_on_interface,
    qos::{QosMode, QosSession},
    relay::{relay_bidirectional, relay_bidirectional_shaped},
    scheduler::FlowMetadata,
    transport::{EngineContext, TransportEngine, TransportProtocol},
};

static NEXT_TCP_CORRELATION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
pub struct TcpTransportEngine;

impl TransportEngine<IpStackTcpStream> for TcpTransportEngine {
    fn protocol(&self) -> TransportProtocol {
        TransportProtocol::Tcp
    }

    fn dispatch(
        &self,
        inbound: IpStackTcpStream,
        context: Arc<EngineContext>,
        tasks: &mut JoinSet<()>,
    ) {
        let source = inbound.local_addr();
        let destination = inbound.peer_addr();
        let correlation_id = NEXT_TCP_CORRELATION_ID.fetch_add(1, Ordering::Relaxed);
        let key = FlowKey::tcp(source, destination);
        let Some(permit) = context.acquire_tcp() else {
            warn!(correlation_id, stage = "tcp_flow_rejected_capacity", %source, %destination, max_connections = context.max_tcp_connections, available_permits = context.tcp_slots.available_permits(), "TCP flow rejected: relay capacity is full");
            return;
        };
        let Some(decision) = context.scheduler.select(FlowMetadata::new(
            TransportProtocol::Tcp,
            source,
            destination,
        )) else {
            warn!(correlation_id, stage = "tcp_flow_rejected_scheduler", %source, %destination, "TCP flow rejected: all configured interfaces are in health cooldown");
            return;
        };
        let interface_index = decision.actual_interface;
        let flow = FlowRecord::new(key, interface_index);
        let selected_identity = context.interfaces.identity_for_index(interface_index);
        let qos = context
            .qos
            .session(flow.id, interface_index, decision.traffic_class);
        info!(correlation_id, flow_id = flow.id, scheduler_mode = ?decision.mode, actual_interface = decision.actual_interface, adaptive_recommendation = decision.recommended_interface, traffic_class = ?decision.traffic_class, fallback = decision.fallback, reason = %decision.reason, candidates = ?decision.candidates, "TCP scheduler decision");
        info!(
            flow_id = flow.id,
            correlation_id,
            %source,
            %destination,
            address_family = flow.key.address_family(),
            interface_index,
            state = ?flow.state(),
            created_at = ?flow.created_at,
            "TCP flow accepted"
        );
        info!(
            correlation_id,
            stage = "tun_flow_accepted",
            %source,
            %destination,
            scheduler_interface_index = interface_index,
            interface_guid = selected_identity.as_ref().map(|identity| identity.guid.as_str()),
            interface_luid = selected_identity.as_ref().map(|identity| identity.luid),
            interface_alias = selected_identity.as_ref().map(|identity| identity.alias.as_str()),
            interface_operational = selected_identity.as_ref().map(|identity| identity.operational),
            "TCP connection lifecycle"
        );
        tasks.spawn(run_tcp(
            inbound,
            context,
            flow,
            correlation_id,
            permit,
            source,
            destination,
            qos,
        ));
    }
}

async fn run_tcp(
    inbound: IpStackTcpStream,
    context: Arc<EngineContext>,
    flow: Arc<FlowRecord>,
    correlation_id: u64,
    _permit: tokio::sync::OwnedSemaphorePermit,
    source: std::net::SocketAddr,
    destination: std::net::SocketAddr,
    qos: QosSession,
) {
    let key = flow.key;
    if context.flows.insert(flow.clone()).await.is_err() {
        warn!(flow_id = flow.id, correlation_id, stage = "tcp_flow_rejected_duplicate", %source, %destination, "TCP flow rejected: five-tuple is already active");
        return;
    }
    flow.set_state(FlowState::Connecting);
    info!(
        correlation_id,
        flow_id = flow.id,
        stage = "interface_resolution",
        %source,
        %destination,
        interface_index = flow.interface_index,
        interface_guid = context.interfaces.guid_for_index(flow.interface_index),
        interface_identity = ?context.interfaces.identity_for_index(flow.interface_index),
        "TCP connection lifecycle"
    );
    let remote = match connect_on_interface(
        destination,
        flow.interface_index,
        correlation_id,
        "flow",
        context.connect_timeout,
    )
    .await
    {
        Ok(remote) => {
            context.interfaces.mark_healthy(flow.interface_index);
            remote
        }
        Err(error) if error.kind() != std::io::ErrorKind::TimedOut => {
            let quarantined = context
                .interfaces
                .report_path_error(flow.interface_index, &error);
            flow.set_state(FlowState::Failed);
            error!(flow_id = flow.id, correlation_id, %source, %destination, interface_index = flow.interface_index, quarantined, error_kind = ?error.kind(), raw_os_error = error.raw_os_error(), duration_ms = flow.counters.duration().as_millis() as u64, %error, "interface-bound TCP connect failed");
            context.flows.remove(key, flow.id).await;
            return;
        }
        Err(error) => {
            flow.set_state(FlowState::Failed);
            error!(flow_id = flow.id, correlation_id, %source, %destination, interface_index = flow.interface_index, timeout_secs = context.connect_timeout.as_secs(), error_kind = ?error.kind(), raw_os_error = error.raw_os_error(), %error, "interface-bound TCP connect timed out");
            context.flows.remove(key, flow.id).await;
            return;
        }
    };

    let outbound_local = remote.local_addr().ok();
    let outbound_peer = remote.peer_addr().ok();
    info!(flow_id = flow.id, correlation_id, %source, %destination, interface_index = flow.interface_index, outbound_local = ?outbound_local, outbound_peer = ?outbound_peer, "TCP flow connected and pinned");
    flow.set_state(FlowState::Relaying);
    info!(
        correlation_id,
        flow_id = flow.id,
        stage = "tcp_relay_initialized",
        %source,
        %destination,
        interface_index = flow.interface_index,
        outbound_local = ?outbound_local,
        outbound_peer = ?outbound_peer,
        "TCP connection lifecycle"
    );
    let relay_result = if context.qos.mode() == QosMode::Disabled {
        relay_bidirectional(inbound, remote, flow.counters.clone()).await
    } else {
        relay_bidirectional_shaped(inbound, remote, flow.counters.clone(), qos).await
    };
    flow.set_state(FlowState::Closing);
    let (bytes_to_network, bytes_to_client) = flow.counters.byte_counts();
    let duration_ms = flow.counters.duration().as_millis().max(1);
    let average_to_network_bps =
        ((bytes_to_network as u128 * 1000) / duration_ms).min(u64::MAX as u128) as u64;
    let average_to_client_bps =
        ((bytes_to_client as u128 * 1000) / duration_ms).min(u64::MAX as u128) as u64;
    match relay_result {
        Ok(_) => {
            flow.set_state(FlowState::Closed);
            info!(flow_id = flow.id, %source, %destination, interface_index = flow.interface_index, outbound_local = ?outbound_local, outbound_peer = ?outbound_peer, state = ?flow.state(), created_at = ?flow.created_at, bytes_to_network, bytes_to_client, average_to_network_bps, average_to_client_bps, last_activity_unix_ms = flow.counters.last_activity_unix_ms(), duration_ms = duration_ms as u64, "TCP flow closed cleanly");
        }
        Err(error) => {
            flow.set_state(FlowState::Failed);
            flow.counters.record_socket_error();
            warn!(flow_id = flow.id, %source, %destination, interface_index = flow.interface_index, outbound_local = ?outbound_local, outbound_peer = ?outbound_peer, state = ?flow.state(), bytes_to_network, bytes_to_client, %error, "TCP relay failed");
        }
    }
    context.flows.remove(key, flow.id).await;
}
