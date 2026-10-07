//! Fixed metric surface bands and continuous terrestrial ecotones. These fields
//! reconstruct the physical atlas; they never use a camera, tile, or visit seed.

use crate::{geometry::normalize, hash64, Biome, WorldConfig};

fn rotate(p: [f64; 3]) -> [f64; 3] {
    [
        0.36 * p[0] + 0.48 * p[1] + 0.80 * p[2],
        -0.80 * p[0] + 0.60 * p[1],
        -0.48 * p[0] - 0.64 * p[1] + 0.60 * p[2],
    ]
}

fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// C2 interpolation: neighbouring noise cells have matching derivatives.
fn lattice(seed: u64, q: [f64; 3]) -> f64 {
    let base = q.map(|x| x.floor() as i64);
    let t = std::array::from_fn::<_, 3, _>(|i| fade(q[i] - base[i] as f64));
    let mut value = 0.0;
    for corner in 0..8 {
        let mut key = seed;
        let mut weight = 1.0;
        for i in 0..3 {
            let bit = (corner >> i) & 1;
            key ^= ((base[i] + bit) as u64).wrapping_mul([0x8da6b343, 0xd8163841, 0xcb1ab31f][i]);
            weight *= if bit == 0 { 1.0 - t[i] } else { t[i] };
        }
        value += weight * (2.0 * (hash64(key) >> 11) as f64 / (1_u64 << 53) as f64 - 1.0);
    }
    value
}

pub(crate) fn metric_noise(seed: u64, p: [f64; 3], radius_m: f64, metres: f64) -> f64 {
    lattice(seed, rotate(p).map(|x| x * radius_m / metres))
}

pub(crate) fn relief(config: &WorldConfig, p: [f64; 3], erodability: f64) -> f64 {
    let frequency = f64::from(config.resolution) * 0.75;
    let coarse = rotate(p);
    let warp = normalize(std::array::from_fn(|i| {
        coarse[i]
            + 0.32 / frequency
                * lattice(
                    config.seed ^ (0x99121 + i as u64 * 107),
                    coarse.map(|x| x * frequency * 0.37),
                )
    }));
    // Ice is surface cover, not a reason to turn the bedrock into sharp
    // crystalline ridges. Each octave also has its own lattice orientation.
    let ridge_fraction = (0.42 - 0.18 * erodability.max(0.7)).clamp(0.10, 0.30);
    let mut q = warp;
    let mut value = 0.0;
    let mut normalization = 0.0;
    let mut amplitude = 1.0;
    let mut frequency = frequency;
    for band in 0..32 {
        if config.radius_m / frequency < config.surface_detail_min_spacing_m {
            break;
        }
        let n = lattice(
            config.seed.wrapping_add(0x44281 + band * 97),
            q.map(|x| x * frequency),
        );
        // Rounded ridge shoulders replace the derivative cusp of abs(n).
        let ridge = 0.45 - 1.4 * (n * n + 0.0064).sqrt();
        value += amplitude * (n * (1.0 - ridge_fraction) + ridge * ridge_fraction);
        normalization += amplitude;
        amplitude *= 2.0_f64.powf(-0.95);
        frequency *= 2.0;
        q = rotate(q);
    }
    value / normalization.max(1e-12)
}

fn transition(x: f64, low: f64, high: f64) -> f64 {
    fade(((x - low) / (high - low)).clamp(0.0, 1.0))
}

/// Climate chooses broad populations; fixed microclimate fragments their
/// transitions. The fragment shader then supplies sub-mesh material patches.
pub(crate) fn biome_mix(
    config: &WorldConfig,
    p: [f64; 3],
    temperature_c: f64,
    moisture: f64,
    height_m: f64,
    discharge: f64,
) -> [f32; 10] {
    let macro_noise = metric_noise(config.seed ^ 0xb107e5, p, config.radius_m, 35_000.0);
    let meso_noise = metric_noise(config.seed ^ 0x713ba51, p, config.radius_m, 4_000.0);
    let t = temperature_c + 2.0 * macro_noise + 0.7 * meso_noise;
    let m = (moisture + 0.07 * macro_noise + 0.04 * meso_noise).clamp(0.0, 1.0);
    let ice = 1.0 - transition(t, -13.0, -3.0);
    let tundra = (1.0 - ice) * (1.0 - transition(t, -0.5, 7.0));
    let warm = (1.0 - ice - tundra).max(0.0);
    let desert = warm * (1.0 - transition(m, 0.12, 0.32));
    let living = (warm - desert).max(0.0);
    let forest = living * transition(m, 0.30, 0.63);
    let rainforest = forest * transition(t, 16.0, 29.0) * transition(m, 0.62, 0.86);
    let grass = (living - forest).max(0.0);
    let wet = transition(m, 0.55, 0.85)
        * transition(discharge, 250.0, 5_000.0)
        * (1.0 - transition(height_m, 120.0, 550.0))
        * transition(t, -4.0, 5.0);
    let rock = transition(height_m, 2_500.0, 5_500.0) * (1.0 - ice) * 0.55;
    let replacement = (wet + rock).min(0.85);
    let mut mix = [0.0; 10];
    for (biome, fraction) in [
        (Biome::Ice, ice),
        (Biome::Tundra, tundra),
        (Biome::Desert, desert),
        (Biome::Grassland, grass),
        (Biome::Forest, forest - rainforest),
        (Biome::Rainforest, rainforest),
    ] {
        mix[biome as usize] = (fraction * (1.0 - replacement)) as f32;
    }
    mix[Biome::Wetland as usize] = wet as f32;
    mix[Biome::Rock as usize] = rock as f32;
    let sum: f32 = mix.iter().sum();
    for w in &mut mix {
        *w /= sum.max(1e-12);
    }
    mix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lattice_derivatives_match_on_both_sides_of_a_cell_boundary() {
        let epsilon = 0.0001;
        let center = [7.0, 0.37, -0.43];
        let mut left = center;
        let mut right = center;
        left[0] -= epsilon;
        right[0] += epsilon;
        let c = lattice(73, center);
        assert!((lattice(73, left) - c).abs() < 1e-9);
        assert!((lattice(73, right) - c).abs() < 1e-9);
        let dl = (c - lattice(73, left)) / epsilon;
        let dr = (lattice(73, right) - c) / epsilon;
        assert!((dl - dr).abs() < 1e-5);
    }

    #[test]
    fn relief_is_repeatable_bounded_and_not_keyed_by_a_tile_or_view() {
        let config = WorldConfig::default();
        for i in 0..1_000 {
            let p = normalize([i as f64 * 0.017 - 4.0, 0.61, 0.37]);
            let a = relief(&config, p, 0.2);
            assert_eq!(a, relief(&config, p, 0.2));
            assert!(a.is_finite() && a.abs() <= 1.0);
            assert_ne!(
                a,
                relief(
                    &WorldConfig {
                        seed: 71,
                        ..config.clone()
                    },
                    p,
                    0.2
                )
            );
        }
    }

    #[test]
    fn climate_ecotones_are_soft_fragmented_and_exclude_water() {
        let config = WorldConfig::default();
        let p = normalize([0.43, 0.61, -0.37]);
        let a = biome_mix(&config, p, -8.0, 0.55, 300.0, 40.0);
        assert!(a[Biome::Ice as usize] > 0.0 && a[Biome::Tundra as usize] > 0.0);
        assert_eq!(a[0], 0.0);
        assert_eq!(a[1], 0.0);
        assert!((a.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        let adjacent = biome_mix(&config, p, -8.0 + 1e-5, 0.55, 300.0, 40.0);
        assert!((a[2] - adjacent[2]).abs() < 1e-5);
        let other = biome_mix(
            &config,
            normalize([0.43, 0.64, -0.39]),
            -8.0,
            0.55,
            300.0,
            40.0,
        );
        assert_ne!(a, other);
    }
}
