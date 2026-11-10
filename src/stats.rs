//! Latency statistics in constant memory.
//!
//! A full Surface Scan can make tens of millions of reads; instead of keeping every
//! latency, a logarithmic histogram (4% growth per bucket) gives percentiles with at most
//! ~4% error and fixed memory.

use serde::{Deserialize, Serialize};
use std::time::Duration;

const GROWTH: f64 = 1.04;
const LOG_BUCKETS: usize = 600;

/// Limits (ms) of the buckets shown to the user; the last bucket is "above 500 ms".
pub const DISPLAY_LIMITS_MS: [u64; 5] = [10, 25, 50, 150, 500];

#[derive(Debug, Clone)]
pub struct LatencyRecorder {
    buckets: Vec<u64>,
    display: [u64; 6],
    count: u64,
    sum_us: u128,
    min_us: u64,
    max_us: u64,
}

impl Default for LatencyRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyRecorder {
    pub fn new() -> Self {
        Self { buckets: vec![0; LOG_BUCKETS], display: [0; 6], count: 0, sum_us: 0, min_us: u64::MAX, max_us: 0 }
    }

    fn index(us: u64) -> usize {
        if us <= 1 { 0 } else { ((us as f64).ln() / GROWTH.ln()).ceil() as usize }.min(LOG_BUCKETS - 1)
    }

    pub fn record(&mut self, latency: Duration) {
        let us = latency.as_micros().min(u64::MAX as u128) as u64;
        self.buckets[Self::index(us)] += 1;
        let ms = us / 1000;
        let slot = DISPLAY_LIMITS_MS.iter().position(|&limit| ms < limit || (ms == limit && us % 1000 == 0)).unwrap_or(5);
        self.display[slot] += 1;
        self.count += 1;
        self.sum_us += us as u128;
        self.min_us = self.min_us.min(us);
        self.max_us = self.max_us.max(us);
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    /// Approximate percentile in microseconds (upper bound of the bucket).
    pub fn percentile_us(&self, p: f64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let target = ((p / 100.0) * self.count as f64).ceil().max(1.0) as u64;
        let mut acc = 0;
        for (i, &n) in self.buckets.iter().enumerate() {
            acc += n;
            if acc >= target {
                let upper = GROWTH.powi(i as i32).round() as u64;
                return upper.clamp(self.min_us, self.max_us);
            }
        }
        self.max_us
    }

    pub fn summary(&self) -> LatencySummary {
        let ms = |us: u64| us as f64 / 1000.0;
        let labels = ["≤ 10 ms", "≤ 25 ms", "≤ 50 ms", "≤ 150 ms", "≤ 500 ms", "> 500 ms"];
        let buckets = labels
            .iter()
            .enumerate()
            .map(|(i, label)| LatencyBucket { label: label.to_string(), upper_ms: DISPLAY_LIMITS_MS.get(i).copied(), count: self.display[i] })
            .collect();
        if self.count == 0 {
            return LatencySummary { buckets, ..Default::default() };
        }
        LatencySummary {
            count: self.count,
            min_ms: ms(self.min_us),
            avg_ms: (self.sum_us as f64 / self.count as f64) / 1000.0,
            p50_ms: ms(self.percentile_us(50.0)),
            p95_ms: ms(self.percentile_us(95.0)),
            p99_ms: ms(self.percentile_us(99.0)),
            max_ms: ms(self.max_us),
            buckets,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LatencyBucket {
    pub label: String,
    pub upper_ms: Option<u64>,
    pub count: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LatencySummary {
    pub count: u64,
    pub min_ms: f64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub buckets: Vec<LatencyBucket>,
}

impl LatencySummary {
    /// Number of reads above `ms`, using the display buckets.
    pub fn count_above(&self, ms: u64) -> u64 {
        self.buckets.iter().filter(|b| b.upper_ms.map(|u| u > ms).unwrap_or(true)).map(|b| b.count).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_summary_is_zeroed() {
        let s = LatencyRecorder::new().summary();
        assert_eq!(s.count, 0);
        assert_eq!(s.buckets.len(), 6);
        assert_eq!(s.max_ms, 0.0);
    }

    #[test]
    fn percentiles_are_close_to_exact() {
        let mut r = LatencyRecorder::new();
        for ms in 1..=1000u64 {
            r.record(Duration::from_millis(ms));
        }
        let s = r.summary();
        assert_eq!(s.count, 1000);
        assert!((s.p50_ms - 500.0).abs() / 500.0 < 0.05, "p50 = {}", s.p50_ms);
        assert!((s.p99_ms - 990.0).abs() / 990.0 < 0.05, "p99 = {}", s.p99_ms);
        assert_eq!(s.min_ms, 1.0);
        assert_eq!(s.max_ms, 1000.0);
        assert!((s.avg_ms - 500.5).abs() < 0.01);
    }

    #[test]
    fn display_buckets_split_correctly() {
        let mut r = LatencyRecorder::new();
        for ms in [1, 10, 11, 30, 100, 400, 501, 3000] {
            r.record(Duration::from_millis(ms));
        }
        let s = r.summary();
        let counts: Vec<u64> = s.buckets.iter().map(|b| b.count).collect();
        assert_eq!(counts, vec![2, 1, 1, 1, 1, 2]);
        assert_eq!(s.count_above(500), 2);
        assert_eq!(s.count_above(150), 3);
    }

    #[test]
    fn single_sample_percentiles_equal_sample() {
        let mut r = LatencyRecorder::new();
        r.record(Duration::from_micros(1234));
        let s = r.summary();
        assert_eq!(s.p50_ms, 1.234);
        assert_eq!(s.p99_ms, 1.234);
    }
}
