use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::qos::QosMode;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(default)]
    pub config: ConfigMeta,
    #[serde(default)]
    pub qos: QosFile,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfigMeta {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
}

impl Default for ConfigMeta {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct QosFile {
    pub enabled: bool,
    pub mode: QosMode,
    pub aggregate_upload_mbps: f64,
    pub aggregate_download_mbps: f64,
    pub burst_ms: u64,
    pub max_flow_queue_kb: usize,
    pub max_interface_queue_mb: usize,
    pub global_memory_mb: usize,
    pub quantum_bytes: usize,
    pub adaptive: AdaptiveFile,
    pub udp_aqm: AqmFile,
    pub class_weights: BTreeMap<String, u32>,
    pub interfaces: BTreeMap<String, InterfaceFile>,
    pub probe_target: Option<String>,
}

impl Default for QosFile {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: QosMode::Disabled,
            aggregate_upload_mbps: 0.0,
            aggregate_download_mbps: 0.0,
            burst_ms: 50,
            max_flow_queue_kb: 256,
            max_interface_queue_mb: 16,
            global_memory_mb: 128,
            quantum_bytes: 1500,
            adaptive: AdaptiveFile::default(),
            udp_aqm: AqmFile::default(),
            class_weights: BTreeMap::from([
                ("interactive".into(), 8),
                ("dns".into(), 10),
                ("best_effort".into(), 4),
                ("bulk".into(), 2),
            ]),
            interfaces: BTreeMap::new(),
            probe_target: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AdaptiveFile {
    pub target_utilization_percent: f64,
    pub minimum_rate_mbps: f64,
    pub interval_secs: u64,
    pub congestion_delay_ms: u64,
    pub recovery_delay_ms: u64,
    pub decrease_factor: f64,
    pub increase_percent: f64,
    pub minimum_samples: u32,
    pub maximum_change_percent: f64,
    pub cooldown_secs: u64,
}

impl Default for AdaptiveFile {
    fn default() -> Self {
        Self {
            target_utilization_percent: 90.0,
            minimum_rate_mbps: 1.0,
            interval_secs: 2,
            congestion_delay_ms: 25,
            recovery_delay_ms: 10,
            decrease_factor: 0.85,
            increase_percent: 5.0,
            minimum_samples: 3,
            maximum_change_percent: 15.0,
            cooldown_secs: 4,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AqmFile {
    pub enabled: bool,
    pub target_delay_ms: u64,
    pub interval_ms: u64,
}

impl Default for AqmFile {
    fn default() -> Self {
        Self {
            enabled: false,
            target_delay_ms: 5,
            interval_ms: 100,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct InterfaceFile {
    pub enabled: bool,
    pub weight: f64,
    pub capacity_upload_mbps: f64,
    pub capacity_download_mbps: f64,
    pub fixed_upload_mbps: f64,
    pub fixed_download_mbps: f64,
    pub minimum_upload_mbps: Option<f64>,
    pub minimum_download_mbps: Option<f64>,
    pub maximum_upload_mbps: Option<f64>,
    pub maximum_download_mbps: Option<f64>,
}

impl Default for InterfaceFile {
    fn default() -> Self {
        Self {
            enabled: true,
            weight: 1.0,
            capacity_upload_mbps: 0.0,
            capacity_download_mbps: 0.0,
            fixed_upload_mbps: 0.0,
            fixed_download_mbps: 0.0,
            minimum_upload_mbps: None,
            minimum_download_mbps: None,
            maximum_upload_mbps: None,
            maximum_download_mbps: None,
        }
    }
}

impl ConfigFile {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut value: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
        let version = value
            .get("config")
            .and_then(|v| v.get("schema_version"))
            .and_then(toml::Value::as_integer)
            .unwrap_or(0);
        match version {
            0 => {
                if let Some(meta) = value.get_mut("config") {
                    if let Some(table) = meta.as_table_mut() {
                        table.insert("schema_version".into(), toml::Value::Integer(1));
                    }
                } else {
                    let mut meta = toml::map::Map::new();
                    meta.insert("schema_version".into(), toml::Value::Integer(1));
                    value
                        .as_table_mut()
                        .ok_or("top level must be a table")?
                        .insert("config".into(), toml::Value::Table(meta));
                }
            }
            1 => {}
            other => {
                return Err(format!(
                    "unsupported configuration schema_version {other}; supported versions: 0, 1"
                ));
            }
        }
        let config: Self = value
            .try_into()
            .map_err(|e: toml::de::Error| e.to_string())?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.config.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(format!(
                "configuration must be migrated to schema version {CURRENT_SCHEMA_VERSION}"
            ));
        }
        let q = &self.qos;
        for (name, value) in [
            ("aggregate_upload_mbps", q.aggregate_upload_mbps),
            ("aggregate_download_mbps", q.aggregate_download_mbps),
            (
                "target_utilization_percent",
                q.adaptive.target_utilization_percent,
            ),
            ("minimum_rate_mbps", q.adaptive.minimum_rate_mbps),
            ("decrease_factor", q.adaptive.decrease_factor),
            ("increase_percent", q.adaptive.increase_percent),
            ("maximum_change_percent", q.adaptive.maximum_change_percent),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(format!("qos.{name} must be finite and non-negative"));
            }
        }
        if !(10.0..=100.0).contains(&q.adaptive.target_utilization_percent)
            || !(0.0..1.0).contains(&q.adaptive.decrease_factor)
            || q.adaptive.increase_percent <= 0.0
            || q.adaptive.maximum_change_percent <= 0.0
            || q.adaptive.maximum_change_percent > 100.0
            || q.adaptive.minimum_samples == 0
            || q.adaptive.interval_secs == 0
            || q.adaptive.cooldown_secs == 0
            || q.adaptive.congestion_delay_ms == 0
            || q.adaptive.recovery_delay_ms >= q.adaptive.congestion_delay_ms
        {
            return Err("invalid adaptive QoS controller settings".into());
        }
        if q.burst_ms == 0
            || q.max_flow_queue_kb == 0
            || q.max_interface_queue_mb == 0
            || q.global_memory_mb == 0
            || q.quantum_bytes == 0
            || q.udp_aqm.target_delay_ms == 0
            || q.udp_aqm.interval_ms == 0
        {
            return Err(
                "QoS burst, queue, memory, quantum and AQM durations must be positive".into(),
            );
        }
        if q.max_flow_queue_kb > q.max_interface_queue_mb.saturating_mul(1024)
            || q.max_interface_queue_mb > q.global_memory_mb
        {
            return Err("QoS queue limits must satisfy flow <= interface <= global memory".into());
        }
        if q.enabled
            && q.mode == QosMode::Fixed
            && q.aggregate_upload_mbps == 0.0
            && q.aggregate_download_mbps == 0.0
            && q.interfaces
                .values()
                .all(|i| i.fixed_upload_mbps == 0.0 && i.fixed_download_mbps == 0.0)
        {
            return Err("fixed QoS mode requires at least one aggregate or interface limit".into());
        }
        for (class, weight) in &q.class_weights {
            if !matches!(
                class.as_str(),
                "interactive" | "dns" | "best_effort" | "bulk"
            ) || *weight == 0
                || *weight > 100
            {
                return Err(format!("invalid QoS class weight '{class}={weight}'"));
            }
        }
        if let Some(target) = &q.probe_target {
            target
                .parse::<std::net::SocketAddr>()
                .map_err(|_| "qos.probe_target must be a socket address")?;
        }
        for (guid, interface) in &q.interfaces {
            if !valid_guid(guid) {
                return Err(format!("invalid interface GUID '{guid}'"));
            }
            if !interface.weight.is_finite() || !(1.0..=1000.0).contains(&interface.weight) {
                return Err(format!("interface '{guid}' weight must be in 1..=1000"));
            }
            for (label, value) in [
                ("capacity_upload_mbps", interface.capacity_upload_mbps),
                ("capacity_download_mbps", interface.capacity_download_mbps),
                ("fixed_upload_mbps", interface.fixed_upload_mbps),
                ("fixed_download_mbps", interface.fixed_download_mbps),
            ] {
                if !value.is_finite() || value < 0.0 {
                    return Err(format!(
                        "interface '{guid}' {label} must be finite and non-negative"
                    ));
                }
            }
            for (direction, min, max, capacity) in [
                (
                    "upload",
                    interface.minimum_upload_mbps,
                    interface.maximum_upload_mbps,
                    interface.capacity_upload_mbps,
                ),
                (
                    "download",
                    interface.minimum_download_mbps,
                    interface.maximum_download_mbps,
                    interface.capacity_download_mbps,
                ),
            ] {
                if min.is_some_and(|v| !v.is_finite() || v < 0.0)
                    || max.is_some_and(|v| !v.is_finite() || v < 0.0)
                {
                    return Err(format!(
                        "interface '{guid}' {direction} min/max must be finite and non-negative"
                    ));
                }
                if let (Some(min), Some(max)) = (min, max)
                    && min > max
                {
                    return Err(format!(
                        "interface '{guid}' {direction} minimum exceeds maximum"
                    ));
                }
                let fixed = match direction {
                    "upload" => interface.fixed_upload_mbps,
                    _ => interface.fixed_download_mbps,
                };
                if max.is_some_and(|max| fixed > max) {
                    return Err(format!(
                        "interface '{guid}' fixed {direction} rate exceeds configured maximum"
                    ));
                }
                if max.is_some_and(|max| capacity > 0.0 && max > capacity) {
                    return Err(format!(
                        "interface '{guid}' {direction} maximum exceeds configured capacity"
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        Self::parse(&text)
    }

    pub fn write_atomic(path: &Path, config: &Self, backup: bool) -> Result<(), String> {
        config.validate()?;
        let text = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = path
            .file_name()
            .ok_or("configuration path has no filename")?
            .to_string_lossy();
        let temp = parent.join(format!(".{name}.tmp-{}", std::process::id()));
        if backup && path.exists() {
            let backup_path = path.with_extension("toml.bak");
            fs::copy(path, &backup_path)
                .map_err(|e| format!("backup {}: {e}", backup_path.display()))?;
        }
        fs::write(&temp, text).map_err(|e| format!("write temporary config: {e}"))?;
        if let Err(error) = atomic_replace(&temp, path) {
            let _ = fs::remove_file(&temp);
            return Err(format!("atomically replace {}: {error}", path.display()));
        }
        Ok(())
    }
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::{io, os::windows::ffi::OsStrExt};
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

pub fn migrate_file(path: &Path) -> Result<PathBuf, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let value: toml::Value = toml::from_str(&raw).map_err(|e| e.to_string())?;
    let version = value
        .get("config")
        .and_then(|v| v.get("schema_version"))
        .and_then(toml::Value::as_integer)
        .unwrap_or(0);
    if version == CURRENT_SCHEMA_VERSION as i64 {
        ConfigFile::parse(&raw)?;
        return Ok(path.to_path_buf());
    }
    if version != 0 {
        return Err(format!(
            "unsupported configuration schema_version {version}; original file retained"
        ));
    }
    let migrated = ConfigFile::parse(&raw)
        .map_err(|e| format!("migration rejected; original file retained: {e}"))?;
    ConfigFile::write_atomic(path, &migrated, true)?;
    Ok(path.to_path_buf())
}

pub fn valid_guid(value: &str) -> bool {
    let value = value.trim().trim_start_matches('{').trim_end_matches('}');
    let parts: Vec<_> = value.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(parts)
            .all(|(len, part)| part.len() == *len && part.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn default_schema_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_defaults_and_rejects_unknown_values() {
        let config = ConfigFile::parse("[config]\nschema_version=1\n").unwrap();
        assert!(!config.qos.enabled);
        assert!(ConfigFile::parse("[config]\nschema_version=1\n[qos]\nunknown=1").is_err());
    }

    #[test]
    fn migrates_v0_schema_in_memory_and_rejects_future_versions() {
        let config =
            ConfigFile::parse("[qos]\nenabled=true\nmode='fixed'\naggregate_upload_mbps=2.0")
                .unwrap();
        assert_eq!(config.config.schema_version, 1);
        assert!(ConfigFile::parse("[config]\nschema_version=99").is_err());
    }

    #[test]
    fn validates_stable_guid_and_rate_constraints() {
        assert!(valid_guid("{01234567-89ab-cdef-0123-456789abcdef}"));
        assert!(!valid_guid("wifi"));
        let mut config = ConfigFile::default();
        config
            .qos
            .interfaces
            .insert("wifi".into(), InterfaceFile::default());
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_contradictory_adaptive_limits() {
        let mut config = ConfigFile::default();
        config.qos.interfaces.insert(
            "01234567-89ab-cdef-0123-456789abcdef".into(),
            InterfaceFile {
                minimum_upload_mbps: Some(5.0),
                maximum_upload_mbps: Some(2.0),
                ..InterfaceFile::default()
            },
        );
        assert!(config.validate().is_err());
    }

    #[test]
    fn migration_creates_backup_and_rewrites_version_atomically() {
        let path =
            std::env::temp_dir().join(format!("pathweave-config-{}.toml", std::process::id()));
        let backup = path.with_extension("toml.bak");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&backup);
        fs::write(&path, "[qos]\nenabled=false\nmode='disabled'\n").unwrap();
        migrate_file(&path).unwrap();
        let migrated = ConfigFile::load(&path).unwrap();
        assert_eq!(migrated.config.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(backup.exists());
        assert!(
            fs::read_to_string(&backup)
                .unwrap()
                .contains("enabled=false")
        );
        fs::remove_file(path).unwrap();
        fs::remove_file(backup).unwrap();
    }
}
