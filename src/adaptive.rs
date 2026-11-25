use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct AdaptiveConfig {
    pub target_utilization: f64,
    pub decrease_factor: f64,
    pub increase_fraction: f64,
    pub target_queue_delay: Duration,
    pub recovery_queue_delay: Duration,
    pub max_change_fraction: f64,
    pub min_confidence: f64,
    pub congestion_samples: u32,
    pub recovery_samples: u32,
    pub cooldown: Duration,
}

impl Default for AdaptiveConfig {
    fn default() -> Self {
        Self {
            target_utilization: 0.90,
            decrease_factor: 0.85,
            increase_fraction: 0.05,
            target_queue_delay: Duration::from_millis(25),
            recovery_queue_delay: Duration::from_millis(10),
            max_change_fraction: 0.15,
            min_confidence: 0.6,
            congestion_samples: 3,
            recovery_samples: 5,
            cooldown: Duration::from_secs(4),
        }
    }
}

impl AdaptiveConfig {
    pub fn validate(self) -> Result<Self, &'static str> {
        if !self.target_utilization.is_finite()
            || !(0.1..=1.0).contains(&self.target_utilization)
            || !self.decrease_factor.is_finite()
            || !(0.5..1.0).contains(&self.decrease_factor)
            || !self.increase_fraction.is_finite()
            || !(0.001..=0.2).contains(&self.increase_fraction)
            || !self.max_change_fraction.is_finite()
            || !(0.01..=0.25).contains(&self.max_change_fraction)
            || !self.min_confidence.is_finite()
            || !(0.0..=1.0).contains(&self.min_confidence)
            || self.target_queue_delay.is_zero()
            || self.recovery_queue_delay >= self.target_queue_delay
            || self.congestion_samples == 0
            || self.recovery_samples == 0
            || self.cooldown.is_zero()
        {
            return Err("invalid adaptive QoS controller parameters");
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AdaptiveObservation {
    pub queue_delay: Option<Duration>,
    pub interface_error_ratio: Option<f64>,
    pub fresh: bool,
    pub confidence: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CongestionState {
    InsufficientData,
    Normal,
    Congested,
    Recovering,
}

#[derive(Clone, Copy, Debug)]
pub struct RateDecision {
    pub rate_bytes_per_sec: u64,
    pub state: CongestionState,
    pub confidence: f64,
    pub changed: bool,
}

pub struct AdaptiveController {
    config: AdaptiveConfig,
    capacity: u64,
    min_rate: u64,
    max_rate: u64,
    current_rate: u64,
    congested_count: u32,
    recovery_count: u32,
    last_change: Option<Duration>,
    state: CongestionState,
    confidence: f64,
}

impl AdaptiveController {
    pub fn new(
        config: AdaptiveConfig,
        capacity_bytes_per_sec: u64,
        min_rate_bytes_per_sec: u64,
        max_rate_bytes_per_sec: Option<u64>,
    ) -> Result<Self, &'static str> {
        let config = config.validate()?;
        if capacity_bytes_per_sec == 0 || min_rate_bytes_per_sec == 0 {
            return Err("adaptive QoS requires positive capacity and minimum rate");
        }
        let max_rate = max_rate_bytes_per_sec.unwrap_or(capacity_bytes_per_sec);
        if max_rate < min_rate_bytes_per_sec {
            return Err("adaptive maximum rate must be at least the minimum rate");
        }
        let target = (capacity_bytes_per_sec as f64 * config.target_utilization) as u64;
        let current_rate = target.clamp(min_rate_bytes_per_sec, max_rate);
        Ok(Self {
            config,
            capacity: capacity_bytes_per_sec,
            min_rate: min_rate_bytes_per_sec,
            max_rate,
            current_rate,
            congested_count: 0,
            recovery_count: 0,
            last_change: None,
            state: CongestionState::Normal,
            confidence: 0.0,
        })
    }

    pub fn current_rate(&self) -> u64 {
        self.current_rate
    }

    pub fn state(&self) -> CongestionState {
        self.state
    }

    pub fn confidence(&self) -> f64 {
        self.confidence
    }

    pub fn step(&mut self, now: Duration, observation: AdaptiveObservation) -> RateDecision {
        self.confidence = observation.confidence.clamp(0.0, 1.0);
        if !observation.fresh || observation.confidence < self.config.min_confidence {
            self.congested_count = 0;
            self.recovery_count = 0;
            self.state = CongestionState::InsufficientData;
            return self.decision(observation.confidence, false);
        }

        let queue = observation.queue_delay;
        let errors = observation.interface_error_ratio.unwrap_or(0.0);
        let congested =
            queue.is_some_and(|delay| delay >= self.config.target_queue_delay) || errors >= 0.02;
        let healthy =
            queue.is_some_and(|delay| delay <= self.config.recovery_queue_delay) && errors <= 0.005;

        if congested {
            self.congested_count = self.congested_count.saturating_add(1);
            self.recovery_count = 0;
            self.state = CongestionState::Congested;
        } else if healthy {
            self.recovery_count = self.recovery_count.saturating_add(1);
            self.congested_count = 0;
            self.state = CongestionState::Recovering;
        } else {
            self.congested_count = 0;
            self.recovery_count = 0;
            self.state = CongestionState::Normal;
        }

        let cooldown_passed = self
            .last_change
            .is_none_or(|last| now.saturating_sub(last) >= self.config.cooldown);
        let changed = if cooldown_passed && self.congested_count >= self.config.congestion_samples {
            let multiplicative = (self.current_rate as f64 * self.config.decrease_factor) as u64;
            let max_delta = (self.current_rate as f64 * self.config.max_change_fraction) as u64;
            self.current_rate = multiplicative
                .max(self.current_rate.saturating_sub(max_delta))
                .clamp(self.min_rate, self.max_rate);
            self.last_change = Some(now);
            self.congested_count = 0;
            self.state = CongestionState::Congested;
            true
        } else if cooldown_passed && self.recovery_count >= self.config.recovery_samples {
            let additive = (self.capacity as f64 * self.config.increase_fraction) as u64;
            let max_delta = (self.current_rate as f64 * self.config.max_change_fraction) as u64;
            let increase = additive.min(max_delta).max(1);
            let updated = self
                .current_rate
                .saturating_add(increase)
                .min(self.max_rate);
            let changed = updated > self.current_rate;
            self.current_rate = updated;
            self.last_change = Some(now);
            self.recovery_count = 0;
            changed
        } else {
            false
        };
        self.decision(observation.confidence, changed)
    }

    fn decision(&self, confidence: f64, changed: bool) -> RateDecision {
        RateDecision {
            rate_bytes_per_sec: self.current_rate,
            state: self.state,
            confidence,
            changed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(delay_ms: u64) -> AdaptiveObservation {
        AdaptiveObservation {
            queue_delay: Some(Duration::from_millis(delay_ms)),
            interface_error_ratio: Some(0.0),
            fresh: true,
            confidence: 0.9,
        }
    }

    #[test]
    fn requires_persistent_congestion_and_decreases_within_limits() {
        let mut controller = AdaptiveController::new(
            AdaptiveConfig::default(),
            12_500_000,
            125_000,
            Some(11_250_000),
        )
        .unwrap();
        let initial = controller.current_rate();
        for second in 1..3 {
            let rate = controller
                .step(Duration::from_secs(second), observed(80))
                .rate_bytes_per_sec;
            assert_eq!(rate, initial);
        }
        let decreased = controller
            .step(Duration::from_secs(3), observed(80))
            .rate_bytes_per_sec;
        assert!(decreased < initial);
        assert!(decreased >= initial * 85 / 100);
    }

    #[test]
    fn recovery_is_slow_bounded_and_stale_data_does_not_change_rate() {
        let mut controller =
            AdaptiveController::new(AdaptiveConfig::default(), 1_000_000, 100_000, Some(900_000))
                .unwrap();
        let initial = controller.current_rate();
        let stale = AdaptiveObservation {
            fresh: false,
            confidence: 0.0,
            ..observed(0)
        };
        let decision = controller.step(Duration::from_secs(60), stale);
        assert_eq!(decision.rate_bytes_per_sec, initial);
        assert_eq!(decision.state, CongestionState::InsufficientData);

        let mut decision = controller.step(Duration::from_secs(5), observed(1));
        for second in 6..10 {
            decision = controller.step(Duration::from_secs(second), observed(1));
        }
        assert!(decision.rate_bytes_per_sec >= initial);
        assert!(decision.rate_bytes_per_sec <= initial * 115 / 100);
    }

    #[test]
    fn capacity_is_required_and_configuration_is_validated() {
        assert!(AdaptiveController::new(AdaptiveConfig::default(), 0, 1, None).is_err());
        let bad = AdaptiveConfig {
            target_utilization: 1.5,
            ..AdaptiveConfig::default()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn long_simulation_stays_bounded_and_ramps_back_gradually() {
        let mut controller = AdaptiveController::new(
            AdaptiveConfig::default(),
            8_000_000,
            100_000,
            Some(7_200_000),
        )
        .unwrap();
        let max = 7_200_000;
        let mut history = Vec::new();
        for tick in 1..=300 {
            let decision = controller.step(Duration::from_secs(tick * 2), observed(100));
            history.push(decision.rate_bytes_per_sec);
        }
        assert!(history.windows(2).all(|pair| pair[1] <= pair[0]));
        assert!(history.iter().all(|rate| (100_000..=max).contains(rate)));
        let floor = controller.current_rate();
        assert_eq!(floor, 100_000);

        for tick in 301..=600 {
            let decision = controller.step(Duration::from_secs(tick * 2), observed(1));
            history.push(decision.rate_bytes_per_sec);
        }
        assert!(controller.current_rate() > floor);
        assert!(history.iter().all(|rate| *rate <= max));
        assert!(history[300..].windows(2).all(|pair| pair[1] >= pair[0]));
    }
}
