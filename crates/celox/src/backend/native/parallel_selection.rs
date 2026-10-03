//! Experimental, bounded online comparison against the independently optimized serial code.
//! Samples advance the real simulation; no cycle is replayed for benchmarking.
#[derive(Debug)]
pub(super) struct Selector {
    window: u64,
    warmup: u64,
    warmup_left: u64,
    cooldown: u64,
    phase: usize,
    calls: u64,
    nanos: u128,
    samples: Vec<u128>,
    sample_calls: Vec<u64>,
    candidates: Vec<usize>,
    selected: usize,
    holding: bool,
    hold_limit: u64,
    retained_on_drift: bool,
    pub rounds: u64,
    max_workers: usize,
    min_gain_percent: u128,
    horizon: Option<u64>,
    investment_ns: u128,
    total_calls: u64,
    hold_reference_ns: u128,
    hold_sample_ns: u128,
    hold_sample_calls: u64,
    hold_elapsed_ns: u128,
    slow_windows: u64,
    hold_slowdown_percent: u128,
    early_reprobes: u64,
}
impl Selector {
    pub fn new(window: u64, cooldown: u64) -> Self {
        Self {
            window: window.max(1),
            warmup: 0,
            warmup_left: 0,
            cooldown: cooldown.max(1),
            phase: 0,
            calls: 0,
            nanos: 0,
            samples: vec![0; 5],
            sample_calls: vec![0; 5],
            candidates: vec![1, 2, 1, 4, 1],
            selected: 1,
            holding: false,
            hold_limit: cooldown.max(1),
            retained_on_drift: false,
            rounds: 0,
            max_workers: 4,
            min_gain_percent: 10,
            horizon: None,
            investment_ns: 0,
            total_calls: 0,
            hold_reference_ns: 0,
            hold_sample_ns: 0,
            hold_sample_calls: 0,
            hold_elapsed_ns: 0,
            slow_windows: 0,
            hold_slowdown_percent: 0,
            early_reprobes: 0,
        }
    }
    pub fn configure(
        mut self,
        max_workers: usize,
        min_gain_percent: u128,
        horizon: Option<u64>,
        investment_ns: u128,
    ) -> Self {
        self.max_workers = max_workers.max(1);
        self.candidates = vec![1];
        let mut width = 2;
        while width <= self.max_workers {
            self.candidates.extend([width, 1]);
            let Some(next) = width.checked_mul(2) else {
                break;
            };
            width = next;
        }
        // Include a non-power-of-two budget, e.g. six independent groups/cores.
        if self.max_workers > 1 && !self.candidates.contains(&self.max_workers) {
            self.candidates.extend([self.max_workers, 1]);
        }
        self.samples = vec![0; self.candidates.len()];
        self.sample_calls = vec![0; self.candidates.len()];
        self.min_gain_percent = min_gain_percent.clamp(1, 99);
        self.horizon = horizon;
        self.investment_ns = investment_ns;
        self
    }
    /// An optional measured candidate set; original optimized serial is implicit.
    /// Apply only while constructing a fresh selector, after the resource cap.
    pub fn with_candidates(mut self, widths: &[usize]) -> Self {
        assert!(
            !widths.is_empty() && widths.iter().all(|&w| w > 0),
            "CELOX_PARALLEL_CANDIDATES requires positive thread counts"
        );
        let widths = widths
            .iter()
            .copied()
            .filter(|&w| w > 1 && w <= self.max_workers)
            .collect::<std::collections::BTreeSet<_>>();
        self.max_workers = widths.last().copied().unwrap_or(1);
        self.candidates = vec![1];
        for width in widths {
            self.candidates.extend([width, 1]);
        }
        self.samples = vec![0; self.candidates.len()];
        self.sample_calls = vec![0; self.candidates.len()];
        self
    }
    /// Advance real cycles while a new pool/cache state warms up, excluding
    /// transition costs from the steady-state rate used for selection. Total
    /// application time and the optional remaining-work budget still count them.
    pub fn with_warmup(mut self, calls: u64) -> Self {
        self.warmup = calls;
        self.warmup_left = calls;
        self
    }
    /// Recheck a stale choice after sustained slowdown. Zero disables the guard
    /// for controlled comparisons against fixed-interval profiling.
    pub fn with_hold_guard(mut self, slowdown_percent: u128) -> Self {
        self.hold_slowdown_percent = slowdown_percent;
        self
    }
    pub fn width(&self) -> usize {
        if self.max_workers < 2 {
            return 1;
        }
        if self.holding {
            self.selected
        } else {
            self.candidates[self.phase]
        }
    }
    pub fn batch_limit(&self) -> u64 {
        if self.max_workers < 2 {
            return u64::MAX;
        }
        if !self.holding && self.warmup_left > 0 {
            return self.warmup_left;
        }
        (if self.holding {
            self.hold_limit
        } else {
            self.window
        })
        .saturating_sub(self.calls)
        .max(1)
    }
    pub fn needs_timing(&self) -> bool {
        self.max_workers > 1
            && if self.holding {
                self.hold_slowdown_percent > 0
            } else {
                self.warmup_left == 0
            }
    }
    pub fn observe(&mut self, nanos: u128) -> Option<usize> {
        self.observe_many(nanos, 1)
    }
    pub fn observe_many(&mut self, nanos: u128, count: u64) -> Option<usize> {
        debug_assert!(count <= self.batch_limit());
        if count == 0 {
            return None;
        }
        self.total_calls = self.total_calls.saturating_add(count);
        if self.max_workers < 2 {
            return None;
        }
        if !self.holding && self.warmup_left > 0 {
            self.warmup_left -= count;
            return None;
        }
        self.calls += count;
        if self.holding {
            // Timings already collected during the hold must not be discarded:
            // a spin pool can deteriorate sharply when another host job starts.
            // Require two slow windows and ten expected windows of residence,
            // rather than reacting to a single scheduling interruption. Use
            // elapsed cost for the residence floor so a severely stalled pool
            // does not have to finish ten already-slow windows before recovery.
            if self.hold_slowdown_percent > 0 && self.hold_reference_ns > 0 {
                self.hold_elapsed_ns = self.hold_elapsed_ns.saturating_add(nanos);
                self.hold_sample_ns = self.hold_sample_ns.saturating_add(nanos);
                self.hold_sample_calls = self.hold_sample_calls.saturating_add(count);
                if self.hold_sample_calls >= self.window {
                    let actual = self
                        .hold_sample_ns
                        .saturating_mul(u128::from(self.window))
                        .saturating_mul(100);
                    let limit = self
                        .hold_reference_ns
                        .saturating_mul(u128::from(self.hold_sample_calls))
                        .saturating_mul(100_u128.saturating_add(self.hold_slowdown_percent));
                    self.slow_windows = if actual > limit {
                        self.slow_windows.saturating_add(1)
                    } else {
                        0
                    };
                    self.hold_sample_ns = 0;
                    self.hold_sample_calls = 0;
                }
            }
            let early = self.slow_windows >= 2
                && self.hold_elapsed_ns >= self.hold_reference_ns.saturating_mul(10);
            if self.calls >= self.hold_limit || early {
                if early && self.calls < self.hold_limit {
                    self.early_reprobes = self.early_reprobes.saturating_add(1);
                }
                self.holding = false;
                self.phase = 0;
                self.calls = 0;
                self.warmup_left = self.warmup;
            }
            return None;
        }
        self.nanos = self.nanos.saturating_add(nanos);
        // Once a parallel trial has spent more than the entire preceding
        // serial window (+ the allowed drift), finishing it cannot demonstrate
        // a gain. Bound time spent on a badly stalled spin pool. Normalize the
        // partial sample below, while counting only cycles actually executed.
        let over_budget = self.phase % 2 == 1
            && self.samples[self.phase - 1] > 0
            && self.nanos > self.samples[self.phase - 1].saturating_mul(120) / 100;
        if self.calls < self.window && !over_budget {
            return None;
        }
        self.sample_calls[self.phase] = self.calls;
        self.samples[self.phase] =
            self.nanos.saturating_mul(u128::from(self.window)) / u128::from(self.calls);
        self.nanos = 0;
        self.calls = 0;
        self.phase += 1;
        let phase_count = self.candidates.len();
        if self.phase < phase_count {
            self.warmup_left = self.warmup;
            return None;
        }
        // Bracket each trial with serial windows. Reject >20% drift, then
        // require >=10% benefit over the faster serial bracket. Prefer fewer
        // workers unless the wider candidate brings another >=10% benefit.
        let mut best = 1;
        let serial_cost = (0..phase_count)
            .step_by(2)
            .map(|i| self.samples[i])
            .min()
            .unwrap();
        let mut best_cost = serial_cost;
        let mut valid = vec![false; phase_count];
        for phase in (1..phase_count).step_by(2) {
            let lo = self.samples[phase - 1].min(self.samples[phase + 1]);
            let hi = self.samples[phase - 1].max(self.samples[phase + 1]);
            if lo == 0 || hi.saturating_mul(100) > lo.saturating_mul(120) {
                continue;
            }
            valid[phase] = true;
            let cost = self.samples[phase];
            if cost.saturating_mul(100) <= best_cost.saturating_mul(100 - self.min_gain_percent) {
                best = self.candidates[phase];
                best_cost = cost;
            }
        }
        let previous_phase = self
            .candidates
            .iter()
            .position(|&w| w == self.selected)
            .unwrap();
        self.retained_on_drift = self.selected > 1 && !valid[previous_phase];
        if self.retained_on_drift {
            // Invalid evidence must not demote a previously measured winner.
            // Retry sooner instead of spending the entire cooldown on serial.
            best = self.selected;
            best_cost = serial_cost; // No fresh savings claim for optional payback filtering.
        }
        let unresolved = self.retained_on_drift || !valid.iter().any(|&v| v);
        self.hold_limit = if unresolved {
            self.cooldown.min(self.window.saturating_mul(10))
        } else {
            self.cooldown
        };
        if let Some(horizon) = self.horizon {
            let remaining = horizon.saturating_sub(self.total_calls);
            let projected_saving = serial_cost
                .saturating_sub(best_cost)
                .saturating_mul(u128::from(remaining))
                / u128::from(self.window);
            if projected_saving <= self.investment_ns {
                best = 1;
            }
        }
        self.selected = best;
        if !self.retained_on_drift || self.hold_reference_ns == 0 || best == 1 {
            // Use the latest serial window for a serial hold, rather than the
            // optimistic minimum used for conservative speedup comparison.
            let phase = if best == 1 {
                phase_count - 1
            } else {
                self.candidates.iter().position(|&w| w == best).unwrap()
            };
            self.hold_reference_ns = self.samples[phase];
        }
        self.hold_sample_ns = 0;
        self.hold_sample_calls = 0;
        self.hold_elapsed_ns = 0;
        self.slow_windows = 0;
        self.holding = true;
        self.rounds += 1;
        Some(best)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn trial(s: &mut Selector, costs: [u128; 5]) -> usize {
        for (phase, cost) in costs.into_iter().enumerate() {
            assert_eq!(s.width(), [1, 2, 1, 4, 1][phase]);
            assert_eq!(
                s.observe(cost),
                if phase == 4 { Some(s.width()) } else { None }
            );
        }
        s.width()
    }
    fn trial_wide(s: &mut Selector, costs: &[u128]) -> usize {
        assert_eq!(costs.len(), s.candidates.len());
        for (phase, &cost) in costs.iter().enumerate() {
            assert_eq!(s.width(), s.candidates[phase]);
            let selected = s.observe(cost);
            if phase + 1 == costs.len() {
                assert_eq!(selected, Some(s.width()));
            } else {
                assert_eq!(selected, None);
            }
        }
        s.width()
    }
    #[test]
    fn holds_without_clock_samples_until_the_next_calibration() {
        let mut s = Selector::new(1, 2).configure(2, 10, None, 0);
        assert!(s.needs_timing());
        trial_wide(&mut s, &[100, 50, 100]);
        assert!(!s.needs_timing());
        s.observe_many(0, 2);
        assert!(s.needs_timing());
        assert_eq!(s.width(), 1);

        let mut guarded = Selector::new(1, 2)
            .configure(2, 10, None, 0)
            .with_hold_guard(100);
        trial_wide(&mut guarded, &[100, 50, 100]);
        assert!(guarded.needs_timing());
        assert!(!Selector::new(1, 2).configure(1, 10, None, 0).needs_timing());
    }

    #[test]
    fn stops_a_slow_candidate_with_actual_cycle_accounting() {
        let mut s = Selector::new(100, 1000).configure(2, 10, None, 0);
        assert_eq!(s.observe_many(1000, 100), None);
        assert_eq!(s.width(), 2);
        assert_eq!(s.observe(500), None);
        assert_eq!(s.observe(500), None);
        assert_eq!(s.width(), 2);
        assert_eq!(s.observe(500), None);
        assert_eq!(s.width(), 1);
        assert_eq!(s.batch_limit(), 100);
        assert_eq!(s.observe_many(1000, 100), Some(1));
        assert_eq!(s.samples, vec![1000, 50_000, 1000]);
        assert_eq!(s.sample_calls, vec![100, 3, 100]);
        assert_eq!(s.total_calls, 203);
    }

    #[test]
    fn warmup_excludes_startup_but_counts_real_cycles_and_repeats_per_phase() {
        let mut s = Selector::new(2, 2).configure(2, 10, None, 0).with_warmup(3);
        for (phase, rate) in [100, 60, 100].into_iter().enumerate() {
            assert_eq!(s.batch_limit(), 3);
            assert!(!s.needs_timing());
            assert_eq!(s.observe(1_000_000), None);
            assert_eq!(s.batch_limit(), 2);
            assert_eq!(s.observe_many(2_000_000, 2), None);
            assert_eq!(s.batch_limit(), 2);
            assert!(s.needs_timing());
            assert_eq!(
                s.observe_many(rate * 2, 2),
                if phase == 2 { Some(2) } else { None }
            );
        }
        assert_eq!(s.total_calls, 15);
        assert_eq!(s.samples, vec![200, 120, 200]);
        s.observe_many(120, 2);
        assert_eq!(s.width(), 1);
        assert_eq!(s.batch_limit(), 3);
        assert_eq!(s.observe_many(9_000_000, 3), None);
        assert_eq!(s.total_calls, 20);
        assert_eq!(s.batch_limit(), 2);
    }
    #[test]
    fn sustained_hold_slowdown_reprobes_before_a_long_cooldown() {
        let mut s = Selector::new(1, 1000)
            .configure(8, 10, None, 0)
            .with_hold_guard(50);
        assert_eq!(trial_wide(&mut s, &[100, 80, 100, 60, 100, 40, 100]), 8);
        for _ in 0..3 {
            assert_eq!(s.observe(100), None);
            assert_eq!(s.width(), 8);
        }
        assert_eq!(s.observe(100), None);
        assert!(!s.holding);
        assert_eq!(s.early_reprobes, 1);
        assert_eq!(trial_wide(&mut s, &[100, 80, 100, 60, 100, 110, 100]), 4);
    }
    #[test]
    fn hold_guard_normalizes_batches_and_ignores_an_isolated_spike() {
        let mut s = Selector::new(1, 1000)
            .configure(8, 10, None, 0)
            .with_hold_guard(50);
        trial_wide(&mut s, &[100, 80, 100, 60, 100, 40, 100]);
        s.observe_many(400, 10); // 40 per call, not a tenfold regression.
        s.observe(100);
        assert!(s.holding);
        s.observe(40);
        assert_eq!(s.slow_windows, 0);
        s.observe(100);
        assert!(s.holding);
        s.observe(100);
        assert!(!s.holding);
    }
    #[test]
    fn hold_guard_is_opt_in_and_regular_reprofiling_still_runs() {
        let mut s = Selector::new(1, 100).configure(8, 10, None, 0);
        trial_wide(&mut s, &[100, 80, 100, 60, 100, 40, 100]);
        s.observe_many(10_000, 99);
        assert!(s.holding);
        assert_eq!(s.early_reprobes, 0);
        s.observe(100);
        assert!(!s.holding);
        assert_eq!(s.early_reprobes, 0);
    }
    #[test]
    fn eight_and_sixteen_are_measured_not_assumed_faster() {
        let make = || Selector::new(1, 3).configure(16, 10, None, 0);
        assert_eq!(
            trial_wide(&mut make(), &[100, 80, 100, 60, 100, 40, 100, 55, 100]),
            8
        );
        assert_eq!(
            trial_wide(&mut make(), &[100, 80, 100, 60, 100, 70, 100, 90, 100]),
            4
        );
        assert_eq!(
            trial_wide(&mut make(), &[100, 80, 100, 60, 100, 40, 100, 30, 100]),
            16
        );
    }
    #[test]
    fn non_power_of_two_budget_and_phase_change() {
        let mut s = Selector::new(1, 3).configure(6, 10, None, 0);
        assert_eq!(s.candidates, vec![1, 2, 1, 4, 1, 6, 1]);
        assert_eq!(trial_wide(&mut s, &[100, 80, 100, 60, 100, 40, 100]), 6);
        s.observe_many(120, 3);
        assert_eq!(trial_wide(&mut s, &[100, 70, 100, 85, 100, 95, 100]), 2);
    }
    #[test]
    fn wider_probe_noise_cannot_displace_a_measured_winner() {
        let mut s = Selector::new(1, 3).configure(8, 10, None, 0);
        assert_eq!(trial_wide(&mut s, &[100, 80, 100, 60, 100, 30, 160]), 4);
    }
    #[test]
    fn explicit_candidates_can_compare_six_against_eight_within_admission() {
        let mut s = Selector::new(1, 3)
            .configure(8, 10, None, 0)
            .with_candidates(&[16, 8, 6, 4, 2, 1, 6]);
        assert_eq!(s.candidates, vec![1, 2, 1, 4, 1, 6, 1, 8, 1]);
        assert_eq!(
            trial_wide(&mut s, &[100, 80, 100, 60, 100, 40, 100, 50, 100]),
            6
        );
        let mut serial = Selector::new(1, 3)
            .configure(2, 10, None, 0)
            .with_candidates(&[4, 8]);
        assert_eq!(serial.width(), 1);
        assert_eq!(serial.batch_limit(), u64::MAX);
        assert_eq!(serial.observe(100), None);
    }
    #[test]
    fn original_serial_wins_over_partition_overhead() {
        assert_eq!(
            trial(&mut Selector::new(1, 10), [100, 130, 100, 110, 100]),
            1
        );
    }
    #[test]
    fn wider_workers_must_earn_their_cost() {
        assert_eq!(trial(&mut Selector::new(1, 10), [100, 70, 100, 66, 100]), 2);
        assert_eq!(trial(&mut Selector::new(1, 10), [100, 70, 100, 50, 100]), 4);
    }
    #[test]
    fn unstable_brackets_reject_false_speedup() {
        assert_eq!(trial(&mut Selector::new(1, 10), [100, 50, 150, 60, 100]), 1);
    }
    #[test]
    fn cooldown_then_reprobe_can_return_to_serial() {
        let mut s = Selector::new(1, 3);
        assert_eq!(trial(&mut s, [100, 75, 100, 50, 100]), 4);
        for _ in 0..3 {
            assert_eq!(s.observe(50), None);
        }
        assert_eq!(trial(&mut s, [100, 130, 100, 110, 100]), 1);
    }
    #[test]
    fn short_horizon_cannot_repay_compilation() {
        let mut short = Selector::new(1, 10).configure(4, 10, Some(10), 1000);
        assert_eq!(trial(&mut short, [100, 70, 100, 50, 100]), 1);
        let mut long = Selector::new(1, 10).configure(4, 10, Some(100), 1000);
        assert_eq!(trial(&mut long, [100, 70, 100, 50, 100]), 4);
    }
    #[test]
    fn worker_budget_limits_exploration() {
        let mut s = Selector::new(1, 10).configure(2, 10, None, 0);
        assert_eq!(s.observe(100), None);
        assert_eq!(s.width(), 2);
        assert_eq!(s.observe(60), None);
        assert_eq!(s.width(), 1);
        assert_eq!(s.observe(100), Some(2));
        assert_eq!(s.width(), 2);
        let mut s = Selector::new(1, 10).configure(1, 10, None, 0);
        for _ in 0..10 {
            assert_eq!(s.observe(100), None);
            assert_eq!(s.width(), 1);
        }
    }
    #[test]
    fn noisy_reprobe_retains_known_winner_then_valid_regression_demotes_it() {
        let mut s = Selector::new(1, 100);
        assert_eq!(trial(&mut s, [100, 75, 100, 50, 100]), 4);
        s.observe_many(5000, 100);
        assert_eq!(trial(&mut s, [140, 100, 100, 50, 140]), 4);
        assert!(s.retained_on_drift);
        assert_eq!(s.batch_limit(), 10);
        s.observe_many(500, 10);
        assert_eq!(trial(&mut s, [100, 130, 100, 120, 100]), 1);
        assert!(!s.retained_on_drift);
    }
    #[test]
    fn native_batches_stop_at_sampling_boundaries() {
        let mut s = Selector::new(8, 16);
        assert_eq!(s.observe_many(400, 4), None);
        assert_eq!(s.batch_limit(), 4);
        assert_eq!(s.observe_many(400, 4), None);
        assert_eq!(s.width(), 2);
        assert_eq!(s.batch_limit(), 8);
        assert_eq!(s.observe_many(0, 0), None);
        assert_eq!(s.batch_limit(), 8);
    }
    #[test]
    fn accumulates_whole_windows() {
        let mut s = Selector::new(2, 10);
        assert_eq!(s.observe(100), None);
        assert_eq!(s.width(), 1);
        assert_eq!(s.observe(100), None);
        assert_eq!(s.width(), 2);
    }
}
