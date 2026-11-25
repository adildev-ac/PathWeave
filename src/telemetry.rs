use std::{
    collections::{HashMap, VecDeque},
    io,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::{task::JoinHandle, time::MissedTickBehavior};
use tracing::warn;

#[derive(Clone, Debug)]
pub struct InterfaceMetrics {
    pub index: u32,
    pub luid: u64,
    pub guid: String,
    pub alias: String,
    pub operational: bool,
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

impl InterfaceMetrics {
    fn from_raw(raw: RawInterface) -> Self {
        Self {
            index: raw.index,
            luid: raw.luid,
            guid: raw.guid,
            alias: raw.alias,
            operational: raw.operational,
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
        self.counter_reset = raw.in_octets < self.in_octets || raw.out_octets < self.out_octets;
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
            let mut metrics = match snapshot.interfaces.remove(&item.index) {
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
        Ok(copied)
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
}
