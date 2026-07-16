use crate::state::{
    AutoCapRun, CAP_TRACE_SAMPLE_SOURCE_HEALTHY, CAP_TRACE_SAMPLE_SOURCE_HELD,
    CAP_TRACE_SAMPLE_SOURCE_POLICY_FLOOR, CapTrace, GcMetrics,
};

pub(crate) const GC_METRICS_WINDOW: usize = 20;
pub(crate) const MIN_HEADROOM_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB cold-start cushion
pub(crate) const MIN_STEADY_HEADROOM_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB steady cushion
pub(crate) const MAX_GROWTH_FACTOR_PER_RUN_PCT: u64 = 10;
pub(crate) const MAX_SHRINK_FACTOR_PER_RUN_PCT: u64 = 10;
pub(crate) const GROWTH_DEADBAND_PCT: u64 = 5;
pub(crate) const HARD_CEILING_MIN_FINALS: usize = 3;

// Two observations are the smallest possible confirmation that a policy
// floor is recurring rather than an isolated build.
pub(crate) const POLICY_FLOOR_CONFIRMATIONS: usize = 2;

struct SizingHistory {
    healthy_finals: Vec<u64>,
    final_growths: Vec<u64>,
    trailing_policy_floors: Vec<u64>,
    ignored_over_cap_count: usize,
}

impl SizingHistory {
    fn new(runs: &[AutoCapRun]) -> Self {
        let healthy_finals: Vec<u64> = runs
            .iter()
            .filter(|run| run.is_healthy())
            .map(|run| run.final_size)
            .collect();
        let ignored_over_cap_count = runs.len() - healthy_finals.len();
        let final_growths = final_growths(&healthy_finals);
        let trailing_policy_floors = runs
            .iter()
            .rev()
            .take_while(|run| run.proves_cap_unattainable())
            .map(AutoCapRun::policy_floor)
            .collect();

        Self {
            healthy_finals,
            final_growths,
            trailing_policy_floors,
            ignored_over_cap_count,
        }
    }

    fn baseline(&self) -> u64 {
        baseline_from_finals(&self.healthy_finals)
    }

    fn observed_growth(&self) -> u64 {
        percentile(&self.final_growths, 90)
    }

    fn observed_growth_pct(&self) -> u64 {
        self.observed_growth()
            .saturating_mul(100)
            .checked_div(self.baseline())
            .unwrap_or(0)
    }

    fn confirmed_policy_floor(&self) -> Option<u64> {
        (self.trailing_policy_floors.len() >= POLICY_FLOOR_CONFIRMATIONS)
            .then(|| lower_median(&self.trailing_policy_floors))
    }

    fn trace(
        &self,
        baseline: u64,
        growth_budget: u64,
        clamp_reason: impl Into<String>,
        sample_source: &'static str,
    ) -> CapTrace {
        let sample_count = if sample_source == CAP_TRACE_SAMPLE_SOURCE_POLICY_FLOOR {
            self.trailing_policy_floors.len()
        } else {
            self.healthy_finals.len()
        };

        CapTrace {
            baseline,
            growth_budget,
            observed_growth_pct: self.observed_growth_pct(),
            clamp_reason: clamp_reason.into(),
            sample_source: sample_source.to_string(),
            sample_count: sample_count as u32,
            ignored_over_cap_sample_count: self.ignored_over_cap_count as u32,
            policy_floor: self.trailing_policy_floors.first().copied().unwrap_or(0),
            policy_floor_sample_count: self.trailing_policy_floors.len() as u32,
        }
    }
}

pub(crate) fn push_bounded<T>(vec: &mut Vec<T>, value: T) {
    vec.push(value);
    if vec.len() > GC_METRICS_WINDOW {
        let overflow = vec.len() - GC_METRICS_WINDOW;
        vec.drain(0..overflow);
    }
}

pub(crate) fn suggest_max_target_size(
    metrics: &GcMetrics,
    seed_from_current: Option<u64>,
) -> Option<(u64, CapTrace)> {
    if metrics.recent_auto_cap_runs.is_empty() {
        let baseline = seed_from_current?;
        return Some((
            baseline.saturating_add(MIN_HEADROOM_BYTES),
            CapTrace {
                baseline,
                growth_budget: MIN_HEADROOM_BYTES,
                clamp_reason: "cold-start".to_string(),
                sample_source: CAP_TRACE_SAMPLE_SOURCE_HELD.to_string(),
                ..Default::default()
            },
        ));
    }

    let history = SizingHistory::new(&metrics.recent_auto_cap_runs);

    if let Some(policy_floor) = history.confirmed_policy_floor() {
        let baseline = history.baseline().max(policy_floor);
        let recovery_target = baseline.saturating_add(MIN_HEADROOM_BYTES);
        // Recovery may bypass the normal 10% movement limit, but only up to a
        // repeatedly measured policy floor plus the normal cold-start cushion.
        // Eligible and unrecognized bytes never contribute to this target.
        let cap = metrics
            .last_suggested_cap
            .map_or(recovery_target, |previous| previous.max(recovery_target));

        return Some((
            cap,
            history.trace(
                baseline,
                MIN_HEADROOM_BYTES,
                "recovery:confirmed-policy-floor",
                CAP_TRACE_SAMPLE_SOURCE_POLICY_FLOOR,
            ),
        ));
    }

    let Some(previous_cap) = metrics.last_suggested_cap else {
        let baseline = history.baseline();
        let growth_budget = growth_budget_from_growths(&history.final_growths, false);
        return Some((
            baseline.saturating_add(growth_budget),
            history.trace(
                baseline,
                growth_budget,
                "cold-start",
                CAP_TRACE_SAMPLE_SOURCE_HEALTHY,
            ),
        ));
    };

    if metrics
        .recent_auto_cap_runs
        .last()
        .is_some_and(|run| !run.is_healthy())
        || history.healthy_finals.is_empty()
    {
        return Some((
            previous_cap,
            history.trace(
                previous_cap,
                0,
                "over-cap/hold",
                CAP_TRACE_SAMPLE_SOURCE_HELD,
            ),
        ));
    }

    let baseline = history.baseline();
    let growth_budget = growth_budget_from_growths(&history.final_growths, true);
    let mut proposed = baseline.saturating_add(growth_budget);
    let mut clamp_reason = "within-window".to_string();

    let mut non_zero_finals: Vec<u64> = history
        .healthy_finals
        .iter()
        .copied()
        .filter(|value| *value > 0)
        .collect();
    if non_zero_finals.len() >= HARD_CEILING_MIN_FINALS {
        non_zero_finals.sort_unstable();
        let hard_ceiling = percentile(&non_zero_finals, 75).saturating_mul(2);
        if proposed > hard_ceiling {
            proposed = hard_ceiling;
            clamp_reason = "hard-ceiling".to_string();
        }
    }

    let observed_growth = history.observed_growth();
    if observed_growth == 0 && baseline >= previous_cap
        || observed_growth > 0 && history.observed_growth_pct() <= GROWTH_DEADBAND_PCT
    {
        proposed = previous_cap;
        clamp_reason = "deadband/hold".to_string();
    }

    let max_up = previous_cap
        .saturating_add(previous_cap.saturating_mul(MAX_GROWTH_FACTOR_PER_RUN_PCT) / 100);
    let max_down = previous_cap
        .saturating_sub(previous_cap.saturating_mul(MAX_SHRINK_FACTOR_PER_RUN_PCT) / 100);
    let baseline_lower = baseline.min(max_up).min(previous_cap);
    let lower = max_down.max(baseline_lower).min(max_up);
    let clamped = proposed.clamp(lower, max_up);
    if clamped != proposed {
        clamp_reason = if clamped == max_up {
            "clamped:+growth"
        } else if clamped == max_down {
            "clamped:-shrink"
        } else {
            "clamped:baseline"
        }
        .to_string();
    }

    Some((
        clamped,
        history.trace(
            baseline,
            growth_budget,
            clamp_reason,
            CAP_TRACE_SAMPLE_SOURCE_HEALTHY,
        ),
    ))
}

pub(crate) fn cap_overage(final_size: u64, cap: u64) -> u64 {
    final_size.saturating_sub(cap)
}

pub(crate) fn record_auto_cap_outcome(metrics: &mut GcMetrics, run: AutoCapRun) {
    push_bounded(&mut metrics.recent_auto_cap_runs, run);
}

pub(crate) fn percentile(sorted: &[u64], p: u32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }

    let idx = (((sorted.len() - 1) as u128 * p as u128 + 50) / 100) as usize;
    sorted
        .get(idx)
        .copied()
        .or_else(|| sorted.last().copied())
        .unwrap_or(0)
}

fn baseline_from_finals(finals: &[u64]) -> u64 {
    let mut sorted = finals.to_vec();
    sorted.sort_unstable();
    percentile(&sorted, 50)
}

fn lower_median(values: &[u64]) -> u64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted
        .get(sorted.len().saturating_sub(1) / 2)
        .copied()
        .unwrap_or(0)
}

fn growth_budget_from_growths(growths: &[u64], has_previous_cap: bool) -> u64 {
    if growths.is_empty() {
        return if has_previous_cap {
            MIN_STEADY_HEADROOM_BYTES
        } else {
            MIN_HEADROOM_BYTES
        };
    }

    percentile(growths, 90).max(if has_previous_cap {
        MIN_STEADY_HEADROOM_BYTES
    } else {
        MIN_HEADROOM_BYTES
    })
}

fn final_growths(finals: &[u64]) -> Vec<u64> {
    let mut growths: Vec<u64> = finals
        .windows(2)
        .filter_map(|window| {
            window
                .get(1)
                .zip(window.first())
                .map(|(next, previous)| next.saturating_sub(*previous))
        })
        .collect();
    growths.sort_unstable();
    growths
}
