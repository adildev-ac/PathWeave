mod adaptive;
mod config;
mod flow;
mod health_control;
mod interface;
mod qos;
mod qos_control;
mod relay;
mod scheduler;
mod tcp_transport;
mod telemetry;
mod transport;
mod udp;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use adaptive::{AdaptiveConfig, AdaptiveController, AdaptiveObservation};
use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser, parser::ValueSource};
use flow::FlowManager;
use interface::InterfacePool;
use ipstack::{IpStack, IpStackConfig, IpStackStream};
use qos::{
    InterfaceRates, QosClass, QosConfig, QosEngine, QosMode, QosPolicyManager, UdpAqmConfig,
};
use scheduler::{
    FlowScheduler, InterfaceCapacity, IpCidr, SchedulerConfig, SchedulerMode, TrafficClass,
    TrafficRule,
};
use telemetry::{
    TelemetryStore, diagnostics as telemetry_diagnostics, sample_interfaces_once,
    start_counter_collector,
};
use tokio::{sync::Semaphore, task::JoinSet, time::timeout};
use tracing::{debug, error, info, warn};
use transport::{EngineContext, TransportEngine};

#[derive(Debug, Parser)]
#[command(
    name = "pathweave-engine",
    about = "Pathweave TUN TCP/UDP forwarding and interface selection prototype"
)]
struct Args {
    /// Versioned TOML configuration file.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Validate the selected TOML configuration and exit without opening TUN.
    #[arg(long, default_value_t = false)]
    validate_config: bool,

    /// Migrate the selected TOML configuration atomically, creating a .bak first.
    #[arg(long, default_value_t = false)]
    migrate_config: bool,

    /// Physical interface indexes to test, in rotation (Get-NetAdapter output).
    #[arg(long, value_delimiter = ',', required = false)]
    interface: Vec<u32>,

    /// Print one Windows IP Helper adapter snapshot as JSON and exit.
    #[arg(long, default_value_t = false)]
    telemetry_once: bool,

    /// TUN IPv4 address; use a dedicated test prefix.
    #[arg(long, default_value = "198.18.0.1")]
    tun_address: String,

    /// TUN IPv4 netmask.
    #[arg(long, default_value = "255.254.0.0")]
    tun_netmask: String,

    /// TUN MTU.
    #[arg(long, default_value_t = 1500)]
    mtu: u16,

    /// Maximum simultaneous TCP relay flows.
    #[arg(long, default_value_t = 512)]
    max_connections: usize,

    /// Enable the UDP forwarding engine (disabled by default).
    #[arg(long, default_value_t = false)]
    enable_udp: bool,

    /// Maximum simultaneous UDP sessions.
    #[arg(long, default_value_t = 2048)]
    max_udp_sessions: usize,

    /// Maximum queued inbound UDP datagrams per flow.
    #[arg(long, default_value_t = 8)]
    udp_queue_capacity: usize,

    /// Maximum UDP payload bytes accepted from or returned to the TUN flow.
    #[arg(long, default_value_t = 1472)]
    udp_max_datagram_bytes: usize,

    /// Close an idle UDP flow after this many seconds.
    #[arg(long, default_value_t = 120)]
    udp_idle_timeout_secs: u64,

    /// Timeout for interface-bound outbound TCP connection establishment.
    #[arg(long, default_value_t = 12)]
    connect_timeout_secs: u64,

    /// Quarantine an interface after a definite local path error for this many seconds.
    #[arg(long, default_value_t = 5)]
    health_cooldown_secs: u64,

    /// Enable the development-only interface health control API on a loopback address.
    #[arg(long)]
    health_control_listen: Option<SocketAddr>,

    /// Enable the optional loopback-only runtime QoS policy API.
    #[arg(long)]
    qos_control_listen: Option<SocketAddr>,

    /// New-flow scheduler mode: round-robin, weighted, adaptive, or adaptive-shadow.
    #[arg(long, value_enum, default_value_t = SchedulerMode::RoundRobin)]
    scheduler_mode: SchedulerMode,

    /// Set a base allocation weight as INDEX=WEIGHT; can be repeated.
    #[arg(long = "interface-weight", value_name = "INDEX=WEIGHT")]
    interface_weight: Vec<String>,

    /// Set sustainable internet capacity as INDEX=DOWNLOAD_MBPS:UPLOAD_MBPS; zero means unknown.
    #[arg(long = "interface-capacity-mbps", value_name = "INDEX=DOWN:UP")]
    interface_capacity_mbps: Vec<String>,

    /// Priority-ordered class rule as tcp|udp|any:PORT|CIDR|*=interactive|bulk|balanced|dns.
    #[arg(long = "traffic-rule", value_name = "PROTOCOL:MATCH=CLASS")]
    traffic_rule: Vec<String>,

    /// Optional per-interface TCP connect-duration probe target (INDEX binding is applied).
    #[arg(long)]
    probe_target: Option<SocketAddr>,

    /// Scheduler and interface diagnostics JSON to stdout, sampled once per second.
    #[arg(long, default_value_t = false)]
    diagnostics_json: bool,

    /// Interface statistics snapshot interval in milliseconds.
    #[arg(long, default_value_t = 1000)]
    telemetry_interval_ms: u64,

    /// Throughput EMA alpha (0 exclusive to 1 inclusive).
    #[arg(long, default_value_t = 0.35)]
    telemetry_ema_alpha: f64,

    /// Maximum age in seconds for metrics used by the scheduler.
    #[arg(long, default_value_t = 5)]
    telemetry_max_age_secs: u64,

    /// Hysteresis threshold that ignores smaller relative score changes.
    #[arg(long, default_value_t = 15.0)]
    scheduler_hysteresis_percent: f64,

    /// Minimum successful probe samples before latency/jitter affect scheduling.
    #[arg(long, default_value_t = 3)]
    probe_min_samples: u64,

    /// Probe sampling interval in seconds.
    #[arg(long, default_value_t = 2)]
    probe_interval_secs: u64,

    /// TCP probe connection-establishment timeout in milliseconds.
    #[arg(long, default_value_t = 1500)]
    probe_timeout_ms: u64,

    /// QoS mode: disabled, fixed, adaptive, or monitor-only.
    #[arg(long, value_enum, default_value_t = QosMode::Disabled)]
    qos_mode: QosMode,

    /// Aggregate upload shaping rate in Mbps; zero means unlimited.
    #[arg(long, default_value_t = 0.0)]
    qos_upload_mbps: f64,

    /// Aggregate download shaping rate in Mbps; zero means unlimited.
    #[arg(long, default_value_t = 0.0)]
    qos_download_mbps: f64,

    /// Per-interface cap as INDEX=UPLOAD:DOWNLOAD Mbps; zero means unlimited.
    #[arg(long = "qos-interface-limit", value_name = "INDEX=UPLOAD:DOWNLOAD")]
    qos_interface_limit: Vec<String>,

    /// Stable per-adapter cap as GUID=UPLOAD:DOWNLOAD Mbps; zero means unlimited.
    #[arg(long = "qos-interface-guid-limit", value_name = "GUID=UPLOAD:DOWNLOAD")]
    qos_interface_guid_limit: Vec<String>,

    /// Per-class DRR weight as interactive|dns|best-effort|bulk=WEIGHT.
    #[arg(long = "qos-class-weight", value_name = "CLASS=WEIGHT")]
    qos_class_weight: Vec<String>,

    /// Token-bucket burst duration in milliseconds.
    #[arg(long, default_value_t = 50)]
    qos_burst_ms: u64,

    /// Maximum bytes buffered for one flow.
    #[arg(long, default_value_t = 256)]
    qos_max_flow_queue_kb: usize,

    /// Maximum bytes queued per interface direction, in MiB.
    #[arg(long, default_value_t = 16)]
    qos_max_interface_queue_mb: usize,

    /// Maximum total QoS buffered memory, in MiB.
    #[arg(long, default_value_t = 128)]
    qos_global_memory_mb: usize,

    /// Base DRR quantum in bytes before applying class weights.
    #[arg(long, default_value_t = 1500)]
    qos_quantum_bytes: usize,

    /// Enable optional CoDel-inspired whole-datagram UDP AQM.
    #[arg(long, default_value_t = false)]
    qos_udp_aqm: bool,

    /// Persistent UDP queue delay target for AQM in milliseconds.
    #[arg(long, default_value_t = 5)]
    qos_udp_aqm_target_ms: u64,

    /// UDP AQM persistence and drop-control interval in milliseconds.
    #[arg(long, default_value_t = 100)]
    qos_udp_aqm_interval_ms: u64,

    /// Adaptive QoS target share of explicitly configured interface capacity.
    #[arg(long, default_value_t = 90.0)]
    qos_adaptive_target_percent: f64,

    /// Minimum adaptive shaping rate in Mbps.
    #[arg(long, default_value_t = 1.0)]
    qos_adaptive_min_mbps: f64,

    /// Adaptive controller sampling interval in seconds.
    #[arg(long, default_value_t = 2)]
    qos_adaptive_interval_secs: u64,

    /// Persistent local queue-delay threshold for adaptive decrease.
    #[arg(long, default_value_t = 25)]
    qos_adaptive_target_delay_ms: u64,

    /// Queue-delay threshold for slow recovery.
    #[arg(long, default_value_t = 10)]
    qos_adaptive_recovery_delay_ms: u64,

    /// Multiplicative adaptive rate decrease factor.
    #[arg(long, default_value_t = 0.85)]
    qos_adaptive_decrease_factor: f64,

    /// Additive adaptive increase as a percentage of configured capacity.
    #[arg(long, default_value_t = 5.0)]
    qos_adaptive_increase_percent: f64,

    /// Maximum adaptive rate change per update, in percent.
    #[arg(long, default_value_t = 15.0)]
    qos_adaptive_max_change_percent: f64,

    /// Consecutive congestion and recovery samples required to change rate.
    #[arg(long, default_value_t = 3)]
    qos_adaptive_minimum_samples: u32,

    /// Minimum time between adaptive rate changes.
    #[arg(long, default_value_t = 4)]
    qos_adaptive_cooldown_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let log_filter = tracing_subscriber::EnvFilter::from_default_env();
    if std::env::var("PATHWEAVE_LOG_FORMAT").is_ok_and(|value| value.eq_ignore_ascii_case("json")) {
        tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_ansi(false)
            .with_env_filter(log_filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(log_filter).init();
    }

    let matches = Args::command().get_matches();
    let mut args = Args::from_arg_matches(&matches)?;
    if args.validate_config || args.migrate_config {
        let path = args
            .config
            .as_deref()
            .context("--config is required with --validate-config or --migrate-config")?;
        if args.migrate_config {
            config::migrate_file(path).map_err(anyhow::Error::msg)?;
            println!("migrated {}", path.display());
            return Ok(());
        }
        let loaded = config::ConfigFile::load(path).map_err(anyhow::Error::msg)?;
        println!(
            "valid schema_version={} qos_mode={:?}",
            loaded.config.schema_version, loaded.qos.mode
        );
        return Ok(());
    }
    let loaded_config = args
        .config
        .as_deref()
        .map(config::ConfigFile::load)
        .transpose()
        .map_err(anyhow::Error::msg)?;
    if args.telemetry_once {
        let snapshot = sample_interfaces_once().await?;
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }
    if args.max_connections == 0 || args.max_udp_sessions == 0 {
        bail!("TCP and UDP session limits must be greater than zero");
    }
    if args.connect_timeout_secs == 0 || args.health_cooldown_secs == 0 {
        bail!("connect timeout and health cooldown must be greater than zero");
    }
    if args.udp_idle_timeout_secs == 0 || args.udp_queue_capacity == 0 {
        bail!("UDP idle timeout and queue capacity must be greater than zero");
    }
    if args.telemetry_interval_ms == 0
        || args.telemetry_max_age_secs == 0
        || args.probe_min_samples == 0
        || args.probe_interval_secs == 0
        || args.probe_timeout_ms == 0
    {
        bail!(
            "telemetry and probe intervals, timeouts, and sample counts must be greater than zero"
        );
    }
    if !args.telemetry_ema_alpha.is_finite()
        || args.telemetry_ema_alpha <= 0.0
        || args.telemetry_ema_alpha > 1.0
    {
        bail!("--telemetry-ema-alpha must be greater than 0 and at most 1");
    }
    if !args.scheduler_hysteresis_percent.is_finite()
        || !(0.0..=100.0).contains(&args.scheduler_hysteresis_percent)
    {
        bail!("--scheduler-hysteresis-percent must be between 0 and 100");
    }
    if args.qos_adaptive_interval_secs == 0
        || args.qos_adaptive_target_delay_ms == 0
        || args.qos_adaptive_recovery_delay_ms >= args.qos_adaptive_target_delay_ms
        || !args.qos_adaptive_target_percent.is_finite()
        || !(10.0..=100.0).contains(&args.qos_adaptive_target_percent)
    {
        bail!("adaptive QoS interval, target share, and delay thresholds are invalid");
    }
    let max_udp_payload = usize::from(args.mtu).saturating_sub(28);
    if args.udp_max_datagram_bytes == 0 || args.udp_max_datagram_bytes > max_udp_payload {
        bail!(
            "--udp-max-datagram-bytes must be between 1 and MTU-28 ({} for MTU {})",
            max_udp_payload,
            args.mtu
        );
    }
    if args
        .health_control_listen
        .is_some_and(|address| !address.ip().is_loopback())
    {
        bail!("--health-control-listen must use a loopback IP address");
    }

    let discovered_adapters = sample_interfaces_once().await?;
    if args.interface.is_empty() {
        let cfg = loaded_config.as_ref().context(
            "provide --interface or configure enabled [qos.interfaces] GUID entries in --config",
        )?;
        for guid in cfg
            .qos
            .interfaces
            .iter()
            .filter(|(_, v)| v.enabled)
            .map(|(g, _)| g)
        {
            let adapter = discovered_adapters
                .iter()
                .find(|row| normalize_guid(&row.stable_guid) == normalize_guid(guid))
                .with_context(|| {
                    format!("configured interface GUID {guid} is not currently present")
                })?;
            args.interface.push(adapter.index);
        }
    }
    let interface_indexes = args.interface.clone();
    let identity_by_index = discovered_adapters
        .iter()
        .map(|adapter| {
            (
                adapter.index,
                interface::InterfaceIdentity {
                    guid: adapter.stable_guid.clone(),
                    luid: adapter.luid,
                    index: adapter.index,
                    alias: adapter.name.clone(),
                    operational: adapter.operational,
                },
            )
        })
        .collect::<std::collections::HashMap<_, _>>();
    let configured_identities = interface_indexes
        .iter()
        .map(|index| {
            identity_by_index.get(index).cloned().ok_or_else(|| {
                anyhow::anyhow!("configured interface index {index} was not found in GetIfTable2")
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if let Some(config) = &loaded_config {
        apply_config_overrides(&mut args, config, &matches, &configured_identities)?;
    }
    let mut scheduler_config = SchedulerConfig {
        mode: args.scheduler_mode,
        max_metric_age: Duration::from_secs(args.telemetry_max_age_secs),
        min_probe_samples: args.probe_min_samples,
        hysteresis_percent: args.scheduler_hysteresis_percent,
        ..SchedulerConfig::default()
    };
    scheduler_config.traffic_rules = args
        .traffic_rule
        .iter()
        .map(|rule| parse_traffic_rule(rule))
        .collect::<Result<Vec<_>>>()?;
    for setting in &args.interface_weight {
        let (index, weight) = setting
            .split_once('=')
            .context("--interface-weight must be INDEX=WEIGHT")?;
        let index: u32 = index
            .parse()
            .context("interface weight index must be an integer")?;
        let weight: f64 = weight.parse().context("interface weight must be numeric")?;
        if !interface_indexes.contains(&index)
            || !weight.is_finite()
            || !(1.0..=1000.0).contains(&weight)
        {
            bail!(
                "invalid --interface-weight '{setting}': index must be configured and weight must be 1..=1000"
            );
        }
        scheduler_config.base_weights.insert(index, weight);
    }
    for setting in &args.interface_capacity_mbps {
        let (index, capacities) = setting
            .split_once('=')
            .context("--interface-capacity-mbps must be INDEX=DOWNLOAD:UPLOAD")?;
        let index: u32 = index
            .parse()
            .context("interface capacity index must be an integer")?;
        let (download, upload) = capacities
            .split_once(':')
            .context("capacities must be DOWNLOAD:UPLOAD in Mbps")?;
        let parse_capacity = |value: &str| -> Result<Option<f64>> {
            let mbps: f64 = value.parse().context("capacity must be numeric Mbps")?;
            if !mbps.is_finite() || mbps < 0.0 {
                bail!("capacity must be finite and non-negative");
            }
            Ok((mbps > 0.0).then_some(mbps * 1_000_000.0))
        };
        if !interface_indexes.contains(&index) {
            bail!(
                "invalid --interface-capacity-mbps '{setting}': interface index is not configured"
            );
        }
        scheduler_config.capacities.insert(
            index,
            InterfaceCapacity {
                download_bps: parse_capacity(download)?,
                upload_bps: parse_capacity(upload)?,
            },
        );
        if let Some(identity) = identity_by_index.get(&index) {
            scheduler_config
                .stable_capacities
                .insert(identity.guid.clone(), scheduler_config.capacities[&index]);
        }
    }

    let mut qos_config = QosConfig {
        mode: args.qos_mode,
        aggregate: InterfaceRates {
            upload_bytes_per_sec: parse_qos_mbps(args.qos_upload_mbps)?,
            download_bytes_per_sec: parse_qos_mbps(args.qos_download_mbps)?,
        },
        burst_ms: args.qos_burst_ms,
        max_flow_queue_bytes: args.qos_max_flow_queue_kb.saturating_mul(1024),
        max_interface_queue_bytes: args.qos_max_interface_queue_mb.saturating_mul(1024 * 1024),
        global_memory_limit_bytes: args.qos_global_memory_mb.saturating_mul(1024 * 1024),
        quantum_bytes: args.qos_quantum_bytes,
        udp_aqm: UdpAqmConfig {
            enabled: args.qos_udp_aqm,
            target_delay: Duration::from_millis(args.qos_udp_aqm_target_ms),
            interval: Duration::from_millis(args.qos_udp_aqm_interval_ms),
        },
        ..QosConfig::default()
    };
    for setting in &args.qos_interface_limit {
        let (index, rates) = setting
            .split_once('=')
            .context("--qos-interface-limit must be INDEX=UPLOAD:DOWNLOAD")?;
        let index: u32 = index
            .parse()
            .context("QoS interface index must be an integer")?;
        let (upload, download) = rates
            .split_once(':')
            .context("QoS interface rates must be UPLOAD:DOWNLOAD in Mbps")?;
        if !interface_indexes.contains(&index) {
            bail!("QoS interface index {index} is not present in --interface");
        }
        qos_config.interfaces.insert(
            index,
            InterfaceRates {
                upload_bytes_per_sec: parse_qos_mbps(
                    upload
                        .parse()
                        .context("QoS upload cap must be numeric Mbps")?,
                )?,
                download_bytes_per_sec: parse_qos_mbps(
                    download
                        .parse()
                        .context("QoS download cap must be numeric Mbps")?,
                )?,
            },
        );
        warn!(
            interface_index = index,
            "index-based QoS setting was resolved to the adapter GUID for this run; use --qos-interface-guid-limit for stable identity"
        );
    }
    for setting in &args.qos_interface_guid_limit {
        let (guid, rates) = setting
            .split_once('=')
            .context("--qos-interface-guid-limit must be GUID=UPLOAD:DOWNLOAD")?;
        let normalized = normalize_guid(guid);
        let Some(identity) = configured_identities
            .iter()
            .find(|identity| normalize_guid(&identity.guid) == normalized)
        else {
            bail!("QoS GUID '{guid}' is not one of the configured --interface adapters");
        };
        let (upload, download) = rates
            .split_once(':')
            .context("QoS GUID rates must be UPLOAD:DOWNLOAD in Mbps")?;
        qos_config.interfaces.insert(
            identity.index,
            InterfaceRates {
                upload_bytes_per_sec: parse_qos_mbps(
                    upload
                        .parse()
                        .context("QoS upload cap must be numeric Mbps")?,
                )?,
                download_bytes_per_sec: parse_qos_mbps(
                    download
                        .parse()
                        .context("QoS download cap must be numeric Mbps")?,
                )?,
            },
        );
    }
    for setting in &args.qos_class_weight {
        let (class, weight) = setting
            .split_once('=')
            .context("--qos-class-weight must be CLASS=WEIGHT")?;
        let class = match class.to_ascii_lowercase().as_str() {
            "interactive" => QosClass::Interactive,
            "dns" => QosClass::Dns,
            "best-effort" | "best_effort" => QosClass::BestEffort,
            "bulk" => QosClass::Bulk,
            _ => bail!("QoS class must be interactive, dns, best-effort, or bulk"),
        };
        let weight: u32 = weight
            .parse()
            .context("QoS class weight must be an integer")?;
        qos_config.class_weights.insert(class, weight);
    }
    for (index, rates) in &qos_config.interfaces {
        if let Some(capacity) = scheduler_config.capacities.get(index) {
            for (direction, configured, known_capacity) in [
                ("upload", rates.upload_bytes_per_sec, capacity.upload_bps),
                (
                    "download",
                    rates.download_bytes_per_sec,
                    capacity.download_bps,
                ),
            ] {
                if let (Some(configured), Some(known_capacity)) = (configured, known_capacity)
                    && configured as f64 * 8.0 > known_capacity
                {
                    warn!(
                        interface_index = index,
                        direction,
                        qos_limit_mbps = configured as f64 * 8.0 / 1_000_000.0,
                        configured_capacity_mbps = known_capacity / 1_000_000.0,
                        "QoS limit exceeds configured sustainable interface capacity"
                    );
                }
            }
        }
    }
    let adaptive_config = AdaptiveConfig {
        target_utilization: args.qos_adaptive_target_percent / 100.0,
        decrease_factor: args.qos_adaptive_decrease_factor,
        increase_fraction: args.qos_adaptive_increase_percent / 100.0,
        target_queue_delay: Duration::from_millis(args.qos_adaptive_target_delay_ms),
        recovery_queue_delay: Duration::from_millis(args.qos_adaptive_recovery_delay_ms),
        max_change_fraction: args.qos_adaptive_max_change_percent / 100.0,
        congestion_samples: args.qos_adaptive_minimum_samples,
        recovery_samples: args.qos_adaptive_minimum_samples,
        cooldown: Duration::from_secs(args.qos_adaptive_cooldown_secs),
        ..AdaptiveConfig::default()
    }
    .validate()
    .map_err(anyhow::Error::msg)?;
    let mut adaptive_controllers = Vec::new();
    if args.qos_mode == QosMode::Adaptive {
        let min_requested = parse_qos_mbps(args.qos_adaptive_min_mbps)?
            .context("adaptive minimum rate must be greater than zero")?;
        for index in &interface_indexes {
            let Some(capacity) = scheduler_config.capacities.get(index) else {
                continue;
            };
            let guid = identity_by_index
                .get(index)
                .context("configured interface identity disappeared")?
                .guid
                .clone();
            let configured_interface = loaded_config.as_ref().and_then(|file| {
                file.qos
                    .interfaces
                    .iter()
                    .find(|(configured_guid, _)| {
                        normalize_guid(configured_guid) == normalize_guid(&guid)
                    })
                    .map(|(_, setting)| setting)
            });
            for (direction, capacity_bps) in [
                (qos::QosDirection::Upload, capacity.upload_bps),
                (qos::QosDirection::Download, capacity.download_bps),
            ] {
                let Some(capacity_bps) =
                    capacity_bps.filter(|value| value.is_finite() && *value > 0.0)
                else {
                    continue;
                };
                let capacity_bytes = (capacity_bps / 8.0).floor().max(1.0) as u64;
                let configured_max =
                    qos_config
                        .interfaces
                        .get(index)
                        .and_then(|rates| match direction {
                            qos::QosDirection::Upload => rates.upload_bytes_per_sec,
                            qos::QosDirection::Download => rates.download_bytes_per_sec,
                        });
                let configured_file_max = configured_interface
                    .and_then(|setting| match direction {
                        qos::QosDirection::Upload => setting.maximum_upload_mbps,
                        qos::QosDirection::Download => setting.maximum_download_mbps,
                    })
                    .map(|mbps| parse_qos_mbps(mbps).map(|rate| rate.unwrap_or(capacity_bytes)))
                    .transpose()?;
                let max_rate = configured_max
                    .into_iter()
                    .chain(configured_file_max)
                    .fold(capacity_bytes, u64::min);
                let configured_file_min =
                    configured_interface.and_then(|setting| match direction {
                        qos::QosDirection::Upload => setting.minimum_upload_mbps,
                        qos::QosDirection::Download => setting.minimum_download_mbps,
                    });
                let min_rate = configured_file_min
                    .map(|mbps| parse_qos_mbps(mbps).map(|v| v.unwrap_or(min_requested)))
                    .transpose()?
                    .unwrap_or(min_requested);
                if min_rate > max_rate {
                    bail!(
                        "adaptive minimum rate exceeds the configured maximum/capacity for interface {index}"
                    );
                }
                let controller = AdaptiveController::new(
                    adaptive_config,
                    capacity_bytes,
                    min_rate,
                    Some(max_rate),
                )
                .map_err(anyhow::Error::msg)?;
                let initial_rate = controller.current_rate();
                let rates = qos_config.interfaces.entry(*index).or_default();
                match direction {
                    qos::QosDirection::Upload => rates.upload_bytes_per_sec = Some(initial_rate),
                    qos::QosDirection::Download => {
                        rates.download_bytes_per_sec = Some(initial_rate)
                    }
                }
                adaptive_controllers.push((guid.clone(), *index, direction, controller));
            }
        }
        if adaptive_controllers.is_empty() {
            warn!(
                "adaptive QoS has no explicit interface capacity; rate control is inactive until capacity is configured"
            );
        }
    }
    let qos_engine = QosEngine::new(qos_config)?;
    let adaptive_enabled = !adaptive_controllers.is_empty();
    let adaptive_controllers = Arc::new(std::sync::Mutex::new(adaptive_controllers));

    let interfaces = Arc::new(InterfacePool::new_stable(
        configured_identities,
        Duration::from_secs(args.health_cooldown_secs),
    )?);
    let qos_policy = Arc::new(QosPolicyManager::new(
        qos_engine.clone(),
        interfaces.clone(),
    ));
    let flow_manager = Arc::new(FlowManager::default());
    let telemetry = Arc::new(TelemetryStore::default());
    let telemetry_task = start_counter_collector(
        telemetry.clone(),
        Duration::from_millis(args.telemetry_interval_ms),
        args.telemetry_ema_alpha,
    );
    let interface_reconciler = {
        let pool = interfaces.clone();
        let store = telemetry.clone();
        let engine = qos_engine.clone();
        let policy_manager = qos_policy.clone();
        let controllers = adaptive_controllers.clone();
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let snapshot = store.snapshot();
                if snapshot.interfaces.is_empty() {
                    continue;
                }
                let discovered = snapshot
                    .interfaces
                    .values()
                    .map(|metric| interface::InterfaceIdentity {
                        guid: metric.guid.clone(),
                        luid: metric.luid,
                        index: metric.index,
                        alias: metric.alias.clone(),
                        operational: metric.operational,
                    })
                    .collect::<Vec<_>>();
                for change in pool.reconcile(&discovered) {
                    if let Some(new_index) = change.current_index {
                        {
                            let mut controllers =
                                controllers.lock().unwrap_or_else(|e| e.into_inner());
                            for (guid, index, _, _) in controllers.iter_mut() {
                                if normalize_guid(guid) == change.guid {
                                    *index = new_index;
                                }
                            }
                        }
                        let policy = policy_manager.snapshot();
                        let adaptive_rates = {
                            let adaptive = controllers.lock().unwrap_or_else(|e| e.into_inner());
                            [qos::QosDirection::Upload, qos::QosDirection::Download].map(
                                |direction| {
                                    let rate = adaptive
                                        .iter()
                                        .find(|(guid, _, candidate, _)| {
                                            normalize_guid(guid) == change.guid
                                                && *candidate == direction
                                        })
                                        .map(|(_, _, _, controller)| controller.current_rate());
                                    (direction, rate)
                                },
                            )
                        };
                        for (direction, adaptive_rate) in adaptive_rates {
                            let configured_rate = policy
                                .rates
                                .iter()
                                .find(|row| {
                                    normalize_guid(&row.interface_guid) == change.guid
                                        && row.direction == direction
                                })
                                .and_then(|row| row.rate_bytes_per_sec);
                            let rate = adaptive_rate.or(configured_rate);
                            if let Err(error) = engine
                                .update_interface_rate(new_index, direction, rate)
                                .await
                            {
                                warn!(interface_guid = %change.guid, interface_index = new_index, %error, "QoS rate reconciliation failed");
                            }
                        }
                        info!(interface_guid = %change.guid, previous_index = change.previous_index, interface_index = new_index, luid = ?change.luid, "adapter identity reconciled; existing flows remain pinned");
                    } else {
                        warn!(interface_guid = %change.guid, interface_index = change.previous_index, "configured adapter is unavailable; new flows will not use it");
                    }
                }
            }
        }))
    };
    let adaptive_task = if !adaptive_enabled {
        None
    } else {
        let store = telemetry.clone();
        let engine = qos_engine.clone();
        let controllers = adaptive_controllers.clone();
        let policy = qos_policy.clone();
        let sample_age = Duration::from_secs(args.telemetry_max_age_secs.max(2));
        let interval_duration = Duration::from_secs(args.qos_adaptive_interval_secs);
        Some(tokio::spawn(async move {
            let start = tokio::time::Instant::now();
            let mut interval = tokio::time::interval(interval_duration);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let now = start.elapsed();
                let snapshot = store.snapshot();
                let lane_stats = engine.diagnostics();
                let changes = {
                    let mut controllers = controllers.lock().unwrap_or_else(|e| e.into_inner());
                    let mut changes = Vec::new();
                    for (_guid, index, direction, controller) in controllers.iter_mut() {
                        let row = lane_stats.iter().find(|lane| {
                            lane.interface_index == *index && lane.direction == *direction
                        });
                        let queue_fresh = row
                            .and_then(|lane| lane.queue_delay_sample_age_ms)
                            .is_some_and(|age| Duration::from_millis(age) <= sample_age);
                        let metrics = snapshot
                            .interfaces
                            .get(index)
                            .filter(|metric| metric.counter_age() <= sample_age);
                        let errors = metrics.and_then(|metric| metric.recent_error_discard_ratio);
                        let error_fresh = errors.is_some();
                        let fresh = queue_fresh || error_fresh;
                        let observation = AdaptiveObservation {
                            queue_delay: row.filter(|_| queue_fresh).map(|lane| {
                                Duration::from_secs_f64(lane.p95_queue_delay_ms / 1000.0)
                            }),
                            interface_error_ratio: errors,
                            fresh,
                            confidence: if queue_fresh {
                                0.9
                            } else if error_fresh {
                                0.65
                            } else {
                                0.0
                            },
                        };
                        let decision = controller.step(now, observation);
                        if decision.changed {
                            changes.push((normalize_guid(_guid), *index, *direction, decision));
                        }
                    }
                    changes
                };
                for (guid, index, direction, decision) in changes {
                    if let Err(error) = engine
                        .update_interface_rate(index, direction, Some(decision.rate_bytes_per_sec))
                        .await
                    {
                        error!(interface_index = index, direction = ?direction, %error, "adaptive QoS rate update failed");
                    } else {
                        policy
                            .note_adaptive_rate(&guid, direction, decision.rate_bytes_per_sec)
                            .await;
                        info!(interface_index = index, direction = ?direction, rate_mbps = decision.rate_bytes_per_sec as f64 * 8.0 / 1_000_000.0, state = ?decision.state, confidence = decision.confidence, "adaptive QoS rate changed");
                    }
                }
            }
        }))
    };
    let probe_tasks = args
        .probe_target
        .map(|target| {
            scheduler::start_probe_tasks(
                interfaces.clone(),
                target,
                telemetry.clone(),
                Duration::from_secs(args.probe_interval_secs),
                Duration::from_millis(args.probe_timeout_ms),
            )
        })
        .unwrap_or_default();
    let flow_scheduler = Arc::new(FlowScheduler::new(
        interfaces.clone(),
        telemetry.clone(),
        scheduler_config,
    ));
    let context = Arc::new(EngineContext {
        interfaces: interfaces.clone(),
        scheduler: flow_scheduler.clone(),
        qos: qos_engine.clone(),
        flows: flow_manager.clone(),
        tcp_slots: Arc::new(Semaphore::new(args.max_connections)),
        max_tcp_connections: args.max_connections,
        udp_slots: Arc::new(Semaphore::new(args.max_udp_sessions)),
        connect_timeout: Duration::from_secs(args.connect_timeout_secs),
        udp_idle_timeout: Duration::from_secs(args.udp_idle_timeout_secs),
        udp_max_datagram_bytes: args.udp_max_datagram_bytes,
    });
    let health_control_task = if let Some(address) = args.health_control_listen {
        let listener = health_control::bind_listener(address).await?;
        info!(%address, "development-only loopback health control enabled");
        Some(tokio::spawn(health_control::run(
            listener,
            interfaces.clone(),
        )))
    } else {
        None
    };
    let qos_control_task = if let Some(address) = args.qos_control_listen {
        let token = std::env::var("PATHWEAVE_QOS_CONTROL_TOKEN").context(
            "--qos-control-listen requires PATHWEAVE_QOS_CONTROL_TOKEN to protect local mutations",
        )?;
        if token.len() < 32 {
            bail!("PATHWEAVE_QOS_CONTROL_TOKEN must contain at least 32 bytes");
        }
        let listener = qos_control::bind_listener(address).await?;
        info!(%address, "loopback QoS runtime control enabled");
        Some(tokio::spawn(qos_control::run(
            listener,
            qos_policy.clone(),
            interfaces.clone(),
            Arc::new(token),
        )))
    } else {
        None
    };

    let mut tun_config = tun::Configuration::default();
    tun_config
        .tun_name("Pathweave")
        .address(args.tun_address.as_str())
        .netmask(args.tun_netmask.as_str())
        .mtu(args.mtu)
        .up();

    #[cfg(windows)]
    {
        let executable_dir = std::env::current_exe()?
            .parent()
            .context("executable has no parent directory")?
            .to_path_buf();
        let wintun_dll = executable_dir.join("examples").join("wintun.dll");
        tun_config.platform_config(|platform| platform.wintun_file(wintun_dll.into_os_string()));
    }

    let device = tun::create_as_async(&tun_config)
        .context("creating TUN device (Windows requires the Wintun driver and elevation)")?;
    let mut stack_config = IpStackConfig::default();
    stack_config.mtu(args.mtu)?;
    stack_config.udp_timeout(context.udp_idle_timeout);
    stack_config.udp_queue_capacity = args.udp_queue_capacity;
    let mut stack = IpStack::new(stack_config, device);
    let tcp_engine = tcp_transport::TcpTransportEngine;
    let udp_engine = udp::UdpTransportEngine;
    let mut flow_tasks = JoinSet::new();
    let diagnostics_task = if args.diagnostics_json {
        let store = telemetry.clone();
        let scheduler = flow_scheduler.clone();
        let controllers = adaptive_controllers.clone();
        let policy = qos_policy.clone();
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                let output = serde_json::json!({
                    "scheduler_mode": scheduler.mode(),
                    "qos_mode": args.qos_mode,
                    "qos_memory_bytes": qos_engine.current_memory_bytes(),
                    "qos_lanes": qos_engine.diagnostics(),
                    "qos_policy": policy.snapshot(),
                    "adaptive_qos": controllers.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(guid, index, direction, controller)| serde_json::json!({
                        "interface_guid": guid,
                        "interface_index": index,
                        "direction": direction,
                        "effective_rate_bytes_per_sec": controller.current_rate(),
                        "congestion_state": format!("{:?}", controller.state()),
                        "confidence": controller.confidence(),
                    })).collect::<Vec<_>>(),
                    "candidates": scheduler.explain_snapshot(),
                    "interfaces": telemetry_diagnostics(&store.snapshot()),
                });
                println!(
                    "{}",
                    serde_json::to_string(&output).unwrap_or_else(|_| "{}".to_owned())
                );
            }
        }))
    } else {
        None
    };
    debug!(tcp_protocol = ?tcp_engine.protocol(), udp_protocol = ?udp_engine.protocol(), "transport engines registered");
    info!(
        mtu = args.mtu,
        interfaces = ?interfaces.configured_indexes(),
        max_connections = args.max_connections,
        udp_enabled = args.enable_udp,
        max_udp_sessions = args.max_udp_sessions,
        udp_queue_capacity = args.udp_queue_capacity,
        udp_max_datagram_bytes = args.udp_max_datagram_bytes,
        udp_idle_timeout_secs = args.udp_idle_timeout_secs,
        scheduler_mode = ?args.scheduler_mode,
        qos_mode = ?args.qos_mode,
        probe_target = ?args.probe_target,
        connect_timeout_secs = args.connect_timeout_secs,
        "TCP/UDP TUN engine started"
    );

    let mut fatal_error = None;
    let shutdown_signal = tokio::signal::ctrl_c();
    tokio::pin!(shutdown_signal);
    loop {
        tokio::select! {
            signal = &mut shutdown_signal => {
                if let Err(error) = signal {
                    warn!(%error, "failed to listen for Ctrl+C; stopping engine");
                }
                info!("shutdown requested");
                break;
            }
            completed = flow_tasks.join_next(), if !flow_tasks.is_empty() => {
                if let Some(Err(error)) = completed {
                    error!(%error, "transport flow task panicked or was cancelled");
                }
            }
            accepted = stack.accept() => {
                match accepted {
                    Ok(IpStackStream::Tcp(inbound)) => tcp_engine.dispatch(inbound, context.clone(), &mut flow_tasks),
                    Ok(IpStackStream::Udp(inbound)) if args.enable_udp => udp_engine.dispatch(inbound, context.clone(), &mut flow_tasks),
                    Ok(IpStackStream::Udp(inbound)) => {
                        warn!(source = %inbound.local_addr(), destination = %inbound.peer_addr(), "UDP intercepted but forwarding is disabled; enable with --enable-udp");
                        drop(inbound);
                    }
                    Ok(IpStackStream::UnknownTransport(packet)) => {
                        warn!(protocol = ?packet.ip_protocol(), "unsupported IP transport intercepted");
                    }
                    Ok(IpStackStream::UnknownNetwork(packet)) => {
                        warn!(bytes = packet.len(), "unsupported network packet intercepted");
                    }
                    Err(error) => {
                        error!(%error, "userspace stack accept failed");
                        fatal_error = Some(error.to_string());
                        break;
                    }
                }
            }
        }
    }

    drop(stack);
    telemetry_task.abort();
    if let Some(task) = interface_reconciler {
        task.abort();
    }
    if let Some(task) = adaptive_task {
        task.abort();
    }
    for task in probe_tasks {
        task.abort();
    }
    if let Some(task) = diagnostics_task {
        task.abort();
    }
    if let Some(task) = health_control_task {
        task.abort();
    }
    if let Some(task) = qos_control_task {
        task.abort();
    }
    let drain = async {
        while let Some(result) = flow_tasks.join_next().await {
            if let Err(error) = result {
                warn!(%error, "flow task failed during shutdown");
            }
        }
    };
    if timeout(Duration::from_secs(5), drain).await.is_err() {
        warn!("flows did not drain within five seconds; aborting remaining tasks");
        flow_tasks.abort_all();
        while flow_tasks.join_next().await.is_some() {}
    }
    if let Some(error) = fatal_error {
        bail!("userspace stack stopped: {error}");
    }
    info!(
        remaining_flows = flow_manager.active_count().await,
        "TCP/UDP engine stopped"
    );
    Ok(())
}

fn apply_config_overrides(
    args: &mut Args,
    config: &config::ConfigFile,
    matches: &clap::ArgMatches,
    identities: &[interface::InterfaceIdentity],
) -> Result<()> {
    let cli = |id: &str| matches.value_source(id) == Some(ValueSource::CommandLine);
    let q = &config.qos;
    if !cli("qos_mode") {
        args.qos_mode = if !q.enabled {
            QosMode::Disabled
        } else if q.mode == QosMode::Disabled {
            QosMode::Fixed
        } else {
            q.mode
        };
    }
    if !cli("qos_upload_mbps") {
        args.qos_upload_mbps = q.aggregate_upload_mbps;
    }
    if !cli("qos_download_mbps") {
        args.qos_download_mbps = q.aggregate_download_mbps;
    }
    if !cli("qos_burst_ms") {
        args.qos_burst_ms = q.burst_ms;
    }
    if !cli("qos_max_flow_queue_kb") {
        args.qos_max_flow_queue_kb = q.max_flow_queue_kb;
    }
    if !cli("qos_max_interface_queue_mb") {
        args.qos_max_interface_queue_mb = q.max_interface_queue_mb;
    }
    if !cli("qos_global_memory_mb") {
        args.qos_global_memory_mb = q.global_memory_mb;
    }
    if !cli("qos_quantum_bytes") {
        args.qos_quantum_bytes = q.quantum_bytes;
    }
    if !cli("qos_udp_aqm") {
        args.qos_udp_aqm = q.udp_aqm.enabled;
    }
    if !cli("qos_udp_aqm_target_ms") {
        args.qos_udp_aqm_target_ms = q.udp_aqm.target_delay_ms;
    }
    if !cli("qos_udp_aqm_interval_ms") {
        args.qos_udp_aqm_interval_ms = q.udp_aqm.interval_ms;
    }
    if !cli("qos_adaptive_target_percent") {
        args.qos_adaptive_target_percent = q.adaptive.target_utilization_percent;
    }
    if !cli("qos_adaptive_min_mbps") {
        args.qos_adaptive_min_mbps = q.adaptive.minimum_rate_mbps;
    }
    if !cli("qos_adaptive_interval_secs") {
        args.qos_adaptive_interval_secs = q.adaptive.interval_secs;
    }
    if !cli("qos_adaptive_target_delay_ms") {
        args.qos_adaptive_target_delay_ms = q.adaptive.congestion_delay_ms;
    }
    if !cli("qos_adaptive_recovery_delay_ms") {
        args.qos_adaptive_recovery_delay_ms = q.adaptive.recovery_delay_ms;
    }
    if !cli("qos_adaptive_decrease_factor") {
        args.qos_adaptive_decrease_factor = q.adaptive.decrease_factor;
    }
    if !cli("qos_adaptive_increase_percent") {
        args.qos_adaptive_increase_percent = q.adaptive.increase_percent;
    }
    if !cli("qos_adaptive_max_change_percent") {
        args.qos_adaptive_max_change_percent = q.adaptive.maximum_change_percent;
    }
    if !cli("qos_adaptive_minimum_samples") {
        args.qos_adaptive_minimum_samples = q.adaptive.minimum_samples;
    }
    if !cli("qos_adaptive_cooldown_secs") {
        args.qos_adaptive_cooldown_secs = q.adaptive.cooldown_secs;
    }
    if !cli("probe_target") {
        args.probe_target = q
            .probe_target
            .as_deref()
            .map(str::parse)
            .transpose()
            .context("invalid qos.probe_target")?;
    }
    if !cli("qos_class_weight") {
        args.qos_class_weight = q
            .class_weights
            .iter()
            .map(|(class, weight)| format!("{class}={weight}"))
            .collect();
    }
    if !cli("interface_weight") {
        args.interface_weight.clear();
        for identity in identities {
            if let Some(setting) = q.interfaces.get(&identity.guid).or_else(|| {
                q.interfaces
                    .iter()
                    .find(|(guid, _)| normalize_guid(guid) == normalize_guid(&identity.guid))
                    .map(|(_, setting)| setting)
            }) {
                args.interface_weight
                    .push(format!("{}={}", identity.index, setting.weight));
            }
        }
    }
    if !cli("interface_capacity_mbps") {
        args.interface_capacity_mbps.clear();
        for identity in identities {
            if let Some((guid, setting)) = q
                .interfaces
                .iter()
                .find(|(guid, _)| normalize_guid(guid) == normalize_guid(&identity.guid))
            {
                let _ = guid;
                args.interface_capacity_mbps.push(format!(
                    "{}={}:{}",
                    identity.index, setting.capacity_download_mbps, setting.capacity_upload_mbps
                ));
            }
        }
    }
    // TOML GUID entries form the persistent base. Existing CLI index syntax remains a
    // temporary override; explicit GUID limits are applied later and therefore win.
    let cli_indexes: std::collections::HashSet<u32> = if cli("qos_interface_limit") {
        args.qos_interface_limit
            .iter()
            .filter_map(|s| s.split_once('=').and_then(|(i, _)| i.parse().ok()))
            .collect()
    } else {
        std::collections::HashSet::new()
    };
    if !cli("qos_interface_guid_limit") {
        args.qos_interface_guid_limit.clear();
        for identity in identities {
            if cli_indexes.contains(&identity.index) {
                continue;
            }
            if let Some(setting) = q
                .interfaces
                .iter()
                .find(|(guid, _)| normalize_guid(guid) == normalize_guid(&identity.guid))
                .map(|(_, value)| value)
                && setting.enabled
            {
                args.qos_interface_guid_limit.push(format!(
                    "{}={}:{}",
                    identity.guid, setting.fixed_upload_mbps, setting.fixed_download_mbps
                ));
            }
        }
    }
    Ok(())
}

fn parse_qos_mbps(mbps: f64) -> Result<Option<u64>> {
    if !mbps.is_finite() || mbps < 0.0 || mbps > (u64::MAX as f64 * 8.0 / 1_000_000.0) {
        bail!("QoS Mbps rate must be finite and non-negative");
    }
    Ok((mbps > 0.0).then_some((mbps * 1_000_000.0 / 8.0).ceil() as u64))
}

fn normalize_guid(guid: &str) -> String {
    guid.trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .to_ascii_lowercase()
}

fn parse_traffic_rule(spec: &str) -> Result<TrafficRule> {
    let (matcher, class) = spec
        .split_once('=')
        .context("--traffic-rule must be PROTOCOL:PORT|CIDR=CLASS")?;
    let (protocol, selector) = matcher
        .split_once(':')
        .context("traffic rule must include protocol and port")?;
    let protocol = match protocol.to_ascii_lowercase().as_str() {
        "tcp" => Some(transport::TransportProtocol::Tcp),
        "udp" => Some(transport::TransportProtocol::Udp),
        "any" => None,
        _ => bail!("traffic-rule protocol must be tcp, udp, or any"),
    };
    let (destination_port, destination_cidr) = if selector == "*" {
        (None, None)
    } else if selector.contains('/') {
        (None, Some(IpCidr::parse(selector)?))
    } else {
        (
            Some(
                selector
                    .parse()
                    .context("traffic-rule port must be 0..=65535, CIDR, or *")?,
            ),
            None,
        )
    };
    let traffic_class = match class.to_ascii_lowercase().as_str() {
        "interactive" => TrafficClass::Interactive,
        "bulk" => TrafficClass::Bulk,
        "balanced" => TrafficClass::Balanced,
        "dns" => TrafficClass::Dns,
        _ => bail!("traffic-rule class must be interactive, bulk, balanced, or dns"),
    };
    Ok(TrafficRule {
        protocol,
        destination_port,
        destination_cidr,
        traffic_class,
    })
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn traffic_rule_parser_accepts_protocol_port_and_cidr() {
        let port_rule = parse_traffic_rule("udp:443=balanced").unwrap();
        assert_eq!(port_rule.protocol, Some(transport::TransportProtocol::Udp));
        assert_eq!(port_rule.destination_port, Some(443));
        assert_eq!(port_rule.traffic_class, TrafficClass::Balanced);

        let ipv6_rule = parse_traffic_rule("any:2001:db8::/32=bulk").unwrap();
        assert!(ipv6_rule.destination_cidr.is_some());
        let ipv4_rule = parse_traffic_rule("tcp:203.0.113.0/24=bulk").unwrap();
        assert!(ipv4_rule.destination_cidr.is_some());
    }

    #[test]
    fn traffic_rule_parser_rejects_invalid_capacity_or_class_shapes() {
        assert!(parse_traffic_rule("udp:abc=bulk").is_err());
        assert!(parse_traffic_rule("tcp:443=unknown").is_err());
        assert!(parse_traffic_rule("any:203.0.113.1/99=bulk").is_err());
    }

    #[test]
    fn toml_values_are_defaults_and_explicit_cli_values_win() {
        let matches = Args::command()
            .try_get_matches_from(["pathweave", "--interface", "8", "--qos-upload-mbps", "3"])
            .unwrap();
        let mut args = Args::from_arg_matches(&matches).unwrap();
        let mut config = config::ConfigFile::default();
        config.qos.enabled = true;
        config.qos.mode = QosMode::Fixed;
        config.qos.aggregate_upload_mbps = 9.0;
        config.qos.aggregate_download_mbps = 4.0;
        let identity = interface::InterfaceIdentity {
            guid: "01234567-89ab-cdef-0123-456789abcdef".into(),
            luid: 1,
            index: 8,
            alias: "Wi-Fi".into(),
            operational: true,
        };
        apply_config_overrides(&mut args, &config, &matches, &[identity]).unwrap();
        assert_eq!(args.qos_mode, QosMode::Fixed);
        assert_eq!(args.qos_upload_mbps, 3.0);
        assert_eq!(args.qos_download_mbps, 4.0);
    }
}
