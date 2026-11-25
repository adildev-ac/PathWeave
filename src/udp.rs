use std::{io, sync::Arc};

use ipstack::IpStackUdpStream;
use tokio::{task::JoinSet, time::Instant};
use tracing::{error, info, warn};

use crate::{
    flow::{FlowKey, FlowRecord, FlowState, TrafficDirection},
    interface::bind_udp_on_interface,
    qos::{QosDirection, QosMode, QosSession},
    scheduler::FlowMetadata,
    transport::{EngineContext, TransportEngine, TransportProtocol},
};

#[derive(Default)]
pub struct UdpTransportEngine;

impl TransportEngine<IpStackUdpStream> for UdpTransportEngine {
    fn protocol(&self) -> TransportProtocol {
        TransportProtocol::Udp
    }

    fn dispatch(
        &self,
        inbound: IpStackUdpStream,
        context: Arc<EngineContext>,
        tasks: &mut JoinSet<()>,
    ) {
        let source = inbound.local_addr();
        let destination = inbound.peer_addr();
        if source.is_ipv6() || destination.is_ipv6() {
            warn!(%source, %destination, "IPv6 UDP is outside the IPv4-focused M1B data plane");
            return;
        }
        let key = FlowKey::udp(source, destination);
        let Some(permit) = context.acquire_udp() else {
            warn!(%source, %destination, "UDP flow rejected: session capacity is full");
            return;
        };
        let Some(decision) = context.scheduler.select(FlowMetadata::new(
            TransportProtocol::Udp,
            source,
            destination,
        )) else {
            warn!(%source, %destination, "UDP flow rejected: all configured interfaces are in health cooldown");
            return;
        };
        let interface_index = decision.actual_interface;
        let flow = FlowRecord::new(key, interface_index);
        let qos = context
            .qos
            .session(flow.id, interface_index, decision.traffic_class);
        info!(flow_id = flow.id, scheduler_mode = ?decision.mode, actual_interface = decision.actual_interface, adaptive_recommendation = decision.recommended_interface, traffic_class = ?decision.traffic_class, fallback = decision.fallback, reason = %decision.reason, candidates = ?decision.candidates, "UDP scheduler decision");
        info!(
            flow_id = flow.id,
            %source,
            %destination,
            address_family = flow.key.address_family(),
            interface_index,
            state = ?flow.state(),
            created_at = ?flow.created_at,
            "UDP flow accepted"
        );
        tasks.spawn(run_udp(
            inbound,
            context,
            flow,
            permit,
            source,
            destination,
            qos,
        ));
    }
}

async fn run_udp(
    mut inbound: IpStackUdpStream,
    context: Arc<EngineContext>,
    flow: Arc<FlowRecord>,
    _permit: tokio::sync::OwnedSemaphorePermit,
    source: std::net::SocketAddr,
    destination: std::net::SocketAddr,
    qos: QosSession,
) {
    let key = flow.key;
    if context.flows.insert(flow.clone()).await.is_err() {
        warn!(flow_id = flow.id, %source, %destination, "UDP flow rejected: five-tuple is already active");
        return;
    }

    flow.set_state(FlowState::Connecting);
    let socket = match bind_udp_on_interface(destination, flow.interface_index) {
        Ok(socket) => {
            context.interfaces.mark_healthy(flow.interface_index);
            socket
        }
        Err(error) => {
            let quarantined = context
                .interfaces
                .report_path_error(flow.interface_index, &error);
            flow.set_state(FlowState::Failed);
            flow.counters.record_socket_error();
            error!(flow_id = flow.id, %source, %destination, interface_index = flow.interface_index, quarantined, %error, "interface-bound UDP socket setup failed");
            context.flows.remove(key, flow.id).await;
            return;
        }
    };
    let outbound_local = socket.local_addr().ok();
    flow.set_state(FlowState::Relaying);
    info!(flow_id = flow.id, %source, %destination, interface_index = flow.interface_index, outbound_local = ?outbound_local, "UDP flow connected and pinned");

    let idle_timeout = context.udp_idle_timeout;
    let end_reason = if context.qos.mode() == QosMode::Disabled {
        let mut buffer = vec![0u8; 65_535];
        let mut last_activity = Instant::now();
        let mut reason = "inbound flow closed";
        loop {
            let deadline = last_activity + idle_timeout;
            tokio::select! {
                datagram = inbound.recv_datagram() => {
                    match datagram {
                    Ok(Some(payload)) => {
                        if payload.len() > context.udp_max_datagram_bytes {
                            flow.counters.record_drop();
                            warn!(flow_id = flow.id, datagram_bytes = payload.len(), max_datagram_bytes = context.udp_max_datagram_bytes, "UDP request dropped: configured datagram limit exceeded");
                            continue;
                        }
                        match socket.send(&payload).await {
                            Ok(bytes) if bytes == payload.len() => {
                                flow.counters.record_datagram(TrafficDirection::ToNetwork, bytes);
                                last_activity = Instant::now();
                            }
                            Ok(bytes) => {
                                flow.counters.record_drop();
                                flow.counters.record_socket_error();
                                warn!(flow_id = flow.id, sent_bytes = bytes, expected_bytes = payload.len(), "UDP request send was unexpectedly partial");
                            }
                            Err(error) => {
                                flow.counters.record_drop();
                                flow.counters.record_socket_error();
                                context.interfaces.report_path_error(flow.interface_index, &error);
                                reason = "outbound send error";
                                break;
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                        reason = "inbound idle timeout";
                        break;
                    }
                    Err(error) => {
                        flow.counters.record_socket_error();
                        reason = "inbound receive error";
                        warn!(flow_id = flow.id, %error, "UDP inbound receive failed");
                        break;
                    }
                    }
                }
                received = socket.recv(&mut buffer) => {
                    match received {
                    Ok(bytes) => {
                        if bytes > context.udp_max_datagram_bytes {
                            flow.counters.record_drop();
                            warn!(flow_id = flow.id, datagram_bytes = bytes, max_datagram_bytes = context.udp_max_datagram_bytes, "UDP reply dropped: configured datagram limit exceeded");
                            last_activity = Instant::now();
                            continue;
                        }
                        match inbound.send_datagram(&buffer[..bytes]) {
                            Ok(()) => {
                                flow.counters.record_datagram(TrafficDirection::ToClient, bytes);
                                last_activity = Instant::now();
                            }
                            Err(error) => {
                                flow.counters.record_drop();
                                if error.kind() == io::ErrorKind::InvalidInput {
                                    warn!(flow_id = flow.id, datagram_bytes = bytes, %error, "UDP reply dropped: TUN MTU would be exceeded");
                                    last_activity = Instant::now();
                                } else {
                                    flow.counters.record_socket_error();
                                    reason = "TUN response send error";
                                    warn!(flow_id = flow.id, %error, "UDP response could not be returned to TUN");
                                    break;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        flow.counters.record_socket_error();
                        context.interfaces.report_path_error(flow.interface_index, &error);
                        reason = "outbound receive error";
                        warn!(flow_id = flow.id, %error, "UDP outbound receive failed");
                        break;
                    }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    reason = "idle timeout";
                    break;
                }
            }
        }
        reason
    } else {
        let (receiver, sender) = inbound.into_datagram_halves();
        let socket = Arc::new(socket);
        run_udp_shaped(
            receiver,
            sender,
            socket,
            context.clone(),
            flow.clone(),
            qos,
            idle_timeout,
        )
        .await
    };

    flow.set_state(FlowState::Closing);
    flow.set_state(if end_reason == "inbound flow closed" {
        FlowState::Closed
    } else if end_reason.contains("error") {
        FlowState::Failed
    } else {
        FlowState::Idle
    });
    let (datagrams_to_network, datagrams_to_client) = flow.counters.datagram_counts();
    let (bytes_to_network, bytes_to_client) = flow.counters.byte_counts();
    info!(
        flow_id = flow.id,
        %source,
        %destination,
        interface_index = flow.interface_index,
        outbound_local = ?outbound_local,
        state = ?flow.state(),
        duration_ms = flow.counters.duration().as_millis() as u64,
        datagrams_to_network,
        datagrams_to_client,
        bytes_to_network,
        bytes_to_client,
        dropped_datagrams = flow.counters.dropped_datagrams(),
        socket_errors = flow.counters.socket_errors(),
        end_reason,
        "UDP flow ended"
    );
    context.flows.remove(key, flow.id).await;
}

async fn run_udp_shaped(
    mut receiver: ipstack::IpStackUdpReceiver,
    sender: ipstack::IpStackUdpSender,
    socket: Arc<tokio::net::UdpSocket>,
    context: Arc<EngineContext>,
    flow: Arc<FlowRecord>,
    qos: QosSession,
    idle_timeout: std::time::Duration,
) -> &'static str {
    let last_activity = Arc::new(tokio::sync::Mutex::new(Instant::now()));
    let upload = async {
        loop {
            let payload = match receiver.recv_datagram().await {
                Ok(Some(payload)) => payload,
                Ok(None) => return "inbound flow closed",
                Err(error) => {
                    flow.counters.record_socket_error();
                    warn!(flow_id = flow.id, %error, "UDP inbound receive failed");
                    return "inbound receive error";
                }
            };
            if payload.len() > context.udp_max_datagram_bytes {
                flow.counters.record_drop();
                qos.record_udp_drop(QosDirection::Upload);
                continue;
            }
            let Some(_permit) = (match qos.acquire_udp(QosDirection::Upload, payload.len()).await {
                Ok(permit) => permit,
                Err(error) => {
                    flow.counters.record_socket_error();
                    warn!(flow_id = flow.id, %error, "UDP upload shaping failed");
                    return "upload shaping error";
                }
            }) else {
                flow.counters.record_drop();
                continue;
            };
            match socket.send(&payload).await {
                Ok(bytes) if bytes == payload.len() => {
                    flow.counters
                        .record_datagram(TrafficDirection::ToNetwork, bytes);
                    *last_activity.lock().await = Instant::now();
                }
                Ok(bytes) => {
                    flow.counters.record_drop();
                    flow.counters.record_socket_error();
                    warn!(
                        flow_id = flow.id,
                        sent_bytes = bytes,
                        expected_bytes = payload.len(),
                        "UDP request send was unexpectedly partial"
                    );
                }
                Err(error) => {
                    flow.counters.record_drop();
                    flow.counters.record_socket_error();
                    context
                        .interfaces
                        .report_path_error(flow.interface_index, &error);
                    warn!(flow_id = flow.id, %error, "UDP outbound send failed");
                    return "outbound send error";
                }
            }
        }
    };
    let download = async {
        let mut buffer = vec![0u8; 65_535];
        loop {
            let bytes = match socket.recv(&mut buffer).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    flow.counters.record_socket_error();
                    context
                        .interfaces
                        .report_path_error(flow.interface_index, &error);
                    warn!(flow_id = flow.id, %error, "UDP outbound receive failed");
                    return "outbound receive error";
                }
            };
            if bytes > context.udp_max_datagram_bytes {
                flow.counters.record_drop();
                qos.record_udp_drop(QosDirection::Download);
                *last_activity.lock().await = Instant::now();
                continue;
            }
            let Some(_permit) = (match qos.acquire_udp(QosDirection::Download, bytes).await {
                Ok(permit) => permit,
                Err(error) => {
                    flow.counters.record_socket_error();
                    warn!(flow_id = flow.id, %error, "UDP download shaping failed");
                    return "download shaping error";
                }
            }) else {
                flow.counters.record_drop();
                continue;
            };
            match sender.send_datagram(&buffer[..bytes]) {
                Ok(()) => {
                    flow.counters
                        .record_datagram(TrafficDirection::ToClient, bytes);
                    *last_activity.lock().await = Instant::now();
                }
                Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                    flow.counters.record_drop();
                    qos.record_udp_drop(QosDirection::Download);
                    *last_activity.lock().await = Instant::now();
                }
                Err(error) => {
                    flow.counters.record_drop();
                    flow.counters.record_socket_error();
                    warn!(flow_id = flow.id, %error, "UDP response could not be returned to TUN");
                    return "TUN response send error";
                }
            }
        }
    };
    tokio::pin!(upload, download);
    let mut check_idle = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        tokio::select! {
            reason = &mut upload => break reason,
            reason = &mut download => break reason,
            _ = check_idle.tick() => {
                if last_activity.lock().await.elapsed() >= idle_timeout { break "idle timeout"; }
            }
        }
    }
}
