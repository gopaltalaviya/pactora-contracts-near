//! The rolling 24 h spend cap, ported line for line from v3
//! `AgentAccountCore` (`_chargeSpend`, `_roll`, `_windowSpent`,
//! `nextReleaseAt`). Pure functions over `Purse`, so they are tested without
//! a blockchain.

use near_sdk::require;

use crate::errors::*;
use crate::types::{Purse, BUCKETS, HOUR};

pub fn hour_of(now_s: u64) -> u64 {
    now_s / HOUR
}

/// Drop the buckets that left the window since `last_hour`.
pub fn roll(p: &mut Purse, hour: u64) {
    let last = p.last_hour;
    if hour == last {
        return;
    }
    if hour - last >= BUCKETS {
        p.buckets.iter_mut().for_each(|b| *b = 0);
        p.window_total = 0;
    } else {
        let mut total = p.window_total;
        for h in (last + 1)..=hour {
            let i = (h % BUCKETS) as usize;
            let b = p.buckets[i];
            if b != 0 {
                total -= b;
                p.buckets[i] = 0;
            }
        }
        p.window_total = total;
    }
    p.last_hour = hour;
}

/// Check the caps and record `spent`. Panics with the v3 error names.
pub fn charge(p: &mut Purse, spent: u128, now_s: u64) {
    require!(spent <= p.per_task_cap, PER_TASK_CAP_EXCEEDED);
    let hour = hour_of(now_s);
    roll(p, hour);
    let remaining = p.daily_cap.saturating_sub(p.window_total);
    require!(spent <= remaining, DAILY_CAP_EXCEEDED);
    p.window_total += spent;
    p.buckets[(hour % BUCKETS) as usize] += spent;
}

/// The window's spend as `roll` would leave it, without writing.
pub fn window_spent(p: &Purse, now_s: u64) -> u128 {
    let hour = hour_of(now_s);
    let last = p.last_hour;
    if hour - last >= BUCKETS {
        return 0;
    }
    let mut total = p.window_total;
    for h in (last + 1)..=hour {
        total -= p.buckets[(h % BUCKETS) as usize];
    }
    total
}

pub fn daily_remaining(p: &Purse, now_s: u64) -> u128 {
    p.daily_cap.saturating_sub(window_spent(p, now_s))
}

/// When the oldest counted spend leaves the window (seconds); 0 if none.
pub fn next_release_at(p: &Purse, now_s: u64) -> u64 {
    let hour = hour_of(now_s);
    let last = p.last_hour;
    if hour - last >= BUCKETS {
        return 0;
    }
    let first = if hour >= BUCKETS - 1 {
        hour - (BUCKETS - 1)
    } else {
        0
    };
    for h in first..=last {
        if p.buckets[(h % BUCKETS) as usize] != 0 {
            return (h + BUCKETS) * HOUR;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn purse(per_task: u128, daily: u128) -> Purse {
        Purse::new(per_task, daily)
    }

    #[test]
    fn per_task_cap_is_checked_before_the_window() {
        let mut p = purse(10, 100);
        let r = std::panic::catch_unwind(move || charge(&mut p, 11, 0));
        assert!(r.is_err());
    }

    #[test]
    fn daily_cap_counts_spends_inside_the_window() {
        let mut p = purse(60, 100);
        charge(&mut p, 60, 0);
        assert_eq!(daily_remaining(&p, 10 * HOUR), 40);
        let mut q = p.clone();
        assert!(std::panic::catch_unwind(move || charge(&mut q, 41, 10 * HOUR)).is_err());
        charge(&mut p, 40, 10 * HOUR);
        assert_eq!(daily_remaining(&p, 10 * HOUR), 0);
    }

    /// The bug v3 fixed (2× the cap across a fixed day boundary): a rolling
    /// window refuses the second full spend one hour later.
    #[test]
    fn no_double_spend_across_a_day_boundary() {
        let mut p = purse(100, 100);
        let day = 24 * HOUR;
        charge(&mut p, 100, day - 1);
        let mut q = p.clone();
        assert!(std::panic::catch_unwind(move || charge(&mut q, 1, day + 1)).is_err());
    }

    #[test]
    fn a_spend_leaves_the_window_after_24_to_25_hours() {
        let mut p = purse(100, 100);
        let t0 = 30 * HOUR + 1_800; // hour 30, half past
        charge(&mut p, 100, t0);
        // Hour 54 (24 h later, same bucket index minus one) still counts.
        assert_eq!(window_spent(&p, 54 * HOUR), 100);
        // Hour 55 is 25 buckets on: gone.
        assert_eq!(window_spent(&p, 55 * HOUR), 0);
        assert_eq!(next_release_at(&p, t0), 55 * HOUR);
    }

    #[test]
    fn long_idle_clears_everything() {
        let mut p = purse(100, 100);
        charge(&mut p, 50, 0);
        roll(&mut p, 1_000);
        assert_eq!(p.window_total, 0);
        assert!(p.buckets.iter().all(|b| *b == 0));
        assert_eq!(next_release_at(&p, 1_000 * HOUR), 0);
    }

    #[test]
    fn view_matches_what_roll_writes() {
        let mut p = purse(100, 1_000);
        for (i, t) in [0u64, 3, 7, 20, 24, 26, 40].iter().enumerate() {
            charge(&mut p, 10 + i as u128, t * HOUR);
        }
        for h in 40..80u64 {
            let seen = window_spent(&p, h * HOUR);
            let mut q = p.clone();
            roll(&mut q, h);
            assert_eq!(seen, q.window_total, "hour {h}");
        }
    }
}
