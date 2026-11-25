use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};

use clap::ValueEnum;
use serde::Serialize;

use crate::{
    interface::{InterfaceHealth, InterfacePool},
    telemetry::{InterfaceMetrics, TelemetryStore},
    transport::TransportProtocol,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
pub enum SchedulerMode {
    #[default]
    RoundRobin,
    Adaptive,
    AdaptiveShadow,
    Weighted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficClass {
    Interactive,
    Bulk,
    Balanced,
    Dns,
}

#[derive(Clone, Copy, Debug)]
pub struct FlowMetadata {
    pub protocol: TransportProtocol,
    pub source: SocketAddr,
    pub destination: SocketAddr,
    pub traffic_class: TrafficClass,
}

impl FlowMetadata {
    pub fn new(protocol: TransportProtocol, source: SocketAddr, destination: SocketAddr) -> Self {
        let traffic_class = if destination.port() == 53 {
            TrafficClass::Dns
        } else {
            TrafficClass::Balanced
        };
        Self {
            protocol,
            source,
            destination,
            traffic_class,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InterfaceCapacity {
    pub download_bps: Option<f64>,
    pub upload_bps: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    pub mode: SchedulerMode,
    pub base_weights: HashMap<u32, f64>,
    pub capacities: HashMap<u32, InterfaceCapacity>,
    pub stable_capacities: HashMap<String, InterfaceCapacity>,
    pub max_metric_age: Duration,
    pub min_probe_samples: u64,
    pub hysteresis_percent: f64,
    pub traffic_rules: Vec<TrafficRule>,
}

#[derive(Clone, Debug)]
pub struct TrafficRule {
    pub protocol: Option<TransportProtocol>,
    pub destination_port: Option<u16>,
    pub destination_cidr: Option<IpCidr>,
    pub traffic_class: TrafficClass,
}

#[derive(Clone, Copy, Debug)]
pub struct IpCidr {
    network: IpAddr,
    prefix: u8,
}

impl IpCidr {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let (address, prefix) = value
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("CIDR must include a prefix length"))?;
        let network: IpAddr = address.parse()?;
        let prefix: u8 = prefix.parse()?;
        let max = if network.is_ipv4() { 32 } else { 128 };
        anyhow::ensure!(prefix <= max, "CIDR prefix must be at most {max}");
        Ok(Self { network, prefix })
    }

    fn contains(self, address: IpAddr) -> bool {
        match (self.network, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - self.prefix)
                };
                u32::from(network) & mask == u32::from(address) & mask
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - self.prefix)
                };
                u128::from(network) & mask == u128::from(address) & mask
            }
            _ => false,
        }
    }
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            mode: SchedulerMode::RoundRobin,
            base_weights: HashMap::new(),
            capacities: HashMap::new(),
            stable_capacities: HashMap::new(),
            max_metric_age: Duration::from_secs(5),
            min_probe_samples: 3,
            hysteresis_percent: 15.0,
            traffic_rules: Vec::new(),
        }
    }
}

fn capacity_observation(
    capacity: InterfaceCapacity,
    metrics: Option<&InterfaceMetrics>,
) -> (Option<f64>, Option<f64>, Option<f64>) {
    let mut directions = Vec::with_capacity(2);
    if let Some(capacity) = capacity.download_bps {
        directions.push((capacity, metrics.map(|m| m.download_bps * 8.0)));
    }
    if let Some(capacity) = capacity.upload_bps {
        directions.push((capacity, metrics.map(|m| m.upload_bps * 8.0)));
    }
    if directions.is_empty() {
        return (None, None, None);
    }
    let count = directions.len() as f64;
    let average_capacity = directions
        .iter()
        .map(|(capacity, _)| *capacity)
        .sum::<f64>()
        / count;
    if directions.iter().any(|(_, load)| load.is_none()) {
        return (Some(average_capacity), None, None);
    }
    let average_load = directions
        .iter()
        .map(|(_, load)| load.unwrap_or_default())
        .sum::<f64>()
        / count;
    let average_headroom = directions
        .iter()
        .map(|(capacity, load)| (capacity - load.unwrap_or_default()).max(0.0))
        .sum::<f64>()
        / count;
    (
        Some(average_capacity),
        Some(average_headroom),
        Some(average_load),
    )
}

#[derive(Clone, Debug, Serialize)]
pub struct CandidateScore {
    pub interface_index: u32,
    pub score: f64,
    pub reason: String,
    pub confidence: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SelectionDecision {
    pub mode: SchedulerMode,
    pub traffic_class: TrafficClass,
    pub address_family: String,
    pub actual_interface: u32,
    pub recommended_interface: u32,
    pub adaptive_used: bool,
    pub fallback: bool,
    pub reason: String,
    pub candidates: Vec<CandidateScore>,
}

pub struct FlowScheduler {
    interfaces: Arc<InterfacePool>,
    telemetry: Arc<TelemetryStore>,
    config: SchedulerConfig,
    smooth_current: Mutex<HashMap<u32, f64>>,
    stable_scores: Mutex<HashMap<u32, f64>>,
}

impl FlowScheduler {
    pub fn new(
        interfaces: Arc<InterfacePool>,
        telemetry: Arc<TelemetryStore>,
        config: SchedulerConfig,
    ) -> Self {
        Self {
            interfaces,
            telemetry,
            config,
            smooth_current: Mutex::new(HashMap::new()),
            stable_scores: Mutex::new(HashMap::new()),
        }
    }

    pub fn mode(&self) -> SchedulerMode {
        self.config.mode
    }

    pub fn select(&self, flow: FlowMetadata) -> Option<SelectionDecision> {
        let flow = self.classify(flow);
        if self.config.mode == SchedulerMode::RoundRobin {
            let selected = self.interfaces.next_available()?;
            return Some(SelectionDecision {
                mode: SchedulerMode::RoundRobin,
                traffic_class: flow.traffic_class,
                address_family: if flow.source.is_ipv4() {
                    "IPv4"
                } else {
                    "IPv6"
                }
                .to_owned(),
                actual_interface: selected,
                recommended_interface: selected,
                adaptive_used: false,
                fallback: false,
                reason: "round-robin baseline".to_owned(),
                candidates: Vec::new(),
            });
        }
        let round_robin = match self.config.mode {
            SchedulerMode::AdaptiveShadow => self.interfaces.next_available(),
            _ => None,
        };
        let snapshot = self.telemetry.snapshot();
        let candidates = self
            .interfaces
            .configured_indexes()
            .into_iter()
            .filter_map(|index| {
                let health = self.interfaces.health(index)?;
                if health == InterfaceHealth::Unavailable
                    || (self.config.mode != SchedulerMode::Adaptive
                        && health == InterfaceHealth::Recovering)
                {
                    return None;
                }
                if let Some(metrics) = snapshot
                    .interfaces
                    .get(&index)
                    .filter(|m| m.counter_age() <= self.config.max_metric_age)
                    && !metrics.operational
                {
                    return None;
                }
                Some((index, health))
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return None;
        }

        let scored = self.score_candidates(flow, &candidates, &snapshot.interfaces);
        let weighted = self.pick_smooth(&scored);
        let metrics_fallback = scored.iter().all(|s| s.confidence < 0.25);
        let (actual, recommendation, fallback, reason) = match self.config.mode {
            SchedulerMode::RoundRobin => unreachable!("handled above"),
            SchedulerMode::Weighted => {
                let selected = weighted?;
                (
                    selected,
                    selected,
                    false,
                    "static weighted round-robin".to_owned(),
                )
            }
            SchedulerMode::Adaptive => {
                let selected = weighted?;
                (
                    selected,
                    selected,
                    metrics_fallback,
                    if metrics_fallback {
                        "adaptive metrics unavailable; static weights used"
                    } else {
                        "adaptive weighted flow score"
                    }
                    .to_owned(),
                )
            }
            SchedulerMode::AdaptiveShadow => (
                round_robin?,
                weighted?,
                metrics_fallback,
                if metrics_fallback {
                    "shadow: actual assignment remains round-robin; adaptive recommendation uses configured weights".to_owned()
                } else {
                    "shadow: actual assignment remains round-robin".to_owned()
                },
            ),
        };
        Some(SelectionDecision {
            mode: self.config.mode,
            traffic_class: flow.traffic_class,
            address_family: if flow.source.is_ipv4() {
                "IPv4"
            } else {
                "IPv6"
            }
            .to_owned(),
            actual_interface: actual,
            recommended_interface: recommendation,
            adaptive_used: self.config.mode == SchedulerMode::Adaptive,
            fallback,
            reason,
            candidates: scored,
        })
    }

    fn classify(&self, mut flow: FlowMetadata) -> FlowMetadata {
        for rule in &self.config.traffic_rules {
            if rule.protocol.is_none_or(|p| p == flow.protocol)
                && rule
                    .destination_port
                    .is_none_or(|port| port == flow.destination.port())
                && rule
                    .destination_cidr
                    .is_none_or(|cidr| cidr.contains(flow.destination.ip()))
            {
                flow.traffic_class = rule.traffic_class;
                return flow;
            }
        }
        flow
    }

    fn score_candidates(
        &self,
        flow: FlowMetadata,
        candidates: &[(u32, InterfaceHealth)],
        metrics: &HashMap<u32, InterfaceMetrics>,
    ) -> Vec<CandidateScore> {
        let mut raw = Vec::with_capacity(candidates.len());
        let mut max_headroom: f64 = 0.0;
        for (index, health) in candidates {
            let metric = metrics
                .get(index)
                .filter(|m| m.counter_age() <= self.config.max_metric_age && m.counter_samples > 0);
            let capacity = metric
                .and_then(|metric| self.config.stable_capacities.get(&metric.guid))
                .or_else(|| self.config.capacities.get(index))
                .copied()
                .unwrap_or_default();
            let (estimated_capacity, headroom, load) = capacity_observation(capacity, metric);
            max_headroom = max_headroom.max(headroom.unwrap_or(0.0));
            raw.push((
                *index,
                *health,
                capacity,
                metric,
                metrics.get(index),
                estimated_capacity,
                headroom,
                load,
            ));
        }

        raw.into_iter()
            .map(
                |(
                    index,
                    health,
                    _capacity,
                    metric,
                    probe_metric,
                    estimated_capacity,
                    headroom,
                    load,
                )| {
                    let base = self
                        .config
                        .base_weights
                        .get(&index)
                        .copied()
                        .unwrap_or(100.0)
                        .clamp(1.0, 1000.0);
                    let mut utility = 1.0;
                    let mut confidence: f64 = 0.0;
                    let mut reasons = Vec::new();
                    if self.config.mode != SchedulerMode::Weighted
                        && let Some(capacity_bps) = estimated_capacity
                    {
                        let cap_confidence = if metric.is_some() { 0.8 } else { 0.2 };
                        confidence = cap_confidence;
                        let headroom_quality = if max_headroom > 0.0 {
                            headroom.unwrap_or(max_headroom * 0.5) / max_headroom
                        } else {
                            0.5
                        };
                        let congestion_quality = load
                            .map(|load| {
                                (1.0 - (load / capacity_bps.max(1.0)).clamp(0.0, 1.0))
                                    .clamp(0.0, 1.0)
                            })
                            .unwrap_or(0.5);
                        reasons.push(format!(
                            "configured average directional headroom {}",
                            headroom
                                .map(|value| format!("{:.1} Mbps", value / 1_000_000.0))
                                .unwrap_or_else(|| "unknown".to_owned())
                        ));
                        utility *= match flow.traffic_class {
                            TrafficClass::Bulk => {
                                0.25 + 0.55 * headroom_quality + 0.20 * congestion_quality
                            }
                            TrafficClass::Interactive | TrafficClass::Dns => {
                                0.40 + 0.20 * headroom_quality + 0.40 * congestion_quality
                            }
                            TrafficClass::Balanced => {
                                0.30 + 0.45 * headroom_quality + 0.25 * congestion_quality
                            }
                        };
                    } else if self.config.mode != SchedulerMode::Weighted {
                        reasons.push("capacity unknown; configured weight fallback".to_owned());
                    }
                    if self.config.mode != SchedulerMode::Weighted
                        && let Some(m) = probe_metric
                        && let (Some(latency), Some(age)) = (m.latency_ms, m.successful_probe_age())
                        && age <= self.config.max_metric_age
                        && m.probe_sample_count() as u64 >= self.config.min_probe_samples
                    {
                        confidence = confidence.max(0.65);
                        let latency_quality = (1.0 / (1.0 + latency / 100.0)).clamp(0.0, 1.0);
                        let jitter_quality = m
                            .jitter_ms
                            .map(|j| (1.0 / (1.0 + j / 50.0)).clamp(0.0, 1.0))
                            .unwrap_or(0.7);
                        let loss_quality = 1.0 - m.probe_loss_ratio.unwrap_or(0.0).clamp(0.0, 1.0);
                        let network_quality = match flow.traffic_class {
                            TrafficClass::Interactive | TrafficClass::Dns => {
                                0.55 * latency_quality + 0.25 * jitter_quality + 0.20 * loss_quality
                            }
                            TrafficClass::Bulk => {
                                0.15 * latency_quality + 0.10 * jitter_quality + 0.75 * loss_quality
                            }
                            TrafficClass::Balanced => {
                                0.35 * latency_quality + 0.20 * jitter_quality + 0.45 * loss_quality
                            }
                        };
                        utility *= 0.55 + 0.45 * network_quality;
                        reasons.push(format!(
                            "TCP connect probe {:.1} ms, jitter {:.1} ms, loss {:.1}%",
                            latency,
                            m.jitter_ms.unwrap_or(0.0),
                            100.0 * m.probe_loss_ratio.unwrap_or(0.0)
                        ));
                    }
                    if self.config.mode != SchedulerMode::Weighted
                        && let Some(error_ratio) = metric.and_then(|m| m.recent_error_discard_ratio)
                    {
                        let penalty = (error_ratio * 10.0).clamp(0.0, 0.5);
                        utility *= 1.0 - penalty;
                        confidence = confidence.max(0.4);
                        reasons.push(format!(
                            "recent interface error/discard ratio {:.3}%",
                            error_ratio * 100.0
                        ));
                    }
                    if health == InterfaceHealth::Degraded {
                        utility *= 0.5;
                        reasons.push("interface marked degraded".to_owned());
                    } else if health == InterfaceHealth::Recovering {
                        utility *= 0.25;
                        reasons.push("recovering path ramp-up".to_owned());
                    }
                    CandidateScore {
                        interface_index: index,
                        score: self.stabilize_score(index, (base * utility).max(0.001)),
                        reason: reasons.join("; "),
                        confidence,
                    }
                },
            )
            .collect()
    }

    fn pick_smooth(&self, candidates: &[CandidateScore]) -> Option<u32> {
        if candidates.is_empty() {
            return None;
        }
        let mut current = self
            .smooth_current
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        current.retain(|index, _| {
            candidates
                .iter()
                .any(|candidate| candidate.interface_index == *index)
        });
        let total: f64 = candidates.iter().map(|c| c.score).sum();
        let mut winner = None;
        let mut winning_value = f64::NEG_INFINITY;
        for candidate in candidates {
            let value = current.entry(candidate.interface_index).or_default();
            *value += candidate.score;
            if *value > winning_value {
                winning_value = *value;
                winner = Some(candidate.interface_index);
            }
        }
        if let Some(index) = winner {
            *current.entry(index).or_default() -= total;
        }
        winner
    }

    fn stabilize_score(&self, index: u32, score: f64) -> f64 {
        let mut previous = self.stable_scores.lock().unwrap_or_else(|e| e.into_inner());
        let old = previous.get(&index).copied();
        let stable = old
            .filter(|old| {
                let delta = (score - old).abs() / old.max(0.001) * 100.0;
                delta < self.config.hysteresis_percent
            })
            .unwrap_or(score);
        previous.insert(index, stable);
        stable
    }

    pub fn explain_snapshot(&self) -> Vec<CandidateScore> {
        let snapshot = self.telemetry.snapshot();
        self.score_candidates(
            FlowMetadata::new(
                TransportProtocol::Tcp,
                "0.0.0.0:0".parse().unwrap(),
                "0.0.0.0:443".parse().unwrap(),
            ),
            &self
                .interfaces
                .configured_indexes()
                .into_iter()
                .filter_map(|i| Some((i, self.interfaces.health(i)?)))
                .collect::<Vec<_>>(),
            &snapshot.interfaces,
        )
    }
}

pub fn start_probe_tasks(
    interfaces: Arc<InterfacePool>,
    target: SocketAddr,
    store: Arc<TelemetryStore>,
    interval: Duration,
    timeout_duration: Duration,
) -> Vec<tokio::task::JoinHandle<()>> {
    interfaces
        .configured_guids()
        .into_iter()
        .map(|guid| {
            let store = store.clone();
            let interfaces = interfaces.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(interval).await;
                    let Some(index) = interfaces.current_index_for_guid(&guid) else {
                        continue;
                    };
                    let started = tokio::time::Instant::now();
                    let result = tokio::time::timeout(
                        timeout_duration,
                        crate::interface::connect_on_interface(target, index),
                    )
                    .await;
                    match result {
                        Ok(Ok(_stream)) => store.record_probe(index, Some(started.elapsed())),
                        Ok(Err(_)) | Err(_) => store.record_probe(index, None),
                    }
                    tokio::time::sleep(interval).await;
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::RawInterface;

    fn scheduler(mode: SchedulerMode, weights: &[(u32, f64)]) -> FlowScheduler {
        let pool = Arc::new(
            InterfacePool::new(
                weights.iter().map(|(i, _)| *i).collect(),
                Duration::from_secs(1),
            )
            .unwrap(),
        );
        let config = SchedulerConfig {
            mode,
            base_weights: weights.iter().copied().collect(),
            ..SchedulerConfig::default()
        };
        FlowScheduler::new(pool, Arc::new(TelemetryStore::default()), config)
    }

    fn flow(port: u16) -> FlowMetadata {
        FlowMetadata::new(
            TransportProtocol::Tcp,
            "192.0.2.1:50000".parse().unwrap(),
            format!("198.51.100.1:{port}").parse().unwrap(),
        )
    }

    #[test]
    fn round_robin_remains_default_fallback() {
        let scheduler = scheduler(SchedulerMode::RoundRobin, &[(8, 100.0), (4, 100.0)]);
        let selected: Vec<_> = (0..4)
            .map(|_| scheduler.select(flow(443)).unwrap().actual_interface)
            .collect();
        assert_eq!(selected, vec![8, 4, 8, 4]);
    }

    #[test]
    fn smooth_weighted_round_robin_tracks_configured_weights() {
        let scheduler = scheduler(SchedulerMode::Weighted, &[(8, 2.0), (4, 1.0)]);
        let decisions: Vec<_> = (0..30)
            .map(|_| scheduler.select(flow(443)).unwrap().actual_interface)
            .collect();
        let wifi = decisions.iter().filter(|i| **i == 8).count();
        assert_eq!(wifi, 20);
    }

    #[test]
    fn shadow_calculates_recommendation_but_routes_round_robin() {
        let scheduler = scheduler(SchedulerMode::AdaptiveShadow, &[(8, 1.0), (4, 3.0)]);
        let first = scheduler.select(flow(443)).unwrap();
        assert_eq!(first.actual_interface, 8);
        assert_eq!(first.recommended_interface, 4);
        assert!(!first.adaptive_used);
    }

    #[test]
    fn dns_classification_is_port_based_and_no_payload_is_needed() {
        assert_eq!(flow(53).traffic_class, TrafficClass::Dns);
        assert_eq!(flow(443).traffic_class, TrafficClass::Balanced);
    }

    #[test]
    fn cidr_traffic_rule_matches_destination_and_uses_first_match_priority() {
        let pool = Arc::new(InterfacePool::new(vec![8], Duration::from_secs(1)).unwrap());
        let config = SchedulerConfig {
            mode: SchedulerMode::AdaptiveShadow,
            traffic_rules: vec![
                TrafficRule {
                    protocol: Some(TransportProtocol::Tcp),
                    destination_port: None,
                    destination_cidr: Some(IpCidr::parse("203.0.113.0/24").unwrap()),
                    traffic_class: TrafficClass::Bulk,
                },
                TrafficRule {
                    protocol: None,
                    destination_port: Some(443),
                    destination_cidr: None,
                    traffic_class: TrafficClass::Interactive,
                },
            ],
            ..SchedulerConfig::default()
        };
        let scheduler = FlowScheduler::new(pool, Arc::new(TelemetryStore::default()), config);
        let decision = scheduler
            .select(FlowMetadata::new(
                TransportProtocol::Tcp,
                "192.0.2.2:50000".parse().unwrap(),
                "203.0.113.42:443".parse().unwrap(),
            ))
            .unwrap();
        assert_eq!(decision.traffic_class, TrafficClass::Bulk);
    }

    #[test]
    fn bulk_shifts_new_flows_from_saturated_path_to_available_headroom() {
        let telemetry = Arc::new(TelemetryStore::default());
        let row = |index| RawInterface {
            index,
            luid: u64::from(index),
            guid: format!("guid-{index}"),
            alias: format!("adapter-{index}"),
            operational: true,
            mtu: 1500,
            transmit_link_bps: 1_000_000_000,
            receive_link_bps: 1_000_000_000,
            in_octets: 0,
            out_octets: 0,
            in_errors: 0,
            out_errors: 0,
            in_discards: 0,
            out_discards: 0,
            in_packets: 0,
            out_packets: 0,
        };
        telemetry.update_counters(vec![row(8), row(4)], Duration::from_secs(1), 0.35);
        telemetry.update_counters(
            vec![
                RawInterface {
                    in_octets: 11_250_000,
                    ..row(8)
                }, // ~90 Mbps
                RawInterface {
                    in_octets: 1_250_000,
                    ..row(4)
                }, // ~10 Mbps
            ],
            Duration::from_secs(1),
            0.35,
        );
        let pool = Arc::new(InterfacePool::new(vec![8, 4], Duration::from_secs(1)).unwrap());
        let config = SchedulerConfig {
            mode: SchedulerMode::Adaptive,
            capacities: HashMap::from([
                (
                    8,
                    InterfaceCapacity {
                        download_bps: Some(100_000_000.0),
                        upload_bps: None,
                    },
                ),
                (
                    4,
                    InterfaceCapacity {
                        download_bps: Some(60_000_000.0),
                        upload_bps: None,
                    },
                ),
            ]),
            traffic_rules: vec![TrafficRule {
                protocol: Some(TransportProtocol::Tcp),
                destination_port: Some(443),
                destination_cidr: None,
                traffic_class: TrafficClass::Bulk,
            }],
            ..SchedulerConfig::default()
        };
        let scheduler = FlowScheduler::new(pool, telemetry, config);
        let decision = scheduler.select(flow(443)).unwrap();
        assert_eq!(decision.actual_interface, 4);
        assert_eq!(decision.traffic_class, TrafficClass::Bulk);
    }

    #[test]
    fn interactive_flows_prefer_lower_latency_after_probe_samples() {
        let telemetry = Arc::new(TelemetryStore::default());
        let row = |index| RawInterface {
            index,
            luid: u64::from(index),
            guid: format!("guid-{index}"),
            alias: format!("adapter-{index}"),
            operational: true,
            mtu: 1500,
            transmit_link_bps: 0,
            receive_link_bps: 0,
            in_octets: 0,
            out_octets: 0,
            in_errors: 0,
            out_errors: 0,
            in_discards: 0,
            out_discards: 0,
            in_packets: 0,
            out_packets: 0,
        };
        telemetry.update_counters(vec![row(8), row(4)], Duration::from_secs(1), 0.35);
        for _ in 0..4 {
            telemetry.record_probe(8, Some(Duration::from_millis(20)));
            telemetry.record_probe(4, Some(Duration::from_millis(80)));
        }
        let pool = Arc::new(InterfacePool::new(vec![8, 4], Duration::from_secs(1)).unwrap());
        let config = SchedulerConfig {
            mode: SchedulerMode::Adaptive,
            traffic_rules: vec![TrafficRule {
                protocol: None,
                destination_port: Some(443),
                destination_cidr: None,
                traffic_class: TrafficClass::Interactive,
            }],
            ..SchedulerConfig::default()
        };
        let scheduler = FlowScheduler::new(pool, telemetry, config);
        let decision = scheduler.select(flow(443)).unwrap();
        assert_eq!(decision.actual_interface, 8);
        assert!(decision.candidates[0].reason.contains("TCP connect probe"));
    }

    #[test]
    fn stale_metrics_fall_back_to_configured_weights_and_do_not_claim_capacity() {
        let telemetry = Arc::new(TelemetryStore::default());
        let pool = Arc::new(InterfacePool::new(vec![8, 4], Duration::from_secs(1)).unwrap());
        let config = SchedulerConfig {
            mode: SchedulerMode::Adaptive,
            max_metric_age: Duration::ZERO,
            ..SchedulerConfig::default()
        };
        let scheduler = FlowScheduler::new(pool, telemetry, config);
        let decision = scheduler.select(flow(443)).unwrap();
        assert!(decision.fallback);
        assert!(
            decision
                .candidates
                .iter()
                .all(|score| score.reason.contains("capacity unknown"))
        );
    }

    #[test]
    fn scheduler_selection_overhead_simulation_reports_1k_10k_100k() {
        let scheduler = scheduler(SchedulerMode::Adaptive, &[(8, 100.0), (4, 100.0)]);
        let mut report = Vec::new();
        for count in [1_000usize, 10_000, 100_000] {
            let mut samples = Vec::with_capacity(count);
            for _ in 0..count {
                let started = std::time::Instant::now();
                scheduler.select(flow(443)).unwrap();
                samples.push(started.elapsed().as_nanos());
            }
            samples.sort_unstable();
            let avg = samples.iter().sum::<u128>() as f64 / count as f64;
            let p95 = samples[count * 95 / 100];
            let p99 = samples[count * 99 / 100];
            report.push(format!("{count}: avg={avg:.0}ns p95={p95}ns p99={p99}ns"));
        }
        println!("scheduler overhead simulation: {}", report.join("; "));
    }
}
