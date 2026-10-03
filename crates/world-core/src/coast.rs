//! Fixed spherical twists: each rotation depends only on its invariant axial
//! coordinate. Its inverse is the same rotation with the opposite angle.
//! Their composition wrinkles shorelines without folding the spherical domain.
use crate::{
    geometry::{cross, dot, normalize},
    random, WorldConfig,
};

const AXES: [[f64; 3]; 6] = [
    [0.5773502691896258, 0.5773502691896258, 0.5773502691896258],
    [0.2672612419124244, -0.5345224838248488, 0.8017837257372732],
    [-0.8017837257372732, 0.2672612419124244, 0.5345224838248488],
    [0.7071067811865475, 0.0, -0.7071067811865475],
    [0.0, 0.8944271909999159, 0.4472135954999579],
    [-0.4082482904638631, 0.8164965809277261, -0.4082482904638631],
];

fn noise_1d(seed: u64, coordinate: f64) -> f64 {
    let i = coordinate.floor() as i64;
    let t = coordinate - i as f64;
    let t = t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let at = |i: i64| 2.0 * random(seed ^ (i as u64).wrapping_mul(0x9e3779b97f4a7c15)) - 1.0;
    at(i) * (1.0 - t) + at(i + 1) * t
}

fn twist(p: [f64; 3], axis: [f64; 3], seed: u64, frequency: f64, strength: f64) -> [f64; 3] {
    let z = dot(axis, p);
    let angle = strength / frequency * noise_1d(seed, z * frequency);
    let (s, c) = angle.sin_cos();
    let tangent = cross(axis, p);
    std::array::from_fn(|i| p[i] * c + tangent[i] * s + axis[i] * z * (1.0 - c))
}

fn band_count(config: &WorldConfig) -> u32 {
    let wavelength = config.radius_m / (f64::from(config.resolution) * 0.75);
    ((wavelength / config.coast_reference_spacing_m)
        .log2()
        .floor()
        + 1.0)
        .clamp(0.0, 27.0) as u32
}

fn band(config: &WorldConfig, mut p: [f64; 3], index: u32, inverse: bool) -> [f64; 3] {
    let frequency = f64::from(config.resolution) * 0.75 * 2.0_f64.powi(index as i32);
    // H=0.85 down to the cutoff. At the default ratio, metre-scale bands are
    // perceptible without overwhelming their larger coastal structure.
    let strength =
        (0.22 * config.coast_detail_ratio * 2.0_f64.powf(0.15 * f64::from(index))).min(0.24);
    for step in 0..2_u32 {
        let step = if inverse { 1 - step } else { step };
        let axis = AXES[((index * 2 + step) % AXES.len() as u32) as usize];
        let seed = config.seed ^ (0xc0a57 + u64::from(index) * 193 + u64::from(step) * 104729);
        p = twist(
            p,
            axis,
            seed,
            frequency,
            if inverse { -strength } else { strength },
        );
    }
    p
}

pub(crate) fn remap(config: &WorldConfig, p: [f64; 3]) -> [f64; 3] {
    if config.coast_detail_ratio == 0.0 {
        return p;
    }
    let mut p = p;
    for index in 0..band_count(config) {
        p = band(config, p, index, false);
    }
    normalize(p)
}

pub(crate) fn inverse(config: &WorldConfig, p: [f64; 3]) -> [f64; 3] {
    if config.coast_detail_ratio == 0.0 {
        return p;
    }
    let mut p = p;
    for index in (0..band_count(config)).rev() {
        p = band(config, p, index, true);
    }
    normalize(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spherical_twists_are_invertible_at_faces_poles_and_coastal_scales() {
        for ratio in [0.16, 1.0] {
            let config = WorldConfig {
                resolution: 512,
                coast_detail_ratio: ratio,
                coast_reference_spacing_m: 0.5,
                ..Default::default()
            };
            for i in 0..1000_u64 {
                let p = normalize(std::array::from_fn(|axis| {
                    2.0 * random(i * 3 + axis as u64) - 1.0
                }));
                let mut q = remap(&config, p);
                assert!((dot(q, q) - 1.0).abs() < 1e-14);
                for index in (0..band_count(&config)).rev() {
                    q = band(&config, q, index, true);
                }
                assert!(
                    dot(
                        std::array::from_fn(|a| p[a] - q[a]),
                        std::array::from_fn(|a| p[a] - q[a])
                    ) < 1e-24
                );
            }
            for p in [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, -1.0],
                normalize([1.0, 1.0, 1.0]),
            ] {
                let mut q = remap(&config, p);
                for index in (0..band_count(&config)).rev() {
                    q = band(&config, q, index, true);
                }
                assert!(p.iter().zip(q).all(|(a, b)| (a - b).abs() < 1e-12));
            }
        }
    }

    #[test]
    fn metre_scale_coastal_bands_have_a_measurable_fixed_displacement() {
        let mut coarse = WorldConfig {
            seed: 2386084681,
            resolution: 512,
            coast_reference_spacing_m: 32.0,
            ..Default::default()
        };
        let p = normalize([0.31829718, -0.70244895, -0.63659436]);
        let a = remap(&coarse, p);
        coarse.coast_reference_spacing_m = 1.0;
        let b = remap(&coarse, p);
        let distance = dot(
            std::array::from_fn(|i| a[i] - b[i]),
            std::array::from_fn(|i| a[i] - b[i]),
        )
        .sqrt()
            * coarse.radius_m;
        assert!(distance > 0.05 && distance < 20.0, "distance={distance}");
    }
}
