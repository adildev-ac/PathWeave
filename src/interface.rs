use std::{
    io,
    net::SocketAddr,
    sync::{
        Mutex, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::net::{TcpSocket, TcpStream, UdpSocket};

#[derive(Debug)]
struct InterfaceSlot {
    guid: String,
    luid: u64,
    index: u32,
    operational: bool,
    health: Mutex<InterfaceHealthState>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceIdentity {
    pub guid: String,
    pub luid: u64,
    pub index: u32,
    pub alias: String,
    pub operational: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceRebind {
    pub guid: String,
    pub previous_index: u32,
    pub current_index: Option<u32>,
    pub luid: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceHealth {
    Healthy,
    Degraded,
    Unavailable,
    Recovering,
}

#[derive(Debug)]
struct InterfaceHealthState {
    status: InterfaceHealth,
    unavailable_until: Option<Instant>,
}

#[derive(Debug)]
pub struct InterfacePool {
    slots: RwLock<Vec<InterfaceSlot>>,
    next: AtomicUsize,
    cooldown: Duration,
}

impl InterfacePool {
    #[cfg(test)]
    pub fn new(indexes: Vec<u32>, cooldown: Duration) -> io::Result<Self> {
        let mut seen = std::collections::HashSet::new();
        if indexes.is_empty()
            || indexes.contains(&0)
            || indexes.iter().any(|index| !seen.insert(*index))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "interface indexes must be nonzero, unique, and nonempty",
            ));
        }
        let identities = indexes
            .into_iter()
            .map(|index| InterfaceIdentity {
                guid: format!("index:{index}"),
                luid: 0,
                index,
                alias: format!("ifindex-{index}"),
                operational: true,
            })
            .collect();
        Self::new_stable(identities, cooldown)
    }

    pub fn new_stable(identities: Vec<InterfaceIdentity>, cooldown: Duration) -> io::Result<Self> {
        let mut seen_guid = std::collections::HashSet::new();
        let mut seen_index = std::collections::HashSet::new();
        if identities.is_empty()
            || identities.iter().any(|id| {
                id.index == 0
                    || id.guid.trim().is_empty()
                    || !seen_guid.insert(normalize_guid(&id.guid))
                    || !seen_index.insert(id.index)
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "configured interfaces require unique nonempty GUIDs and unique nonzero indexes",
            ));
        }
        Ok(Self {
            slots: RwLock::new(
                identities
                    .into_iter()
                    .map(|identity| InterfaceSlot {
                        guid: normalize_guid(&identity.guid),
                        luid: identity.luid,
                        index: identity.index,
                        operational: identity.operational,
                        health: Mutex::new(InterfaceHealthState {
                            status: if identity.operational {
                                InterfaceHealth::Healthy
                            } else {
                                InterfaceHealth::Unavailable
                            },
                            unavailable_until: None,
                        }),
                    })
                    .collect(),
            ),
            next: AtomicUsize::new(0),
            cooldown,
        })
    }

    pub fn next_available(&self) -> Option<u32> {
        let now = Instant::now();
        let start = self.next.fetch_add(1, Ordering::Relaxed);
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        if slots.is_empty() {
            return None;
        }
        for preferred in [InterfaceHealth::Healthy, InterfaceHealth::Degraded] {
            for offset in 0..slots.len() {
                let slot = &slots[(start + offset) % slots.len()];
                let mut health = slot
                    .health
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if health.unavailable_until.is_some_and(|until| until <= now) {
                    health.status = InterfaceHealth::Healthy;
                    health.unavailable_until = None;
                }
                if slot.operational && health.status == preferred {
                    return Some(slot.index);
                }
            }
        }
        None
    }

    pub fn mark_healthy(&self, index: u32) {
        self.set_health(index, InterfaceHealth::Healthy);
    }

    pub fn set_health(&self, index: u32, status: InterfaceHealth) -> bool {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = slots.iter().find(|slot| slot.index == index) {
            let mut health = slot
                .health
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            health.status = status;
            health.unavailable_until = None;
            true
        } else {
            false
        }
    }

    pub fn health(&self, index: u32) -> Option<InterfaceHealth> {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        let slot = slots.iter().find(|slot| slot.index == index)?;
        let mut health = slot
            .health
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if health
            .unavailable_until
            .is_some_and(|until| until <= Instant::now())
        {
            health.status = InterfaceHealth::Healthy;
            health.unavailable_until = None;
        }
        Some(health.status)
    }

    pub fn report_path_error(&self, index: u32, error: &io::Error) -> bool {
        if !is_interface_path_error(error) {
            return false;
        }
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = slots.iter().find(|slot| slot.index == index) {
            let mut health = slot
                .health
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            health.status = InterfaceHealth::Unavailable;
            health.unavailable_until = Some(Instant::now() + self.cooldown);
            true
        } else {
            false
        }
    }

    pub fn configured_indexes(&self) -> Vec<u32> {
        self.slots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|slot| slot.index)
            .collect()
    }

    pub fn guid_for_index(&self, index: u32) -> Option<String> {
        self.slots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|slot| slot.index == index)
            .map(|slot| slot.guid.clone())
    }

    pub fn configured_guids(&self) -> Vec<String> {
        self.slots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|slot| slot.guid.clone())
            .collect()
    }

    pub fn current_index_for_guid(&self, guid: &str) -> Option<u32> {
        let guid = normalize_guid(guid);
        self.slots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|slot| slot.guid == guid && slot.operational)
            .map(|slot| slot.index)
    }

    pub fn contains_guid(&self, guid: &str) -> bool {
        let guid = normalize_guid(guid);
        self.slots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|slot| slot.guid == guid)
    }

    pub fn reconcile(&self, discovered: &[InterfaceIdentity]) -> Vec<InterfaceRebind> {
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        let mut changes = Vec::new();
        for slot in slots.iter_mut() {
            let previous_index = slot.index;
            let was_operational = slot.operational;
            if let Some(current) = discovered
                .iter()
                .find(|row| normalize_guid(&row.guid) == slot.guid)
            {
                slot.index = current.index;
                slot.luid = current.luid;
                let mut health = slot.health.lock().unwrap_or_else(|e| e.into_inner());
                if !current.operational {
                    slot.operational = false;
                    health.status = InterfaceHealth::Unavailable;
                    health.unavailable_until = None;
                } else {
                    if !was_operational {
                        health.status = InterfaceHealth::Degraded;
                    }
                    slot.operational = true;
                    health.unavailable_until = None;
                }
                if previous_index != current.index || was_operational != current.operational {
                    changes.push(InterfaceRebind {
                        guid: slot.guid.clone(),
                        previous_index,
                        current_index: current.operational.then_some(current.index),
                        luid: current.operational.then_some(current.luid),
                    });
                }
            } else {
                slot.operational = false;
                let mut health = slot.health.lock().unwrap_or_else(|e| e.into_inner());
                let changed = was_operational || health.status != InterfaceHealth::Unavailable;
                health.status = InterfaceHealth::Unavailable;
                health.unavailable_until = None;
                if changed {
                    changes.push(InterfaceRebind {
                        guid: slot.guid.clone(),
                        previous_index,
                        current_index: None,
                        luid: None,
                    });
                }
            }
        }
        changes
    }
}

fn normalize_guid(guid: &str) -> String {
    guid.trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .to_ascii_lowercase()
}

fn is_interface_path_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::NetworkDown
            | io::ErrorKind::HostUnreachable
            | io::ErrorKind::AddrNotAvailable
    ) || matches!(error.raw_os_error(), Some(10049 | 10050 | 10051 | 10065))
}

pub async fn connect_on_interface(
    destination: SocketAddr,
    interface_index: u32,
) -> io::Result<TcpStream> {
    let socket = if destination.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    set_tcp_interface(&socket, destination, interface_index)?;
    socket.connect(destination).await
}

pub fn bind_udp_on_interface(
    destination: SocketAddr,
    interface_index: u32,
) -> io::Result<UdpSocket> {
    let bind_addr = if destination.is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from(([0u16; 8], 0))
    };
    let socket = std::net::UdpSocket::bind(bind_addr)?;
    socket.set_nonblocking(true)?;
    set_udp_interface(&socket, destination, interface_index)?;
    socket.connect(destination)?;
    UdpSocket::from_std(socket)
}

#[cfg(windows)]
fn set_tcp_interface(
    socket: &TcpSocket,
    destination: SocketAddr,
    interface_index: u32,
) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    set_interface_raw(
        socket.as_raw_socket() as usize,
        destination,
        interface_index,
    )
}

#[cfg(windows)]
fn set_udp_interface(
    socket: &std::net::UdpSocket,
    destination: SocketAddr,
    interface_index: u32,
) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    set_interface_raw(
        socket.as_raw_socket() as usize,
        destination,
        interface_index,
    )
}

#[cfg(windows)]
fn set_interface_raw(
    raw_socket: usize,
    destination: SocketAddr,
    interface_index: u32,
) -> io::Result<()> {
    use std::mem::size_of;
    use windows_sys::Win32::Networking::WinSock::{
        IP_UNICAST_IF, IPPROTO_IP, IPPROTO_IPV6, IPV6_UNICAST_IF, SOCKET_ERROR, setsockopt,
    };

    let index = interface_index.to_be_bytes();
    let (level, option) = if destination.is_ipv4() {
        (IPPROTO_IP, IP_UNICAST_IF)
    } else {
        (IPPROTO_IPV6, IPV6_UNICAST_IF)
    };
    // SAFETY: socket is a live Winsock socket and `index` is a DWORD in the byte order
    // required by IP_UNICAST_IF / IPV6_UNICAST_IF.
    let result = unsafe {
        setsockopt(
            raw_socket,
            level,
            option,
            index.as_ptr(),
            size_of::<u32>() as i32,
        )
    };
    if result == SOCKET_ERROR {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn set_tcp_interface(
    _socket: &TcpSocket,
    _destination: SocketAddr,
    _interface_index: u32,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "interface binding is currently implemented on Windows only",
    ))
}

#[cfg(not(windows))]
fn set_udp_interface(
    _socket: &std::net::UdpSocket,
    _destination: SocketAddr,
    _interface_index: u32,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "interface binding is currently implemented on Windows only",
    ))
}

#[cfg(test)]
mod tests {
    use std::{io, thread, time::Duration};

    use super::{InterfaceHealth, InterfaceIdentity, InterfacePool};

    #[test]
    fn scheduler_rotates_and_skips_quarantined_interfaces() {
        let pool = InterfacePool::new(vec![8, 4], Duration::from_millis(20)).unwrap();
        assert_eq!(pool.next_available(), Some(8));
        assert_eq!(pool.next_available(), Some(4));
        assert!(pool.report_path_error(8, &io::Error::from_raw_os_error(10051)));
        assert_eq!(pool.next_available(), Some(4));
        thread::sleep(Duration::from_millis(25));
        assert!(matches!(pool.next_available(), Some(8) | Some(4)));
    }

    #[test]
    fn remote_refusal_does_not_mark_an_interface_down() {
        let pool = InterfacePool::new(vec![8], Duration::from_secs(1)).unwrap();
        assert!(!pool.report_path_error(8, &io::Error::from_raw_os_error(10061)));
        assert_eq!(pool.next_available(), Some(8));
    }

    #[test]
    fn health_override_excludes_and_recovers_interface_without_moving_existing_flow() {
        let pool = InterfacePool::new(vec![8, 4], Duration::from_secs(1)).unwrap();
        let pinned_flow_interface = pool.next_available().unwrap();
        assert_eq!(pinned_flow_interface, 8);

        assert!(pool.set_health(4, InterfaceHealth::Unavailable));
        assert_eq!(pool.next_available(), Some(8));
        assert_eq!(pool.health(4), Some(InterfaceHealth::Unavailable));
        assert_eq!(pinned_flow_interface, 8);

        assert!(pool.set_health(4, InterfaceHealth::Recovering));
        assert_eq!(pool.next_available(), Some(8));
        assert_eq!(pool.health(4), Some(InterfaceHealth::Recovering));

        assert!(pool.set_health(4, InterfaceHealth::Healthy));
        assert_eq!(pool.next_available(), Some(4));
        assert_eq!(pool.health(4), Some(InterfaceHealth::Healthy));
    }

    #[test]
    fn scheduler_uses_degraded_interface_only_when_no_healthy_path_is_available() {
        let pool = InterfacePool::new(vec![8, 4], Duration::from_secs(1)).unwrap();
        pool.set_health(8, InterfaceHealth::Degraded);
        assert_eq!(pool.next_available(), Some(4));
        pool.set_health(4, InterfaceHealth::Unavailable);
        assert_eq!(pool.next_available(), Some(8));
        pool.set_health(8, InterfaceHealth::Recovering);
        assert_eq!(pool.next_available(), None);
    }

    #[test]
    fn stable_guid_reconciles_changed_index_and_preserves_health_configuration() {
        let pool = InterfacePool::new_stable(
            vec![InterfaceIdentity {
                guid: "{ABCDEF01-0000-0000-0000-000000000001}".into(),
                luid: 10,
                index: 8,
                alias: "USB tether".into(),
                operational: true,
            }],
            Duration::from_secs(1),
        )
        .unwrap();
        pool.set_health(8, InterfaceHealth::Degraded);
        let changes = pool.reconcile(&[InterfaceIdentity {
            guid: "abcdef01-0000-0000-0000-000000000001".into(),
            luid: 99,
            index: 42,
            alias: "renamed adapter".into(),
            operational: true,
        }]);
        assert_eq!(pool.configured_indexes(), vec![42]);
        assert_eq!(pool.health(42), Some(InterfaceHealth::Degraded));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].previous_index, 8);
        assert_eq!(changes[0].current_index, Some(42));
        assert_eq!(changes[0].luid, Some(99));
    }

    #[test]
    fn missing_guid_stays_unavailable_and_duplicate_alias_is_not_used_for_matching() {
        let pool = InterfacePool::new_stable(
            vec![InterfaceIdentity {
                guid: "adapter-guid-a".into(),
                luid: 1,
                index: 8,
                alias: "USB tether".into(),
                operational: true,
            }],
            Duration::from_secs(1),
        )
        .unwrap();
        pool.reconcile(&[InterfaceIdentity {
            guid: "adapter-guid-b".into(),
            luid: 2,
            index: 42,
            alias: "USB tether".into(),
            operational: true,
        }]);
        assert_eq!(pool.next_available(), None);
        assert_eq!(pool.configured_indexes(), vec![8]);
    }

    #[test]
    fn adapter_refresh_does_not_clear_path_health_quarantine() {
        let identity = InterfaceIdentity {
            guid: "adapter-guid-a".into(),
            luid: 1,
            index: 8,
            alias: "Wi-Fi".into(),
            operational: true,
        };
        let pool =
            InterfacePool::new_stable(vec![identity.clone()], Duration::from_secs(5)).unwrap();
        pool.set_health(8, InterfaceHealth::Unavailable);
        pool.reconcile(&[identity]);
        assert_eq!(pool.health(8), Some(InterfaceHealth::Unavailable));
        assert_eq!(pool.next_available(), None);
    }
}
