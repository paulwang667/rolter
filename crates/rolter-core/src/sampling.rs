//! Unit-interval draws for weighted traffic splits.
//!
//! A split that should vary from request to request takes a fresh uniform draw
//! (the gateway's `rand` generator). A split that must keep one caller on one
//! arm, an A/B test or a canary, hashes the caller's identity instead, so the
//! same identity always lands on the same draw.

/// A uniform draw in `[0, 1)` from `key`, the same for the same key on every
/// process and every release.
///
/// FNV-1a with a splitmix64 finaliser: FNV alone mixes its low bits poorly on
/// short keys, and the split reads the high bits. The algorithm is pinned by a
/// test, because changing it reshuffles every sticky assignment at once.
pub fn stable_unit(key: &[u8]) -> f64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    for &byte in key {
        h ^= u64::from(byte);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^= h >> 31;
    // the top 53 bits give every representable f64 in [0, 1) equal weight
    (h >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_key_always_draws_the_same_unit() {
        assert_eq!(
            stable_unit(b"gpt-4o\x1fsession-7"),
            stable_unit(b"gpt-4o\x1fsession-7")
        );
    }

    #[test]
    fn the_draw_is_pinned_so_a_sticky_assignment_survives_an_upgrade() {
        // a changed algorithm moves every sticky caller to another arm at once
        assert_eq!(
            stable_unit(b"alice").to_bits(),
            4_605_135_311_451_508_457_u64
        );
    }

    #[test]
    fn draws_stay_in_the_unit_interval() {
        for i in 0..10_000u32 {
            let u = stable_unit(&i.to_le_bytes());
            assert!((0.0..1.0).contains(&u), "{u} for {i}");
        }
    }

    #[test]
    fn draws_are_close_to_uniform_over_deciles() {
        let n = 100_000u32;
        let mut deciles = [0u32; 10];
        for i in 0..n {
            let u = stable_unit(format!("caller-{i}").as_bytes());
            deciles[((u * 10.0) as usize).min(9)] += 1;
        }
        let expected = f64::from(n) / 10.0;
        for (d, count) in deciles.iter().enumerate() {
            let skew = (f64::from(*count) - expected).abs() / expected;
            assert!(skew < 0.02, "decile {d} holds {count} of {n}");
        }
    }
}
