//! Share-based latency selection (`fastest_share`).
//!
//! `fastest` sends every pick to the target with the best latency score. Two
//! things make that a herd. Min-max scoring turns any gap between two targets,
//! however small, into a 1.0 against a 0.0, so the in-flight term cannot break
//! the tie. And the latency average learns of a queue only after the requests
//! in it finish, so the favoured target floods before its score moves.
//!
//! This strategy draws a target instead. A target's share is proportional to
//! `(1/latency)^2`, damped by its in-flight count, with a uniform floor so
//! every usable target keeps producing samples. A target that is much faster
//! takes most of the traffic, but not all of it, and the distance between two
//! latencies is kept rather than squashed to the ends of a range.

use std::sync::Arc;

use rand::RngExt;

use crate::scorer::LatencySource;
use crate::{LoadBalancer, RouteContext, Usable};

/// Exponent on `1/latency`. A target twice as fast earns four times the
/// share before damping.
const SPEED_EXPONENT: f64 = 2.0;

/// Share of picks spread uniformly over the usable targets, whatever their
/// latency, so a slow target keeps producing samples.
const EXPLORE_FLOOR: f64 = 0.05;

/// Latency-share balancer. `latency` is `None` on a pool with no latency
/// source (a provider group), where every target starts unknown and the
/// split follows in-flight load alone.
pub struct FastestShare {
    n: usize,
    latency: Option<Arc<dyn LatencySource>>,
}

impl FastestShare {
    pub fn new(n: usize, latency: Option<Arc<dyn LatencySource>>) -> Self {
        Self { n, latency }
    }

    fn choose(&self, loads: &[u64], usable: &Usable, draw: f64) -> Option<usize> {
        let latencies = match &self.latency {
            Some(source) => source.latencies(self.n),
            None => Vec::new(),
        };
        share_pick(self.n, usable, &latencies, loads, draw)
    }
}

impl LoadBalancer for FastestShare {
    fn name(&self) -> &'static str {
        "fastest_share"
    }

    fn reads_prompt(&self) -> bool {
        false
    }

    fn pick(&self, _ctx: &RouteContext, loads: &[u64]) -> Option<usize> {
        if self.n == 0 {
            return None;
        }
        let usable = Usable::ask(self.n, &|_| true);
        self.choose(loads, &usable, rand::rng().random::<f64>())
    }

    fn pick_eligible(
        &self,
        _ctx: &RouteContext,
        loads: &[u64],
        eligible: &dyn Fn(usize) -> bool,
    ) -> Option<usize> {
        if self.n == 0 {
            return None;
        }
        let usable = Usable::ask(self.n, eligible);
        self.choose(loads, &usable, rand::rng().random::<f64>())
    }
}

/// The target a draw `r` in `[0, 1)` picks over the usable targets.
///
/// `latencies` and `loads` are index-aligned with the route. A missing or
/// non-positive latency is unknown and takes the mean of the known ones, or
/// `1.0` when none is known, so an unsampled target is neither favoured nor
/// punished.
fn share_pick(
    n: usize,
    usable: &Usable,
    latencies: &[f64],
    loads: &[u64],
    r: f64,
) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let known = |i: usize| {
        latencies
            .get(i)
            .copied()
            .filter(|l| *l > 0.0 && l.is_finite())
    };

    let (mut sum, mut seen) = (0.0, 0usize);
    for i in (0..n).filter(|&i| usable.contains(i)) {
        if let Some(lat) = known(i) {
            sum += lat;
            seen += 1;
        }
    }
    let fill = if seen > 0 { sum / seen as f64 } else { 1.0 };

    let weight = |i: usize| {
        let lat = known(i).unwrap_or(fill);
        let speed = (1.0 / lat).powf(SPEED_EXPONENT);
        let load = loads.get(i).copied().unwrap_or(0) as f64;
        speed / (1.0 + load)
    };

    let (mut total, mut count) = (0.0, 0usize);
    for i in (0..n).filter(|&i| usable.contains(i)) {
        total += weight(i);
        count += 1;
    }
    if count == 0 {
        return None;
    }
    // a degenerate weight set (every target infinitely fast, say) falls back
    // to a uniform split rather than a NaN share
    let weighted = total.is_finite() && total > 0.0;

    let mut acc = 0.0;
    let mut last = None;
    for i in (0..n).filter(|&i| usable.contains(i)) {
        last = Some(i);
        acc += if weighted {
            (1.0 - EXPLORE_FLOOR) * weight(i) / total + EXPLORE_FLOOR / count as f64
        } else {
            1.0 / count as f64
        };
        if r < acc {
            return Some(i);
        }
    }
    // the shares sum to 1 only up to rounding, so a draw at the very top of
    // the range lands past the last boundary: it takes the last usable target
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Vec<f64>);

    impl LatencySource for Fixed {
        fn latencies(&self, n: usize) -> Vec<f64> {
            let mut out = self.0.clone();
            out.resize(n, 0.0);
            out
        }
    }

    /// The share of `target` over a fine grid of draws.
    fn share_of(n: usize, usable: &Usable, lat: &[f64], loads: &[u64], target: usize) -> f64 {
        let steps = 100_000;
        let hits = (0..steps)
            .filter(|k| {
                let r = (*k as f64 + 0.5) / steps as f64;
                share_pick(n, usable, lat, loads, r) == Some(target)
            })
            .count();
        hits as f64 / steps as f64
    }

    #[test]
    fn a_faster_target_takes_the_share_its_speed_earns() {
        // (1/10)^2 against (1/100)^2 is 100:1, then the uniform floor
        let usable = Usable::ask(2, &|_| true);
        let p0 = share_of(2, &usable, &[10.0, 100.0], &[], 0);
        let expected = 0.95 * (100.0 / 101.0) + 0.05 / 2.0;
        assert!((p0 - expected).abs() < 0.002, "{p0} vs {expected}");
    }

    #[test]
    fn equal_latencies_split_by_in_flight_load() {
        // weights 1 and 1/(1+9): the idle target takes the larger share
        let usable = Usable::ask(2, &|_| true);
        let p0 = share_of(2, &usable, &[5.0, 5.0], &[0, 9], 0);
        let expected = 0.95 * (1.0 / 1.1) + 0.05 / 2.0;
        assert!((p0 - expected).abs() < 0.002, "{p0} vs {expected}");
    }

    #[test]
    fn with_nothing_known_the_load_alone_decides() {
        let usable = Usable::ask(2, &|_| true);
        let p0 = share_of(2, &usable, &[], &[0, 3], 0);
        let expected = 0.95 * (1.0 / 1.25) + 0.05 / 2.0;
        assert!((p0 - expected).abs() < 0.002, "{p0} vs {expected}");
    }

    #[test]
    fn an_ineligible_target_takes_no_share() {
        let usable = Usable::ask(3, &|i| i == 1);
        for k in 0..1_000 {
            let r = (f64::from(k) + 0.5) / 1_000.0;
            assert_eq!(share_pick(3, &usable, &[9.0, 4.0, 1.0], &[], r), Some(1));
        }
    }

    #[test]
    fn a_draw_at_the_top_of_the_range_still_picks_a_usable_target() {
        let usable = Usable::ask(3, &|i| i != 1);
        let pick = share_pick(3, &usable, &[9.0, 4.0, 1.0], &[], 0.999_999_999_9);
        assert!(matches!(pick, Some(0) | Some(2)), "{pick:?}");
    }

    #[test]
    fn a_pick_with_a_fixed_source_mostly_takes_the_fastest_target() {
        let balancer = FastestShare::new(3, Some(Arc::new(Fixed(vec![300.0, 40.0, 900.0]))));
        let ctx = RouteContext::default();
        let mut hits = 0;
        for _ in 0..2_000 {
            if balancer.pick(&ctx, &[]) == Some(1) {
                hits += 1;
            }
        }
        // the expected share is about 0.949; the bound sits many deviations below it
        assert!(hits > 1_700, "fastest target drew {hits} of 2000");
    }

    #[test]
    fn without_a_latency_source_the_split_follows_load() {
        let balancer = FastestShare::new(2, None);
        let ctx = RouteContext::default();
        let mut zero = 0;
        for _ in 0..4_000 {
            if balancer.pick_eligible(&ctx, &[0, 0], &|_| true) == Some(0) {
                zero += 1;
            }
        }
        assert!(
            (1_800..=2_200).contains(&zero),
            "target 0 drew {zero} of 4000"
        );
    }

    #[test]
    fn it_names_itself_and_refuses_an_empty_route() {
        let balancer = FastestShare::new(0, None);
        assert_eq!(balancer.name(), "fastest_share");
        assert_eq!(balancer.pick(&RouteContext::default(), &[]), None);
    }
}
