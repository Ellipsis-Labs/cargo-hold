use crate::state::{CapTrace, GcMetrics};

pub(crate) const GC_METRICS_WINDOW: usize = 20;
pub(crate) const MIN_HEADROOM_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB safety cushion
pub(crate) const MIN_STEADY_HEADROOM_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB cushion once a cap exists
pub(crate) const MAX_GROWTH_FACTOR_PER_RUN_PCT: u64 = 10; // limit upward drift to +10% per run
pub(crate) const MAX_SHRINK_FACTOR_PER_RUN_PCT: u64 = 10; // limit downward drift to -10% per run
pub(crate) const GROWTH_DEADBAND_PCT: u64 = 5; // tolerate small oscillations without moving the cap
pub(crate) const HARD_CEILING_MIN_FINALS: usize = 3; // require enough history before clamping

pub(crate) fn push_bounded(vec: &mut Vec<u64>, value: u64) {
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
    let (seed, seeded_from_current) = match metrics.seed_initial_size {
        Some(seed) => (seed, false),
        None => (seed_from_current?, true),
    };

    let finals = finals_from_metrics(metrics, seed);
    let growths = growths_from_metrics(metrics, &finals, seed);
    let final_growths = positive_final_growths(&finals);
    let baseline = baseline_from_finals(&finals);
    let has_prev_cap = metrics.last_suggested_cap.is_some();
    let growth_budget = growth_budget_from_growths(&growths, has_prev_cap);

    let mut proposed = baseline.saturating_add(growth_budget);

    let mut clamp_reason = "none".to_string();

    let cold_start_from_current = seeded_from_current
        && metrics.last_suggested_cap.is_none()
        && metrics.recent_initial_sizes.is_empty()
        && metrics.recent_bytes_freed.is_empty()
        && metrics.recent_final_sizes.is_empty();
    let mut non_zero_finals: Vec<u64> = finals.iter().copied().filter(|v| *v > 0).collect();
    if !cold_start_from_current && non_zero_finals.len() >= HARD_CEILING_MIN_FINALS {
        non_zero_finals.sort_unstable();
        let ceiling_base = percentile(&non_zero_finals, 75);
        let hard_ceiling = ceiling_base.saturating_mul(2);
        if proposed > hard_ceiling {
            proposed = hard_ceiling;
            clamp_reason = "hard-ceiling".to_string();
        }
    }

    if let Some(prev_cap) = metrics.last_suggested_cap {
        // If observed growth (based on finals) is within a deadband, hold the cap
        // steady.
        let observed_p90 = percentile(&final_growths, 90);
        let growth_pct = if baseline == 0 {
            0
        } else {
            observed_p90.saturating_mul(100) / baseline
        };

        if observed_p90 == 0 {
            // No observed positive growth; hold steady when baseline is at/above the cap,
            // otherwise allow the shrink clamp to apply.
            if baseline >= prev_cap {
                proposed = prev_cap;
                clamp_reason = "deadband/hold".to_string();
            }
        } else if growth_pct <= GROWTH_DEADBAND_PCT {
            proposed = prev_cap;
            clamp_reason = "deadband/hold".to_string();
        }

        let max_up = prev_cap + prev_cap.saturating_mul(MAX_GROWTH_FACTOR_PER_RUN_PCT) / 100;
        let max_down =
            prev_cap.saturating_sub(prev_cap.saturating_mul(MAX_SHRINK_FACTOR_PER_RUN_PCT) / 100);

        let baseline_lower = baseline.min(max_up).min(prev_cap);
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
        } else if clamp_reason == "none" {
            clamp_reason = "within-window".to_string();
        }
        proposed = clamped;
    } else {
        proposed = proposed.max(baseline);
        if clamp_reason == "none" {
            clamp_reason = "cold-start".to_string();
        }
    }

    let observed_growth_pct = if baseline == 0 {
        0
    } else {
        percentile(&final_growths, 90).saturating_mul(100) / baseline.max(1)
    };

    Some((
        proposed,
        CapTrace {
            baseline,
            growth_budget,
            observed_growth_pct,
            clamp_reason,
        },
    ))
}

pub(crate) fn percentile(sorted: &[u64], p: u32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }

    let idx = (((sorted.len() - 1) as u128 * p as u128 + 50) / 100) as usize;
    sorted.get(idx).copied().unwrap_or(*sorted.last().unwrap())
}

fn finals_from_metrics(metrics: &GcMetrics, seed: u64) -> Vec<u64> {
    let mut finals: Vec<u64> = if !metrics.recent_final_sizes.is_empty() {
        metrics.recent_final_sizes.clone()
    } else {
        let len = metrics
            .recent_initial_sizes
            .len()
            .min(metrics.recent_bytes_freed.len());

        (0..len)
            .map(|i| metrics.recent_initial_sizes[i].saturating_sub(metrics.recent_bytes_freed[i]))
            .collect()
    };

    if finals.is_empty() {
        finals.push(seed);
    }

    finals
}

fn growths_from_metrics(metrics: &GcMetrics, finals: &[u64], seed: u64) -> Vec<u64> {
    let len = finals.len().min(metrics.recent_initial_sizes.len());

    let mut growths: Vec<u64> = Vec::with_capacity(len.saturating_sub(1));
    for i in 1..len {
        let prev_final = finals.get(i - 1).copied().unwrap_or(seed);
        let init = metrics.recent_initial_sizes[i];
        growths.push(init.saturating_sub(prev_final));
    }

    growths
}

fn baseline_from_finals(finals: &[u64]) -> u64 {
    let mut finals_sorted = finals.to_vec();
    finals_sorted.sort_unstable();
    percentile(&finals_sorted, 50)
}

fn growth_budget_from_growths(growths: &[u64], has_prev_cap: bool) -> u64 {
    // Only consider positive growth when sizing headroom.
    let mut positives: Vec<u64> = growths.iter().copied().filter(|g| *g > 0).collect();

    if positives.is_empty() {
        // Steady-state: keep a small cushion instead of re-adding the full cold-start
        // headroom.
        return if has_prev_cap {
            MIN_STEADY_HEADROOM_BYTES
        } else {
            MIN_HEADROOM_BYTES
        };
    }

    positives.sort_unstable();
    let p90 = percentile(&positives, 90);

    if has_prev_cap {
        p90.max(MIN_STEADY_HEADROOM_BYTES)
    } else {
        p90.max(MIN_HEADROOM_BYTES)
    }
}

fn positive_final_growths(finals: &[u64]) -> Vec<u64> {
    let mut growths: Vec<u64> = finals
        .windows(2)
        .filter_map(|w| w.get(1).zip(w.first()).map(|(b, a)| b.saturating_sub(*a)))
        .filter(|g| *g > 0)
        .collect();

    growths.sort_unstable();
    growths
}
