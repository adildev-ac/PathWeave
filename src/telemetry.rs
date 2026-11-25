use std::{
    collections::{HashMap, VecDeque},
    io,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::{task::JoinHandle, time::MissedTickBehavior};
use tracing::warn;

type AddressSnapshot = HashMap<u32, (Vec<String>, Vec<String>)>;
type GatewaySnapshot = HashMap<u32, Vec<String>>;

#[derive(Clone, Debug)]
pub struct InterfaceMetrics {
    pub index: u32,
    pub luid: u64,
    pub guid: String,
    pub alias: String,
    pub operational: bool,
    pub ipv4_addresses: Vec<String>,
    pub ipv6_addresses: Vec<String>,
    pub default_gateways: Vec<String>,
    pub mtu: u32,
    /// Physical/media link rates from Windows. These are diagnostic only and are
    /// never treated as internet capacity.
    pub transmit_link_bps: u64,
    pub receive_link_bps: u64,
    pub in_octets: u64,
    pub out_octets: u64,
    pub in_errors: u64,
    pub out_errors: u64,
    pub in_discards: u64,
    pub out_discards: u64,
    in_packets: u64,
    out_packets: u64,
    pub recent_error_discard_ratio: Option<f64>,
    pub download_bps: f64,
    pub upload_bps: f64,
    pub peak_download_bps: f64,
    pub peak_upload_bps: f64,
    pub counter_samples: u64,
    pub throughput_history: VecDeque<ThroughputSample>,
    pub counter_reset: bool,
    pub latency_ms: Option<f64>,
    pub min_latency_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
    pub probe_loss_ratio: Option<f64>,
    pub probe_successes: u64,
    pub probe_failures: u64,
    pub consecutive_probe_failures: u32,
    sampled_at: Instant,
    last_probe_at: Option<Instant>,
    last_probe_success_at: Option<Instant>,
    previous_latency_ms: Option<f64>,
    probe_outcomes: VecDeque<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ThroughputSample {
    pub sampled_at_unix_ms: u64,
    /// Observed adapter byte rate, not an estimate of available link capacity.
    pub upload_bytes_per_sec: f64,
    /// Observed adapter byte rate, not an estimate of available link capacity.
    pub download_bytes_per_sec: f64,
}

impl InterfaceMetrics {
    fn from_raw(raw: RawInterface) -> Self {
        Self {
            index: raw.index,
            luid: raw.luid,
            guid: raw.guid,
            alias: raw.alias,
            operational: raw.operational,
            ipv4_addresses: raw.ipv4_addresses,
            ipv6_addresses: raw.ipv6_addresses,
            default_gateways: raw.default_gateways,
            mtu: raw.mtu,
            transmit_link_bps: raw.transmit_link_bps,
            receive_link_bps: raw.receive_link_bps,
            in_octets: raw.in_octets,
            out_octets: raw.out_octets,
            in_errors: raw.in_errors,
            out_errors: raw.out_errors,
            in_discards: raw.in_discards,
            out_discards: raw.out_discards,
            in_packets: raw.in_packets,
            out_packets: raw.out_packets,
            recent_error_discard_ratio: None,
            download_bps: 0.0,
            upload_bps: 0.0,
            peak_download_bps: 0.0,
            peak_upload_bps: 0.0,
            counter_samples: 0,
            throughput_history: VecDeque::with_capacity(60),
            counter_reset: false,
            latency_ms: None,
            min_latency_ms: None,
            jitter_ms: None,
            probe_loss_ratio: None,
            probe_successes: 0,
            probe_failures: 0,
            consecutive_probe_failures: 0,
            sampled_at: Instant::now(),
            last_probe_at: None,
            last_probe_success_at: None,
            previous_latency_ms: None,
            probe_outcomes: VecDeque::with_capacity(60),
        }
    }

    pub fn counter_age(&self) -> Duration {
        self.sampled_at.elapsed()
    }

    pub fn probe_age(&self) -> Option<Duration> {
        self.last_probe_at.map(|at| at.elapsed())
    }

    pub fn successful_probe_age(&self) -> Option<Duration> {
        self.last_probe_success_at.map(|at| at.elapsed())
    }

    pub fn probe_sample_count(&self) -> usize {
        self.probe_outcomes.len()
    }

    fn sample(&mut self, raw: &RawInterface, elapsed: Duration, alpha: f64) {
        self.counter_reset = raw.in_octets < self.in_octets
            || raw.out_octets < self.out_octets
            || raw.index != self.index
            || raw.luid != self.luid;
        if !self.counter_reset && self.guid == raw.guid {
            let packet_delta = raw.in_packets.saturating_sub(self.in_packets)
                + raw.out_packets.saturating_sub(self.out_packets);
            let error_delta = raw.in_errors.saturating_sub(self.in_errors)
                + raw.out_errors.saturating_sub(self.out_errors)
                + raw.in_discards.saturating_sub(self.in_discards)
                + raw.out_discards.saturating_sub(self.out_discards);
            self.recent_error_discard_ratio = (packet_delta > 0)
                .then_some((error_delta as f64 / packet_delta as f64).clamp(0.0, 1.0));
        } else {
            self.recent_error_discard_ratio = None;
        }
        if !self.counter_reset && !elapsed.is_zero() && self.guid == raw.guid {
            let seconds = elapsed.as_secs_f64();
            let download = (raw.in_octets - self.in_octets) as f64 / seconds;
            let upload = (raw.out_octets - self.out_octets) as f64 / seconds;
            if self.counter_samples == 0 {
                self.download_bps = download;
                self.upload_bps = upload;
            } else {
                self.download_bps = alpha * download + (1.0 - alpha) * self.download_bps;
                self.upload_bps = alpha * upload + (1.0 - alpha) * self.upload_bps;
            }
            self.peak_download_bps = self.peak_download_bps.max(download);
            self.peak_upload_bps = self.peak_upload_bps.max(upload);
            self.counter_samples += 1;
            let sampled_at_unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u64::MAX as u128) as u64;
            self.throughput_history.push_back(ThroughputSample {
                sampled_at_unix_ms,
                upload_bytes_per_sec: upload,
                download_bytes_per_sec: download,
            });
            if self.throughput_history.len() > 60 {
                self.throughput_history.pop_front();
            }
        } else if self.counter_reset || self.guid != raw.guid {
            self.download_bps = 0.0;
            self.upload_bps = 0.0;
            self.counter_samples = 0;
        }
        self.index = raw.index;
        self.luid = raw.luid;
        self.guid = raw.guid.clone();
        self.alias = raw.alias.clone();
        self.operational = raw.operational;
        self.ipv4_addresses = raw.ipv4_addresses.clone();
        self.ipv6_addresses = raw.ipv6_addresses.clone();
        self.default_gateways = raw.default_gateways.clone();
        self.mtu = raw.mtu;
        self.transmit_link_bps = raw.transmit_link_bps;
        self.receive_link_bps = raw.receive_link_bps;
        self.in_octets = raw.in_octets;
        self.out_octets = raw.out_octets;
        self.in_errors = raw.in_errors;
        self.out_errors = raw.out_errors;
        self.in_discards = raw.in_discards;
        self.out_discards = raw.out_discards;
        self.in_packets = raw.in_packets;
        self.out_packets = raw.out_packets;
        self.sampled_at = Instant::now();
    }

    fn record_probe(&mut self, latency: Option<Duration>) {
        self.last_probe_at = Some(Instant::now());
        match latency {
            Some(duration) => {
                self.last_probe_success_at = self.last_probe_at;
                let sample = duration.as_secs_f64() * 1000.0;
                if let Some(previous) = self.previous_latency_ms {
                    let diff = (sample - previous).abs();
                    self.jitter_ms =
                        Some(self.jitter_ms.map_or(diff, |old| 0.25 * diff + 0.75 * old));
                }
                self.previous_latency_ms = Some(sample);
                self.latency_ms = Some(
                    self.latency_ms
                        .map_or(sample, |old| 0.25 * sample + 0.75 * old),
                );
                self.min_latency_ms =
                    Some(self.min_latency_ms.map_or(sample, |old| old.min(sample)));
                self.probe_successes += 1;
                self.consecutive_probe_failures = 0;
            }
            None => {
                self.probe_failures += 1;
                self.consecutive_probe_failures += 1;
            }
        }
        self.probe_outcomes.push_back(latency.is_none());
        if self.probe_outcomes.len() > 60 {
            self.probe_outcomes.pop_front();
        }
        self.probe_loss_ratio = Some(
            self.probe_outcomes.iter().filter(|failed| **failed).count() as f64
                / self.probe_outcomes.len() as f64,
        );
    }
}

#[derive(Clone, Debug, Default)]
pub struct TelemetrySnapshot {
    pub interfaces: HashMap<u32, InterfaceMetrics>,
}

#[derive(Default)]
pub struct TelemetryStore {
    current: RwLock<Arc<TelemetrySnapshot>>,
}

impl TelemetryStore {
    pub fn snapshot(&self) -> Arc<TelemetrySnapshot> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn update_counters(&self, raw: Vec<RawInterface>, elapsed: Duration, alpha: f64) {
        let mut snapshot = (*self.snapshot()).clone();
        let mut present = HashMap::with_capacity(raw.len());
        for item in raw {
            let previous = snapshot.interfaces.remove(&item.index).or_else(|| {
                let old_index = snapshot
                    .interfaces
                    .iter()
                    .find(|(_, metrics)| metrics.guid == item.guid)
                    .map(|(index, _)| *index)?;
                snapshot.interfaces.remove(&old_index)
            });
            let mut metrics = match previous {
                Some(mut previous) if previous.guid == item.guid => {
                    previous.sample(&item, elapsed, alpha);
                    previous
                }
                _ => InterfaceMetrics::from_raw(item.clone()),
            };
            metrics.index = item.index;
            present.insert(item.index, metrics);
        }
        snapshot.interfaces = present;
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(snapshot);
    }

    pub fn record_probe(&self, index: u32, latency: Option<Duration>) {
        let mut snapshot = (*self.snapshot()).clone();
        if let Some(metrics) = snapshot.interfaces.get_mut(&index) {
            metrics.record_probe(latency);
        }
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(snapshot);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RawInterface {
    pub index: u32,
    pub luid: u64,
    pub guid: String,
    pub alias: String,
    pub operational: bool,
    pub ipv4_addresses: Vec<String>,
    pub ipv6_addresses: Vec<String>,
    pub default_gateways: Vec<String>,
    pub mtu: u32,
    pub transmit_link_bps: u64,
    pub receive_link_bps: u64,
    pub in_octets: u64,
    pub out_octets: u64,
    pub in_errors: u64,
    pub out_errors: u64,
    pub in_discards: u64,
    pub out_discards: u64,
    pub in_packets: u64,
    pub out_packets: u64,
}

pub fn start_counter_collector(
    store: Arc<TelemetryStore>,
    interval: Duration,
    alpha: f64,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(interval);
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut previous_sample = Instant::now();
        loop {
            timer.tick().await;
            let elapsed = previous_sample.elapsed();
            previous_sample = Instant::now();
            match tokio::task::spawn_blocking(read_interface_counters).await {
                Ok(Ok(rows)) => store.update_counters(rows, elapsed, alpha),
                Ok(Err(error)) => warn!(%error, "interface telemetry sample failed"),
                Err(error) => warn!(%error, "interface telemetry collector task failed"),
            }
        }
    })
}

pub async fn sample_interfaces_once() -> io::Result<Vec<InterfaceDiagnostics>> {
    let rows = tokio::task::spawn_blocking(read_interface_counters)
        .await
        .map_err(io::Error::other)??;
    let store = TelemetryStore::default();
    store.update_counters(rows, Duration::ZERO, 0.35);
    Ok(diagnostics(&store.snapshot()))
}

#[derive(Serialize)]
pub struct InterfaceDiagnostics {
    pub index: u32,
    pub luid: u64,
    pub stable_guid: String,
    pub name: String,
    pub operational: bool,
    pub ipv4_addresses: Vec<String>,
    pub ipv6_addresses: Vec<String>,
    pub default_gateways: Vec<String>,
    pub mtu: u32,
    pub transmit_link_mbps: f64,
    pub receive_link_mbps: f64,
    pub download_mbps: f64,
    pub upload_mbps: f64,
    pub peak_download_mbps: f64,
    pub peak_upload_mbps: f64,
    pub in_errors: u64,
    pub out_errors: u64,
    pub in_discards: u64,
    pub out_discards: u64,
    pub recent_error_discard_ratio: Option<f64>,
    pub counter_age_ms: u64,
    pub counter_samples: u64,
    pub observed_throughput_history: Vec<ThroughputSample>,
    pub counter_reset: bool,
    pub latency_ms: Option<f64>,
    pub min_latency_ms: Option<f64>,
    pub jitter_ms: Option<f64>,
    pub probe_loss_ratio: Option<f64>,
    pub probe_successes: u64,
    pub probe_failures: u64,
    pub probe_age_ms: Option<u64>,
    pub last_successful_probe_age_ms: Option<u64>,
}

pub fn diagnostics(snapshot: &TelemetrySnapshot) -> Vec<InterfaceDiagnostics> {
    let mut rows: Vec<_> = snapshot
        .interfaces
        .values()
        .map(|m| InterfaceDiagnostics {
            index: m.index,
            luid: m.luid,
            stable_guid: m.guid.clone(),
            name: m.alias.clone(),
            operational: m.operational,
            ipv4_addresses: m.ipv4_addresses.clone(),
            ipv6_addresses: m.ipv6_addresses.clone(),
            default_gateways: m.default_gateways.clone(),
            mtu: m.mtu,
            transmit_link_mbps: m.transmit_link_bps as f64 / 1_000_000.0,
            receive_link_mbps: m.receive_link_bps as f64 / 1_000_000.0,
            download_mbps: m.download_bps * 8.0 / 1_000_000.0,
            upload_mbps: m.upload_bps * 8.0 / 1_000_000.0,
            peak_download_mbps: m.peak_download_bps * 8.0 / 1_000_000.0,
            peak_upload_mbps: m.peak_upload_bps * 8.0 / 1_000_000.0,
            in_errors: m.in_errors,
            out_errors: m.out_errors,
            in_discards: m.in_discards,
            out_discards: m.out_discards,
            recent_error_discard_ratio: m.recent_error_discard_ratio,
            counter_age_ms: m.counter_age().as_millis() as u64,
            counter_samples: m.counter_samples,
            observed_throughput_history: m.throughput_history.iter().cloned().collect(),
            counter_reset: m.counter_reset,
            latency_ms: m.latency_ms,
            min_latency_ms: m.min_latency_ms,
            jitter_ms: m.jitter_ms,
            probe_loss_ratio: m.probe_loss_ratio,
            probe_successes: m.probe_successes,
            probe_failures: m.probe_failures,
            probe_age_ms: m.probe_age().map(|age| age.as_millis() as u64),
            last_successful_probe_age_ms: m
                .successful_probe_age()
                .map(|age| age.as_millis() as u64),
        })
        .collect();
    rows.sort_by_key(|row| row.index);
    rows
}

#[cfg(windows)]
fn read_interface_counters() -> io::Result<Vec<RawInterface>> {
    use std::slice;

    use windows_sys::Win32::NetworkManagement::{
        IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_ROW2, MIB_IF_TABLE2},
        Ndis::IfOperStatusUp,
    };

    // SAFETY: GetIfTable2 fills a system-allocated table; rows are copied before
    // FreeMibTable releases that allocation. The flexible array starts at Table.
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        let result = GetIfTable2(&mut table);
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        if table.is_null() {
            return Err(io::Error::other("GetIfTable2 returned a null table"));
        }
        let count = (*table).NumEntries as usize;
        let rows = slice::from_raw_parts(
            std::ptr::addr_of!((*table).Table).cast::<MIB_IF_ROW2>(),
            count,
        );
        let copied = rows
            .iter()
            .map(|row: &MIB_IF_ROW2| RawInterface {
                index: row.InterfaceIndex,
                luid: row.InterfaceLuid.Value,
                guid: format_guid(row.InterfaceGuid),
                alias: decode_utf16(&row.Alias),
                operational: row.OperStatus == IfOperStatusUp,
                ipv4_addresses: Vec::new(),
                ipv6_addresses: Vec::new(),
                default_gateways: Vec::new(),
                mtu: row.Mtu,
                transmit_link_bps: row.TransmitLinkSpeed,
                receive_link_bps: row.ReceiveLinkSpeed,
                in_octets: row.InOctets,
                out_octets: row.OutOctets,
                in_errors: row.InErrors,
                out_errors: row.OutErrors,
                in_discards: row.InDiscards,
                out_discards: row.OutDiscards,
                in_packets: row.InUcastPkts + row.InNUcastPkts,
                out_packets: row.OutUcastPkts + row.OutNUcastPkts,
            })
            .collect();
        FreeMibTable(table.cast());
        let mut copied: Vec<RawInterface> = copied;
        match read_address_routes() {
            Ok((addresses, gateways)) => {
                for row in &mut copied {
                    if let Some((ipv4, ipv6)) = addresses.get(&row.index) {
                        row.ipv4_addresses = ipv4.clone();
                        row.ipv6_addresses = ipv6.clone();
                    }
                    row.default_gateways = gateways.get(&row.index).cloned().unwrap_or_default();
                }
            }
            Err(error) => warn!(%error, "interface address and default-route refresh failed"),
        }
        Ok(copied)
    }
}

#[cfg(windows)]
fn read_address_routes() -> io::Result<(AddressSnapshot, GatewaySnapshot)> {
    use std::{
        net::{IpAddr, Ipv4Addr, Ipv6Addr},
        slice,
    };
    use windows_sys::Win32::{
        NetworkManagement::IpHelper::{
            FreeMibTable, GetIpForwardTable2, GetUnicastIpAddressTable, MIB_IPFORWARD_ROW2,
            MIB_IPFORWARD_TABLE2, MIB_UNICASTIPADDRESS_ROW, MIB_UNICASTIPADDRESS_TABLE,
        },
        Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_INET},
    };

    unsafe fn as_ip(address: SOCKADDR_INET) -> Option<IpAddr> {
        match unsafe { address.si_family } {
            AF_INET => {
                let bytes = unsafe { address.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes() };
                Some(IpAddr::V4(Ipv4Addr::from(bytes)))
            }
            AF_INET6 => {
                let bytes = unsafe { address.Ipv6.sin6_addr.u.Byte };
                Some(IpAddr::V6(Ipv6Addr::from(bytes)))
            }
            _ => None,
        }
    }

    unsafe {
        let mut address_table: *mut MIB_UNICASTIPADDRESS_TABLE = std::ptr::null_mut();
        let result = GetUnicastIpAddressTable(AF_UNSPEC, &mut address_table);
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        if address_table.is_null() {
            return Err(io::Error::other("GetUnicastIpAddressTable returned null"));
        }
        let address_rows = slice::from_raw_parts(
            std::ptr::addr_of!((*address_table).Table).cast::<MIB_UNICASTIPADDRESS_ROW>(),
            (*address_table).NumEntries as usize,
        );
        let mut addresses: HashMap<u32, (Vec<String>, Vec<String>)> = HashMap::new();
        for row in address_rows {
            let Some(ip) = as_ip(row.Address) else {
                continue;
            };
            let entry = addresses.entry(row.InterfaceIndex).or_default();
            match ip {
                IpAddr::V4(ip) => entry.0.push(format!("{ip}/{}", row.OnLinkPrefixLength)),
                IpAddr::V6(ip) => entry.1.push(format!("{ip}/{}", row.OnLinkPrefixLength)),
            }
        }
        FreeMibTable(address_table.cast());

        let mut route_table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
        let result = GetIpForwardTable2(AF_UNSPEC, &mut route_table);
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        if route_table.is_null() {
            return Err(io::Error::other("GetIpForwardTable2 returned null"));
        }
        let route_rows = slice::from_raw_parts(
            std::ptr::addr_of!((*route_table).Table).cast::<MIB_IPFORWARD_ROW2>(),
            (*route_table).NumEntries as usize,
        );
        let mut gateways: HashMap<u32, Vec<String>> = HashMap::new();
        for row in route_rows {
            if row.DestinationPrefix.PrefixLength != 0 {
                continue;
            }
            if let Some(next_hop) = as_ip(row.NextHop).filter(|ip| !ip.is_unspecified()) {
                let list = gateways.entry(row.InterfaceIndex).or_default();
                let value = next_hop.to_string();
                if !list.contains(&value) {
                    list.push(value);
                }
            }
        }
        FreeMibTable(route_table.cast());
        Ok((addresses, gateways))
    }
}

#[cfg(windows)]
fn format_guid(guid: windows_sys::core::GUID) -> String {
    let d4 = guid.data4;
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        guid.data1, guid.data2, guid.data3, d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]
    )
}

#[cfg(windows)]
fn decode_utf16(value: &[u16]) -> String {
    let end = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}

#[cfg(not(windows))]
fn read_interface_counters() -> io::Result<Vec<RawInterface>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "GetIfTable2 telemetry is implemented on Windows only",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(index: u32, guid: &str, down: u64, up: u64) -> RawInterface {
        RawInterface {
            index,
            luid: u64::from(index),
            guid: guid.to_owned(),
            alias: format!("adapter-{index}"),
            operational: true,
            ipv4_addresses: Vec::new(),
            ipv6_addresses: Vec::new(),
            default_gateways: Vec::new(),
            mtu: 1500,
            transmit_link_bps: 1_000_000_000,
            receive_link_bps: 1_000_000_000,
            in_octets: down,
            out_octets: up,
            in_errors: 0,
            out_errors: 0,
            in_discards: 0,
            out_discards: 0,
            in_packets: 0,
            out_packets: 0,
        }
    }

    #[test]
    fn counter_rates_use_elapsed_time_and_ema() {
        let store = TelemetryStore::default();
        store.update_counters(
            vec![raw(4, "wifi-guid", 100, 200)],
            Duration::from_secs(1),
            0.5,
        );
        store.update_counters(
            vec![raw(4, "wifi-guid", 1100, 2200)],
            Duration::from_secs(1),
            0.5,
        );
        let first = store.snapshot();
        assert_eq!(first.interfaces[&4].download_bps, 1000.0);
        assert_eq!(first.interfaces[&4].upload_bps, 2000.0);
        store.update_counters(
            vec![raw(4, "wifi-guid", 3100, 6200)],
            Duration::from_secs(1),
            0.5,
        );
        let second = store.snapshot();
        assert_eq!(second.interfaces[&4].download_bps, 1500.0);
        assert_eq!(second.interfaces[&4].upload_bps, 3000.0);
    }

    #[test]
    fn counter_reset_and_index_change_do_not_create_fake_rates() {
        let store = TelemetryStore::default();
        store.update_counters(
            vec![raw(4, "wifi-guid", 1000, 2000)],
            Duration::from_secs(1),
            0.5,
        );
        store.update_counters(
            vec![raw(4, "wifi-guid", 10, 20)],
            Duration::from_secs(1),
            0.5,
        );
        assert!(store.snapshot().interfaces[&4].counter_reset);
        store.update_counters(
            vec![raw(7, "wifi-guid", 20, 40)],
            Duration::from_secs(1),
            0.5,
        );
        let snapshot = store.snapshot();
        assert!(!snapshot.interfaces.contains_key(&4));
        assert_eq!(snapshot.interfaces[&7].guid, "wifi-guid");
        assert_eq!(snapshot.interfaces[&7].download_bps, 0.0);
    }

    #[test]
    fn probe_failures_do_not_invent_latency() {
        let store = TelemetryStore::default();
        store.update_counters(
            vec![raw(4, "wifi-guid", 100, 200)],
            Duration::from_secs(1),
            0.5,
        );
        store.record_probe(4, None);
        let metrics = store.snapshot();
        assert_eq!(metrics.interfaces[&4].latency_ms, None);
        assert_eq!(metrics.interfaces[&4].probe_loss_ratio, Some(1.0));
    }

    #[test]
    fn interface_errors_and_discards_are_reported_as_recent_packet_ratio() {
        let store = TelemetryStore::default();
        let mut first = raw(4, "wifi-guid", 0, 0);
        first.in_packets = 100;
        first.out_packets = 100;
        store.update_counters(vec![first.clone()], Duration::from_secs(1), 0.35);
        first.in_packets += 100;
        first.out_packets += 100;
        first.in_errors = 1;
        first.out_discards = 1;
        store.update_counters(vec![first], Duration::from_secs(1), 0.35);
        assert_eq!(
            store.snapshot().interfaces[&4].recent_error_discard_ratio,
            Some(0.01)
        );
    }

    #[test]
    fn address_and_gateway_snapshots_refresh_and_throughput_history_is_bounded() {
        let store = TelemetryStore::default();
        let mut initial = raw(8, "wifi-guid", 0, 0);
        initial.ipv4_addresses = vec!["192.0.2.10/24".into()];
        initial.default_gateways = vec!["192.0.2.1".into()];
        store.update_counters(vec![initial], Duration::from_secs(1), 0.35);
        for sample in 1..=75 {
            let mut next = raw(8, "wifi-guid", sample * 10_000, sample * 20_000);
            next.ipv4_addresses = vec!["192.0.2.11/24".into()];
            next.ipv6_addresses = vec!["2001:db8::10/64".into()];
            next.default_gateways = vec!["192.0.2.254".into()];
            store.update_counters(vec![next], Duration::from_secs(1), 0.35);
        }
        let metrics = store.snapshot();
        let iface = &metrics.interfaces[&8];
        assert_eq!(iface.ipv4_addresses, vec!["192.0.2.11/24"]);
        assert_eq!(iface.ipv6_addresses, vec!["2001:db8::10/64"]);
        assert_eq!(iface.default_gateways, vec!["192.0.2.254"]);
        assert_eq!(iface.throughput_history.len(), 60);
        assert_eq!(
            iface
                .throughput_history
                .back()
                .unwrap()
                .upload_bytes_per_sec,
            20_000.0
        );
    }

    #[test]
    fn changed_index_preserves_guid_history_without_counting_a_fake_rate() {
        let store = TelemetryStore::default();
        store.update_counters(vec![raw(8, "wifi-guid", 0, 0)], Duration::ZERO, 0.35);
        store.update_counters(
            vec![raw(8, "wifi-guid", 1000, 2000)],
            Duration::from_secs(1),
            0.35,
        );
        assert_eq!(store.snapshot().interfaces[&8].throughput_history.len(), 1);
        store.update_counters(
            vec![raw(11, "wifi-guid", 1500, 2500)],
            Duration::from_secs(1),
            0.35,
        );
        let snapshot = store.snapshot();
        assert!(!snapshot.interfaces.contains_key(&8));
        assert_eq!(snapshot.interfaces[&11].throughput_history.len(), 1);
        assert_eq!(snapshot.interfaces[&11].download_bps, 0.0);
        assert!(snapshot.interfaces[&11].counter_reset);
    }
}
