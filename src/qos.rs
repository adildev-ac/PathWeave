use std::{
    collections::{HashMap, VecDeque},
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, oneshot};

use crate::scheduler::TrafficClass;

const DEFAULT_TCP_CHUNK: usize = 16 * 1024;
const MIN_BURST_BYTES: usize = 1500;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QosMode {
    #[default]
    Disabled,
    Fixed,
    Adaptive,
    MonitorOnly,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QosDirection {
    Upload,
    Download,
}

impl QosDirection {
    fn index(self) -> usize {
        match self {
            Self::Upload => 0,
            Self::Download => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum QosClass {
    Interactive,
    Dns,
    BestEffort,
    Bulk,
}

impl From<TrafficClass> for QosClass {
    fn from(class: TrafficClass) -> Self {
        match class {
            TrafficClass::Interactive => Self::Interactive,
            TrafficClass::Dns => Self::Dns,
            TrafficClass::Bulk => Self::Bulk,
            TrafficClass::Balanced => Self::BestEffort,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InterfaceRates {
    pub upload_bytes_per_sec: Option<u64>,
    pub download_bytes_per_sec: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct QosConfig {
    pub mode: QosMode,
    pub aggregate: InterfaceRates,
    pub interfaces: HashMap<u32, InterfaceRates>,
    pub burst_ms: u64,
    pub max_flow_queue_bytes: usize,
    pub max_interface_queue_bytes: usize,
    pub global_memory_limit_bytes: usize,
    pub quantum_bytes: usize,
    pub class_weights: HashMap<QosClass, u32>,
    pub udp_aqm: UdpAqmConfig,
}

#[derive(Clone, Copy, Debug)]
pub struct UdpAqmConfig {
    pub enabled: bool,
    pub target_delay: Duration,
    pub interval: Duration,
}

impl Default for UdpAqmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target_delay: Duration::from_millis(5),
            interval: Duration::from_millis(100),
        }
    }
}

impl Default for QosConfig {
    fn default() -> Self {
        Self {
            mode: QosMode::Disabled,
            aggregate: InterfaceRates::default(),
            interfaces: HashMap::new(),
            burst_ms: 50,
            max_flow_queue_bytes: 256 * 1024,
            max_interface_queue_bytes: 16 * 1024 * 1024,
            global_memory_limit_bytes: 128 * 1024 * 1024,
            quantum_bytes: 1500,
            class_weights: HashMap::from([
                (QosClass::Interactive, 8),
                (QosClass::Dns, 10),
                (QosClass::BestEffort, 4),
                (QosClass::Bulk, 2),
            ]),
            udp_aqm: UdpAqmConfig::default(),
        }
    }
}

impl QosConfig {
    pub fn validate(&self) -> io::Result<()> {
        let rates = [
            self.aggregate.upload_bytes_per_sec,
            self.aggregate.download_bytes_per_sec,
        ]
        .into_iter()
        .chain(
            self.interfaces
                .values()
                .flat_map(|rates| [rates.upload_bytes_per_sec, rates.download_bytes_per_sec]),
        );
        if rates.into_iter().flatten().any(|rate| rate == 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "QoS rates must be positive; use zero only in CLI input to mean unlimited",
            ));
        }
        if self.burst_ms == 0
            || self.max_flow_queue_bytes == 0
            || self.max_interface_queue_bytes == 0
            || self.global_memory_limit_bytes == 0
            || self.quantum_bytes == 0
            || self.udp_aqm.target_delay.is_zero()
            || self.udp_aqm.interval.is_zero()
            || self.global_memory_limit_bytes > u32::MAX as usize
            || self.max_interface_queue_bytes > u32::MAX as usize
            || self.max_flow_queue_bytes > u32::MAX as usize
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "QoS burst, queue, memory, and quantum values must be positive and fit the semaphore limit",
            ));
        }
        for weight in self.class_weights.values() {
            if *weight == 0 || *weight > 100 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "QoS class weights must be in 1..=100",
                ));
            }
        }
        if self.mode == QosMode::Fixed
            && self.aggregate.upload_bytes_per_sec.is_none()
            && self.aggregate.download_bytes_per_sec.is_none()
            && self.interfaces.values().all(|rates| {
                rates.upload_bytes_per_sec.is_none() && rates.download_bytes_per_sec.is_none()
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fixed QoS mode requires at least one aggregate or interface rate limit",
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct QosSession {
    inner: Arc<QosSessionInner>,
}

struct QosSessionInner {
    engine: Arc<QosEngine>,
    flow_id: u64,
    interface_index: u32,
    class: QosClass,
}

impl Drop for QosSessionInner {
    fn drop(&mut self) {
        self.engine.unregister(self.flow_id);
    }
}

impl QosSession {
    pub fn tcp_chunk_limit(&self, direction: QosDirection) -> usize {
        if self.inner.engine.config.mode == QosMode::Disabled {
            return DEFAULT_TCP_CHUNK;
        }
        self.inner
            .engine
            .max_request_bytes(self.inner.interface_index, direction)
            .min(self.inner.engine.config.max_flow_queue_bytes)
            .clamp(1, DEFAULT_TCP_CHUNK)
    }

    pub async fn acquire_tcp(
        &self,
        direction: QosDirection,
        bytes: usize,
    ) -> io::Result<QosPermit> {
        if self.inner.engine.config.mode == QosMode::Disabled {
            return Ok(QosPermit::unlimited(bytes));
        }
        self.inner
            .engine
            .acquire(
                self.inner.flow_id,
                self.inner.interface_index,
                self.inner.class,
                direction,
                bytes,
                true,
            )
            .await?
            .ok_or_else(|| io::Error::other("TCP QoS queue unexpectedly rejected data"))
    }

    /// Returns `None` if a complete UDP datagram cannot fit the configured
    /// burst or bounded queues. Datagram payloads are never split.
    pub async fn acquire_udp(
        &self,
        direction: QosDirection,
        bytes: usize,
    ) -> io::Result<Option<QosPermit>> {
        if self.inner.engine.config.mode == QosMode::Disabled {
            return Ok(Some(QosPermit::unlimited(bytes)));
        }
        if bytes
            > self
                .inner
                .engine
                .max_request_bytes(self.inner.interface_index, direction)
            || bytes > self.inner.engine.config.max_flow_queue_bytes
        {
            self.record_udp_drop(direction);
            return Ok(None);
        }
        let permit = self
            .inner
            .engine
            .acquire(
                self.inner.flow_id,
                self.inner.interface_index,
                self.inner.class,
                direction,
                bytes,
                false,
            )
            .await?;
        if permit.is_none() {
            self.record_udp_drop(direction);
        }
        Ok(permit)
    }

    pub fn record_udp_drop(&self, direction: QosDirection) {
        let lane = self
            .inner
            .engine
            .lane(self.inner.interface_index, direction);
        lane.stats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .udp_drops += 1;
    }
}

pub struct QosPermit {
    pub bytes: usize,
    _global_memory: Option<OwnedSemaphorePermit>,
    _interface_memory: Option<OwnedSemaphorePermit>,
}

impl QosPermit {
    fn unlimited(bytes: usize) -> Self {
        Self {
            bytes,
            _global_memory: None,
            _interface_memory: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct QosLaneDiagnostics {
    pub interface_index: u32,
    pub direction: QosDirection,
    pub active_flows: usize,
    pub queued_bytes: usize,
    pub peak_queued_bytes: usize,
    pub bytes_shaped: u64,
    pub udp_drops: u64,
    pub udp_aqm_drops: u64,
    pub average_queue_delay_ms: f64,
    pub p50_queue_delay_ms: f64,
    pub p95_queue_delay_ms: f64,
    pub p99_queue_delay_ms: f64,
    pub pacing_wait_ms: f64,
    pub queue_delay_samples: usize,
    pub queue_delay_sample_age_ms: Option<u64>,
}

struct LaneStats {
    udp_drops: u64,
    udp_aqm_drops: u64,
    bytes_shaped: u64,
    peak_queued_bytes: usize,
    queue_delays_us: VecDeque<u64>,
    last_queue_delay_at: Option<Instant>,
    pacing_wait_us: u128,
}

impl Default for LaneStats {
    fn default() -> Self {
        Self {
            udp_drops: 0,
            udp_aqm_drops: 0,
            bytes_shaped: 0,
            peak_queued_bytes: 0,
            queue_delays_us: VecDeque::with_capacity(256),
            last_queue_delay_at: None,
            pacing_wait_us: 0,
        }
    }
}

struct Request {
    flow_id: u64,
    class: QosClass,
    bytes: usize,
    enqueued_at: Instant,
    udp_datagram: bool,
    completion: oneshot::Sender<io::Result<bool>>,
}

#[derive(Clone, Copy, Debug, Default)]
struct CodelState {
    first_above: Option<Instant>,
    dropping: bool,
    count: u32,
    drop_next: Option<Instant>,
}

impl CodelState {
    fn should_drop(
        &mut self,
        now: Instant,
        sojourn: Duration,
        backlog_bytes: usize,
        config: UdpAqmConfig,
    ) -> bool {
        if backlog_bytes < MIN_BURST_BYTES || sojourn < config.target_delay {
            self.first_above = None;
            self.dropping = false;
            self.drop_next = None;
            return false;
        }
        if !self.dropping {
            let Some(first_above) = self.first_above else {
                self.first_above = Some(now + config.interval);
                return false;
            };
            if now < first_above {
                return false;
            }
            self.dropping = true;
            self.count = 1;
            self.drop_next = Some(now + config.interval);
            return true;
        }
        if self.drop_next.is_some_and(|next| now >= next) {
            self.count = self.count.saturating_add(1);
            let spacing = config.interval.div_f64((self.count as f64).sqrt().max(1.0));
            self.drop_next = Some(now + spacing.max(Duration::from_millis(1)));
            return true;
        }
        false
    }
}

struct FlowQueue {
    class: QosClass,
    deficit: usize,
    requests: VecDeque<Request>,
    codel: CodelState,
    last_request_at: Option<Instant>,
}

#[derive(Default)]
struct DrrState {
    queues: HashMap<u64, FlowQueue>,
    active: VecDeque<u64>,
    queued_bytes: usize,
}

struct DrrLane {
    interface_index: u32,
    direction: QosDirection,
    base_quantum: usize,
    class_weights: HashMap<QosClass, u32>,
    max_interface_queue_bytes: usize,
    state: Mutex<DrrState>,
    notify: Notify,
    stats: Mutex<LaneStats>,
    interface_memory: Arc<Semaphore>,
}

impl DrrLane {
    fn new(
        config: &QosConfig,
        interface_index: u32,
        direction: QosDirection,
        interface_memory: Arc<Semaphore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            interface_index,
            direction,
            base_quantum: config.quantum_bytes,
            class_weights: config.class_weights.clone(),
            max_interface_queue_bytes: config.max_interface_queue_bytes,
            state: Mutex::new(DrrState::default()),
            notify: Notify::new(),
            stats: Mutex::new(LaneStats::default()),
            interface_memory,
        })
    }

    fn start(self: &Arc<Self>, engine: Arc<QosEngine>) {
        let lane = self.clone();
        tokio::spawn(async move {
            lane.run(engine).await;
        });
    }

    async fn acquire(
        self: &Arc<Self>,
        engine: &Arc<QosEngine>,
        flow_id: u64,
        class: QosClass,
        bytes: usize,
        wait_for_capacity: bool,
    ) -> io::Result<Option<QosPermit>> {
        if bytes > self.max_interface_queue_bytes || bytes > engine.config.global_memory_limit_bytes
        {
            return Ok(None);
        }
        let amount = bytes.max(1) as u32;
        let global_memory = if wait_for_capacity {
            match engine
                .global_memory
                .clone()
                .acquire_many_owned(amount)
                .await
            {
                Ok(permit) => Some(permit),
                Err(_) => return Err(io::Error::other("QoS memory budget is closed")),
            }
        } else {
            match engine.global_memory.clone().try_acquire_many_owned(amount) {
                Ok(permit) => Some(permit),
                Err(_) => return Ok(None),
            }
        };
        let interface_memory = if wait_for_capacity {
            match self
                .interface_memory
                .clone()
                .acquire_many_owned(amount)
                .await
            {
                Ok(permit) => Some(permit),
                Err(_) => return Err(io::Error::other("QoS interface queue is closed")),
            }
        } else {
            match self.interface_memory.clone().try_acquire_many_owned(amount) {
                Ok(permit) => Some(permit),
                Err(_) => return Ok(None),
            }
        };

        let enqueued_at = Instant::now();
        let (completion, receiver) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let activate = state
                .queues
                .get(&flow_id)
                .is_none_or(|flow| flow.requests.is_empty());
            if activate && !state.active.contains(&flow_id) {
                state.active.push_back(flow_id);
            }
            let flow = state.queues.entry(flow_id).or_insert_with(|| FlowQueue {
                class,
                deficit: 0,
                requests: VecDeque::new(),
                codel: CodelState::default(),
                last_request_at: None,
            });
            if flow.last_request_at.is_some_and(|last| {
                enqueued_at.saturating_duration_since(last) > engine.config.udp_aqm.interval
            }) {
                flow.codel = CodelState::default();
            }
            flow.last_request_at = Some(enqueued_at);
            flow.class = class;
            flow.requests.push_back(Request {
                flow_id,
                class,
                bytes,
                enqueued_at,
                udp_datagram: !wait_for_capacity,
                completion,
            });
            state.queued_bytes = state.queued_bytes.saturating_add(bytes);
            if state.queued_bytes > self.max_interface_queue_bytes {
                if let Some(flow) = state.queues.get_mut(&flow_id) {
                    flow.requests.pop_back();
                }
                state.queued_bytes = state.queued_bytes.saturating_sub(bytes);
                return Ok(None);
            }
            let mut stats = self.stats.lock().unwrap_or_else(|e| e.into_inner());
            stats.peak_queued_bytes = stats.peak_queued_bytes.max(state.queued_bytes);
        }
        self.notify.notify_one();
        let serviced = receiver.await.map_err(io::Error::other)??;
        if !serviced {
            return Ok(None);
        }
        Ok(Some(QosPermit {
            bytes,
            _global_memory: global_memory,
            _interface_memory: interface_memory,
        }))
    }

    async fn run(self: Arc<Self>, engine: Arc<QosEngine>) {
        loop {
            let request = self.next_request();
            let Some(request) = request else {
                self.notify.notified().await;
                continue;
            };
            if request.completion.is_closed() {
                continue;
            }
            let pacing_started = Instant::now();
            let result = if matches!(engine.config.mode, QosMode::Fixed | QosMode::Adaptive) {
                engine
                    .rates
                    .acquire(self.interface_index, self.direction, request.bytes)
                    .await
            } else {
                Ok(())
            };
            let mut result = result.map(|()| true);
            if result.as_ref().is_ok_and(|serviced| *serviced)
                && request.udp_datagram
                && request.class != QosClass::Dns
                && engine.config.udp_aqm.enabled
            {
                let now = Instant::now();
                let sojourn = request.enqueued_at.elapsed();
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let backlog_bytes = state.queued_bytes;
                if let Some(flow) = state.queues.get_mut(&request.flow_id)
                    && flow
                        .codel
                        .should_drop(now, sojourn, backlog_bytes, engine.config.udp_aqm)
                {
                    result = Ok(false);
                    self.stats
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .udp_aqm_drops += 1;
                }
            }
            {
                let mut stats = self.stats.lock().unwrap_or_else(|e| e.into_inner());
                stats.pacing_wait_us += pacing_started.elapsed().as_micros();
                if result.as_ref().is_ok_and(|serviced| *serviced) {
                    stats.bytes_shaped = stats.bytes_shaped.saturating_add(request.bytes as u64);
                }
                let delay = request
                    .enqueued_at
                    .elapsed()
                    .as_micros()
                    .min(u64::MAX as u128) as u64;
                stats.queue_delays_us.push_back(delay);
                stats.last_queue_delay_at = Some(Instant::now());
                if stats.queue_delays_us.len() > 256 {
                    stats.queue_delays_us.pop_front();
                }
            }
            let _ = request.completion.send(result);
        }
    }

    fn next_request(&self) -> Option<Request> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let rounds = state.active.len().saturating_mul(1024).max(1024);
        for _ in 0..rounds {
            let flow_id = state.active.pop_front()?;
            let Some(flow) = state.queues.get_mut(&flow_id) else {
                continue;
            };
            let Some(front) = flow.requests.front() else {
                continue;
            };
            let quantum = self
                .base_quantum
                .saturating_mul(*self.class_weights.get(&flow.class).unwrap_or(&1) as usize);
            if flow.deficit < front.bytes {
                flow.deficit = flow.deficit.saturating_add(quantum);
                state.active.push_back(flow_id);
                continue;
            }
            let flow = state.queues.get_mut(&flow_id)?;
            let request = flow.requests.pop_front()?;
            flow.deficit -= request.bytes;
            if !flow.requests.is_empty() {
                state.active.push_back(flow_id);
            }
            state.queued_bytes = state.queued_bytes.saturating_sub(request.bytes);
            return Some(request);
        }
        None
    }

    fn unregister(&self, flow_id: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(queue) = state.queues.remove(&flow_id) {
            let pending: usize = queue.requests.iter().map(|request| request.bytes).sum();
            state.queued_bytes = state.queued_bytes.saturating_sub(pending);
        }
        state.active.retain(|id| *id != flow_id);
    }

    fn diagnostics(&self) -> QosLaneDiagnostics {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let stats = self.stats.lock().unwrap_or_else(|e| e.into_inner());
        let mut delays: Vec<u64> = stats.queue_delays_us.iter().copied().collect();
        delays.sort_unstable();
        let percentile = |p: usize| -> f64 {
            if delays.is_empty() {
                0.0
            } else {
                delays[(delays.len() - 1) * p / 100] as f64 / 1000.0
            }
        };
        let average = if delays.is_empty() {
            0.0
        } else {
            delays.iter().map(|n| *n as u128).sum::<u128>() as f64 / delays.len() as f64 / 1000.0
        };
        QosLaneDiagnostics {
            interface_index: self.interface_index,
            direction: self.direction,
            active_flows: state.queues.len(),
            queued_bytes: state.queued_bytes,
            peak_queued_bytes: stats.peak_queued_bytes,
            bytes_shaped: stats.bytes_shaped,
            udp_drops: stats.udp_drops,
            udp_aqm_drops: stats.udp_aqm_drops,
            average_queue_delay_ms: average,
            p50_queue_delay_ms: percentile(50),
            p95_queue_delay_ms: percentile(95),
            p99_queue_delay_ms: percentile(99),
            pacing_wait_ms: stats.pacing_wait_us as f64 / 1000.0,
            queue_delay_samples: delays.len(),
            queue_delay_sample_age_ms: stats
                .last_queue_delay_at
                .map(|sample| sample.elapsed().as_millis().min(u64::MAX as u128) as u64),
        }
    }
}

struct Bucket {
    rate: f64,
    capacity: f64,
    tokens: f64,
    last_refill: Instant,
}

impl Bucket {
    fn new(rate: u64, burst_ms: u64) -> Self {
        let capacity =
            ((rate as f64 * burst_ms as f64 / 1000.0).ceil() as usize).max(MIN_BURST_BYTES) as f64;
        Self {
            rate: rate as f64,
            capacity,
            tokens: capacity,
            last_refill: Instant::now(),
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
        self.last_refill = now;
    }

    fn update_rate(&mut self, rate: u64, burst_ms: u64) {
        let capacity =
            ((rate as f64 * burst_ms as f64 / 1000.0).ceil() as usize).max(MIN_BURST_BYTES) as f64;
        self.rate = rate as f64;
        self.tokens = self.tokens.min(capacity);
        self.capacity = capacity;
        self.last_refill = Instant::now();
    }

    fn wait_for(&self, amount: usize) -> io::Result<Duration> {
        if amount as f64 > self.capacity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "request is larger than the token bucket burst",
            ));
        }
        if self.tokens >= amount as f64 {
            return Ok(Duration::ZERO);
        }
        Ok(Duration::from_secs_f64(
            ((amount as f64 - self.tokens) / self.rate).max(0.000_001),
        ))
    }
}

struct RateState {
    aggregate: [Option<Bucket>; 2],
    interfaces: HashMap<u32, [Option<Bucket>; 2]>,
}

struct RateLimiter {
    state: tokio::sync::Mutex<RateState>,
    burst_ms: u64,
}

impl RateLimiter {
    fn new(config: &QosConfig) -> Self {
        let make = |rate: Option<u64>| rate.map(|rate| Bucket::new(rate, config.burst_ms));
        let aggregate = [
            make(config.aggregate.upload_bytes_per_sec),
            make(config.aggregate.download_bytes_per_sec),
        ];
        let interfaces = config
            .interfaces
            .iter()
            .map(|(index, rates)| {
                (
                    *index,
                    [
                        make(rates.upload_bytes_per_sec),
                        make(rates.download_bytes_per_sec),
                    ],
                )
            })
            .collect();
        Self {
            state: tokio::sync::Mutex::new(RateState {
                aggregate,
                interfaces,
            }),
            burst_ms: config.burst_ms,
        }
    }

    async fn update_interface_rates(&self, updates: &[(u32, QosDirection, Option<u64>)]) {
        let mut state = self.state.lock().await;
        for (interface_index, direction, rate) in updates {
            let buckets = state
                .interfaces
                .entry(*interface_index)
                .or_insert_with(|| [None, None]);
            let slot = &mut buckets[direction.index()];
            *slot = rate.map(|rate| match slot.take() {
                Some(mut bucket) => {
                    bucket.update_rate(rate, self.burst_ms);
                    bucket
                }
                None => Bucket::new(rate, self.burst_ms),
            });
        }
    }

    async fn acquire(
        &self,
        interface_index: u32,
        direction: QosDirection,
        bytes: usize,
    ) -> io::Result<()> {
        let mut remaining = bytes;
        while remaining > 0 {
            let (wait, consumed) = {
                let mut state = self.state.lock().await;
                let now = Instant::now();
                let index = direction.index();
                let RateState {
                    aggregate,
                    interfaces,
                } = &mut *state;
                let mut global = aggregate[index].as_mut();
                let mut interface = interfaces
                    .get_mut(&interface_index)
                    .and_then(|buckets| buckets[index].as_mut());
                if let Some(bucket) = global.as_mut() {
                    bucket.refill(now);
                }
                if let Some(bucket) = interface.as_mut() {
                    bucket.refill(now);
                }
                let max_chunk = global
                    .as_ref()
                    .map(|bucket| bucket.capacity as usize)
                    .into_iter()
                    .chain(interface.as_ref().map(|bucket| bucket.capacity as usize))
                    .min()
                    .unwrap_or(remaining)
                    .max(1);
                let amount = remaining.min(max_chunk);
                let global_wait = global
                    .as_ref()
                    .map(|bucket| (**bucket).wait_for(amount))
                    .transpose()?
                    .unwrap_or(Duration::ZERO);
                let interface_wait = interface
                    .as_ref()
                    .map(|bucket| (**bucket).wait_for(amount))
                    .transpose()?
                    .unwrap_or(Duration::ZERO);
                let wait = global_wait.max(interface_wait);
                if wait.is_zero() {
                    if let Some(bucket) = global.as_mut() {
                        bucket.tokens -= amount as f64;
                    }
                    if let Some(bucket) = interface.as_mut() {
                        bucket.tokens -= amount as f64;
                    }
                    (wait, amount)
                } else {
                    (wait, 0)
                }
            };
            remaining -= consumed;
            if remaining > 0 && !wait.is_zero() {
                tokio::time::sleep(wait.min(Duration::from_millis(100))).await;
            }
        }
        Ok(())
    }

    fn max_burst(
        &self,
        direction: QosDirection,
        config: &QosConfig,
        effective_interface_rate: Option<u64>,
    ) -> usize {
        let index = direction.index();
        let capacity = |rate: Option<u64>| {
            rate.map(|rate| {
                ((rate as u128 * config.burst_ms as u128 / 1000).max(MIN_BURST_BYTES as u128))
                    .min(usize::MAX as u128) as usize
            })
        };
        let global = capacity(if index == 0 {
            config.aggregate.upload_bytes_per_sec
        } else {
            config.aggregate.download_bytes_per_sec
        });
        let per_interface = capacity(effective_interface_rate);
        match (global, per_interface) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => DEFAULT_TCP_CHUNK,
        }
    }
}

pub struct QosEngine {
    config: QosConfig,
    rates: RateLimiter,
    global_memory: Arc<Semaphore>,
    interface_memory: Mutex<HashMap<u32, Arc<Semaphore>>>,
    lanes: Mutex<HashMap<(u32, QosDirection), Arc<DrrLane>>>,
    effective_rates: Mutex<HashMap<(u32, QosDirection), Option<u64>>>,
}

impl QosEngine {
    pub fn new(config: QosConfig) -> io::Result<Arc<Self>> {
        config.validate()?;
        let effective_rates = config
            .interfaces
            .iter()
            .flat_map(|(index, rates)| {
                [
                    ((*index, QosDirection::Upload), rates.upload_bytes_per_sec),
                    (
                        (*index, QosDirection::Download),
                        rates.download_bytes_per_sec,
                    ),
                ]
            })
            .collect();
        Ok(Arc::new(Self {
            rates: RateLimiter::new(&config),
            global_memory: Arc::new(Semaphore::new(config.global_memory_limit_bytes)),
            interface_memory: Mutex::new(HashMap::new()),
            lanes: Mutex::new(HashMap::new()),
            effective_rates: Mutex::new(effective_rates),
            config,
        }))
    }

    pub fn mode(&self) -> QosMode {
        self.config.mode
    }

    pub async fn update_interface_rate(
        &self,
        interface_index: u32,
        direction: QosDirection,
        rate_bytes_per_sec: Option<u64>,
    ) -> io::Result<()> {
        if rate_bytes_per_sec == Some(0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "adaptive rate must be positive or unlimited",
            ));
        }
        self.update_interface_rates_atomic(&[(interface_index, direction, rate_bytes_per_sec)])
            .await
    }

    pub async fn update_interface_rates_atomic(
        &self,
        updates: &[(u32, QosDirection, Option<u64>)],
    ) -> io::Result<()> {
        if updates.iter().any(|(_, _, rate)| *rate == Some(0)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "QoS rates must be positive or unlimited",
            ));
        }
        self.rates.update_interface_rates(updates).await;
        let mut effective = self
            .effective_rates
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (interface_index, direction, rate) in updates {
            effective.insert((*interface_index, *direction), *rate);
        }
        Ok(())
    }

    pub fn effective_rates_snapshot(&self) -> HashMap<(u32, QosDirection), Option<u64>> {
        self.effective_rates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn session(
        self: &Arc<Self>,
        flow_id: u64,
        interface_index: u32,
        class: TrafficClass,
    ) -> QosSession {
        QosSession {
            inner: Arc::new(QosSessionInner {
                engine: self.clone(),
                flow_id,
                interface_index,
                class: class.into(),
            }),
        }
    }

    pub fn diagnostics(&self) -> Vec<QosLaneDiagnostics> {
        let lanes = self.lanes.lock().unwrap_or_else(|e| e.into_inner());
        let mut output: Vec<_> = lanes.values().map(|lane| lane.diagnostics()).collect();
        output.sort_by_key(|row| (row.interface_index, row.direction.index()));
        output
    }

    pub fn current_memory_bytes(&self) -> usize {
        self.config
            .global_memory_limit_bytes
            .saturating_sub(self.global_memory.available_permits())
    }

    fn max_request_bytes(&self, interface_index: u32, direction: QosDirection) -> usize {
        let effective = self
            .effective_rates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(interface_index, direction))
            .copied()
            .flatten();
        self.rates.max_burst(direction, &self.config, effective)
    }

    fn lane(self: &Arc<Self>, interface_index: u32, direction: QosDirection) -> Arc<DrrLane> {
        let mut lanes = self.lanes.lock().unwrap_or_else(|e| e.into_inner());
        lanes
            .entry((interface_index, direction))
            .or_insert_with(|| {
                let mut memory = self
                    .interface_memory
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let interface_memory = memory
                    .entry(interface_index)
                    .or_insert_with(|| {
                        Arc::new(Semaphore::new(self.config.max_interface_queue_bytes))
                    })
                    .clone();
                let lane = DrrLane::new(&self.config, interface_index, direction, interface_memory);
                lane.start(self.clone());
                lane
            })
            .clone()
    }

    async fn acquire(
        self: &Arc<Self>,
        flow_id: u64,
        interface_index: u32,
        class: QosClass,
        direction: QosDirection,
        bytes: usize,
        wait_for_capacity: bool,
    ) -> io::Result<Option<QosPermit>> {
        self.lane(interface_index, direction)
            .acquire(self, flow_id, class, bytes, wait_for_capacity)
            .await
    }

    fn unregister(&self, flow_id: u64) {
        for lane in self
            .lanes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            lane.unregister(flow_id);
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeQosRate {
    pub interface_guid: String,
    pub direction: QosDirection,
    pub rate_bytes_per_sec: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeQosPolicySnapshot {
    pub policy_version: u64,
    pub updated_at_unix_ms: u64,
    pub rates: Vec<RuntimeQosRate>,
}

pub struct QosPolicyManager {
    engine: Arc<QosEngine>,
    interfaces: Arc<crate::interface::InterfacePool>,
    current: std::sync::RwLock<RuntimeQosPolicySnapshot>,
    previous: Mutex<Option<RuntimeQosPolicySnapshot>>,
    update_lock: tokio::sync::Mutex<()>,
}

impl QosPolicyManager {
    pub fn new(engine: Arc<QosEngine>, interfaces: Arc<crate::interface::InterfacePool>) -> Self {
        let effective = engine.effective_rates_snapshot();
        let mut rates = effective
            .into_iter()
            .filter_map(|((index, direction), rate_bytes_per_sec)| {
                interfaces.guid_for_index(index).map(|guid| RuntimeQosRate {
                    interface_guid: guid,
                    direction,
                    rate_bytes_per_sec,
                })
            })
            .collect::<Vec<_>>();
        rates.sort_by(|a, b| {
            a.interface_guid
                .cmp(&b.interface_guid)
                .then_with(|| a.direction.index().cmp(&b.direction.index()))
        });
        Self {
            engine,
            interfaces,
            current: std::sync::RwLock::new(RuntimeQosPolicySnapshot {
                policy_version: 1,
                updated_at_unix_ms: unix_ms(),
                rates,
            }),
            previous: Mutex::new(None),
            update_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn snapshot(&self) -> RuntimeQosPolicySnapshot {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub async fn set_interface_rate(
        &self,
        guid: &str,
        direction: QosDirection,
        rate_bytes_per_sec: Option<u64>,
    ) -> io::Result<u64> {
        let _update_guard = self.update_lock.lock().await;
        if self.engine.mode() == QosMode::Adaptive {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "adaptive mode owns effective rates; change capacity or adaptive policy instead",
            ));
        }
        if rate_bytes_per_sec == Some(0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "rate must be positive or unlimited",
            ));
        }
        let canonical = normalize_guid(guid);
        if !self.interfaces.contains_guid(&canonical) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "adapter GUID is not configured",
            ));
        }
        let old = self.snapshot();
        let mut candidate = old.clone();
        if let Some(row) = candidate.rates.iter_mut().find(|row| {
            normalize_guid(&row.interface_guid) == canonical && row.direction == direction
        }) {
            row.rate_bytes_per_sec = rate_bytes_per_sec;
        } else {
            candidate.rates.push(RuntimeQosRate {
                interface_guid: canonical.clone(),
                direction,
                rate_bytes_per_sec,
            });
        }
        self.commit(candidate, old).await
    }

    async fn commit(
        &self,
        mut candidate: RuntimeQosPolicySnapshot,
        old: RuntimeQosPolicySnapshot,
    ) -> io::Result<u64> {
        let updates = candidate
            .rates
            .iter()
            .filter_map(|row| {
                self.interfaces
                    .current_index_for_guid(&row.interface_guid)
                    .map(|index| (index, row.direction, row.rate_bytes_per_sec))
            })
            .collect::<Vec<_>>();
        self.engine.update_interface_rates_atomic(&updates).await?;
        candidate.policy_version = old.policy_version.saturating_add(1);
        candidate.updated_at_unix_ms = unix_ms();
        *self.previous.lock().unwrap_or_else(|e| e.into_inner()) = Some(old);
        let version = candidate.policy_version;
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = candidate;
        Ok(version)
    }

    pub async fn rollback(&self) -> io::Result<u64> {
        let _update_guard = self.update_lock.lock().await;
        let old = self.snapshot();
        let mut prior = self
            .previous
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "no previous QoS policy is available",
                )
            })?;
        let updates = prior
            .rates
            .iter()
            .filter_map(|row| {
                self.interfaces
                    .current_index_for_guid(&row.interface_guid)
                    .map(|index| (index, row.direction, row.rate_bytes_per_sec))
            })
            .collect::<Vec<_>>();
        if let Err(error) = self.engine.update_interface_rates_atomic(&updates).await {
            *self.previous.lock().unwrap_or_else(|e| e.into_inner()) = Some(prior);
            return Err(error);
        }
        prior.policy_version = old.policy_version.saturating_add(1);
        prior.updated_at_unix_ms = unix_ms();
        let version = prior.policy_version;
        *self.previous.lock().unwrap_or_else(|e| e.into_inner()) = Some(old);
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = prior;
        Ok(version)
    }

    pub async fn note_adaptive_rate(&self, guid: &str, direction: QosDirection, rate: u64) {
        let _update_guard = self.update_lock.lock().await;
        let canonical = normalize_guid(guid);
        let mut current = self.current.write().unwrap_or_else(|e| e.into_inner());
        if let Some(row) = current.rates.iter_mut().find(|row| {
            normalize_guid(&row.interface_guid) == canonical && row.direction == direction
        }) {
            row.rate_bytes_per_sec = Some(rate);
        }
        current.updated_at_unix_ms = unix_ms();
    }
}

fn normalize_guid(guid: &str) -> String {
    guid.trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .to_ascii_lowercase()
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn token_bucket_allows_initial_burst_then_paces_at_configured_rate() {
        let config = QosConfig {
            mode: QosMode::Fixed,
            burst_ms: 100,
            aggregate: InterfaceRates {
                upload_bytes_per_sec: Some(10_000),
                ..InterfaceRates::default()
            },
            max_flow_queue_bytes: 10_000,
            max_interface_queue_bytes: 10_000,
            global_memory_limit_bytes: 10_000,
            ..QosConfig::default()
        };
        let engine = QosEngine::new(config).unwrap();
        let session = engine.session(1, 8, TrafficClass::Bulk);
        let burst = session
            .acquire_tcp(QosDirection::Upload, 1_000)
            .await
            .unwrap();
        drop(burst);
        let started = tokio::time::Instant::now();
        let paced = session
            .acquire_tcp(QosDirection::Upload, 1_000)
            .await
            .unwrap();
        assert!(tokio::time::Instant::now().duration_since(started) >= Duration::from_millis(45));
        assert_eq!(paced.bytes, 1_000);
    }

    #[tokio::test]
    async fn drr_preserves_flow_fairness_and_cleans_empty_flows() {
        let config = QosConfig {
            mode: QosMode::MonitorOnly,
            quantum_bytes: 100,
            max_flow_queue_bytes: 1000,
            max_interface_queue_bytes: 4000,
            global_memory_limit_bytes: 8000,
            ..QosConfig::default()
        };
        let engine = QosEngine::new(config).unwrap();
        let one = engine.session(1, 8, TrafficClass::Balanced);
        let two = engine.session(2, 8, TrafficClass::Balanced);
        let a = one.acquire_tcp(QosDirection::Upload, 100).await.unwrap();
        let b = two.acquire_tcp(QosDirection::Upload, 100).await.unwrap();
        assert_eq!(a.bytes + b.bytes, 200);
        drop(a);
        drop(b);
        drop(one);
        assert!(
            engine
                .diagnostics()
                .iter()
                .all(|lane| lane.active_flows <= 1)
        );
    }

    #[tokio::test]
    async fn drr_services_fifty_concurrent_flows_without_starvation() {
        let config = QosConfig {
            mode: QosMode::MonitorOnly,
            quantum_bytes: 1500,
            max_flow_queue_bytes: 32 * 1024,
            max_interface_queue_bytes: 2 * 1024 * 1024,
            global_memory_limit_bytes: 4 * 1024 * 1024,
            ..QosConfig::default()
        };
        let engine = QosEngine::new(config).unwrap();
        let mut tasks = tokio::task::JoinSet::new();
        for flow_id in 1..=50 {
            let session = engine.session(flow_id, 8, TrafficClass::Balanced);
            tasks.spawn(async move {
                let mut bytes = 0;
                for _ in 0..10 {
                    let permit = session
                        .acquire_tcp(QosDirection::Upload, 1200)
                        .await
                        .unwrap();
                    bytes += permit.bytes;
                }
                bytes
            });
        }
        let mut completed = 0;
        while let Some(result) = tasks.join_next().await {
            assert_eq!(result.unwrap(), 12_000);
            completed += 1;
        }
        assert_eq!(completed, 50);
    }

    #[test]
    fn config_rejects_zero_rate_values_and_fixed_mode_without_a_limit() {
        let mut config = QosConfig::default();
        config.aggregate.upload_bytes_per_sec = Some(0);
        assert!(config.validate().is_err());
        config.aggregate.upload_bytes_per_sec = None;
        config.mode = QosMode::Fixed;
        assert!(config.validate().is_err());
    }

    #[test]
    fn udp_codel_inspired_logic_ignores_transients_and_requires_persistent_delay() {
        let config = UdpAqmConfig {
            enabled: true,
            target_delay: Duration::from_millis(5),
            interval: Duration::from_millis(100),
        };
        let start = Instant::now();
        let mut state = CodelState::default();
        assert!(!state.should_drop(start, Duration::from_millis(80), 1000, config));
        assert!(!state.should_drop(start, Duration::from_millis(80), 4000, config));
        assert!(!state.should_drop(
            start + Duration::from_millis(99),
            Duration::from_millis(80),
            4000,
            config
        ));
        assert!(state.should_drop(
            start + Duration::from_millis(100),
            Duration::from_millis(80),
            4000,
            config
        ));
        assert!(state.should_drop(
            start + Duration::from_millis(200),
            Duration::from_millis(80),
            4000,
            config
        ));
        assert!(state.should_drop(
            start + Duration::from_millis(271),
            Duration::from_millis(80),
            4000,
            config
        ));
        assert!(!state.should_drop(
            start + Duration::from_millis(272),
            Duration::from_millis(1),
            4000,
            config
        ));
        assert!(!state.dropping);
    }

    #[tokio::test]
    async fn udp_oversize_and_queue_overflow_drop_whole_datagram() {
        let config = QosConfig {
            mode: QosMode::MonitorOnly,
            max_flow_queue_bytes: 100,
            max_interface_queue_bytes: 100,
            global_memory_limit_bytes: 100,
            ..QosConfig::default()
        };
        let engine = QosEngine::new(config).unwrap();
        let session = engine.session(1, 8, TrafficClass::Dns);
        assert!(
            session
                .acquire_udp(QosDirection::Upload, 101)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            session
                .acquire_udp(QosDirection::Upload, 100)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(engine.diagnostics()[0].udp_drops, 1);
    }

    #[tokio::test]
    async fn runtime_rate_update_is_versioned_and_rolls_back_atomically() {
        let guid = "aabbccdd-0000-0000-0000-000000000001";
        let engine = QosEngine::new(QosConfig {
            mode: QosMode::Fixed,
            interfaces: HashMap::from([(
                8,
                InterfaceRates {
                    upload_bytes_per_sec: Some(1000),
                    download_bytes_per_sec: Some(2000),
                },
            )]),
            ..QosConfig::default()
        })
        .unwrap();
        let interfaces = Arc::new(
            crate::interface::InterfacePool::new_stable(
                vec![crate::interface::InterfaceIdentity {
                    guid: guid.into(),
                    luid: 88,
                    index: 8,
                    alias: "USB".into(),
                    operational: true,
                }],
                Duration::from_secs(1),
            )
            .unwrap(),
        );
        let policy = QosPolicyManager::new(engine.clone(), interfaces);
        assert_eq!(policy.snapshot().policy_version, 1);
        assert_eq!(
            policy
                .set_interface_rate(guid, QosDirection::Upload, Some(3000))
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            engine.effective_rates_snapshot()[&(8, QosDirection::Upload)],
            Some(3000)
        );
        assert_eq!(policy.rollback().await.unwrap(), 3);
        assert_eq!(
            engine.effective_rates_snapshot()[&(8, QosDirection::Upload)],
            Some(1000)
        );
    }

    #[tokio::test]
    async fn rate_lowering_does_not_fail_an_already_large_tcp_request() {
        let engine = QosEngine::new(QosConfig {
            mode: QosMode::Fixed,
            burst_ms: 50,
            interfaces: HashMap::from([(
                8,
                InterfaceRates {
                    upload_bytes_per_sec: Some(1_000_000),
                    ..InterfaceRates::default()
                },
            )]),
            ..QosConfig::default()
        })
        .unwrap();
        engine
            .update_interface_rate(8, QosDirection::Upload, Some(100_000))
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            engine.rates.acquire(8, QosDirection::Upload, 16 * 1024),
        )
        .await
        .unwrap()
        .unwrap();
    }
}
