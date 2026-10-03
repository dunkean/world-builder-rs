//! Fixed spherical twists: each rotation depends only on its invariant axial
//! coordinate. Its inverse is the same rotation with the opposite angle.
//! Their composition wrinkles shorelines without folding the spherical domain.
use crate::{
    geometry::{cross, dot, normalize},
    random, WorldConfig,
};
use std::cell::RefCell;

#[derive(Clone, Copy, PartialEq, Eq)]
struct WarpKey {
    seed: u64,
    resolution: u32,
    radius: u64,
    cutoff: u64,
    strength: u64,
}

impl From<&WorldConfig> for WarpKey {
    fn from(config: &WorldConfig) -> Self {
        Self {
            seed: config.seed,
            resolution: config.resolution,
            radius: config.radius_m.to_bits(),
            cutoff: config.coast_reference_spacing_m.to_bits(),
            strength: config.coast_detail_ratio.to_bits(),
        }
    }
}

#[derive(Clone, Copy)]
struct Twist {
    axis: [f64; 3],
    seed: u64,
    frequency: f64,
    strength: f64,
    offset: f64,
}

// The three independent axes change with every band and world. Repeating six
// axes at exact octave frequencies exposed long straight rows of shore detail.
fn axis(seed: u64) -> [f64; 3] {
    let z = 2.0 * random(seed) - 1.0;
    let angle = std::f64::consts::TAU * random(seed ^ 0x79b97f4a7c15);
    let radius = (1.0 - z * z).sqrt();
    [radius * angle.cos(), radius * angle.sin(), z]
}

struct CoastWarp {
    bands: Vec<[Twist; 3]>,
}

impl CoastWarp {
    fn new(config: &WorldConfig) -> Self {
        let bands = (0..band_count(config))
            .filter_map(|index| {
                let seed = config.seed ^ (0xc0a57 + u64::from(index) * 193);
                let frequency = f64::from(config.resolution) * 0.75 * 2.0_f64.powi(index as i32);
                // H=0.68 before the shear bound: short wavelengths retain more
                // displacement than the previous H=0.85 / strength=0.24 field.
                // This bounds the angle coefficient, not the full composition's
                // Jacobian. Its cap still responds to the user's detail slider.
                let strength =
                    (0.55 * config.coast_detail_ratio * 2.0_f64.powf(0.32 * f64::from(index)))
                        .min(0.45 * config.coast_detail_ratio.sqrt());
                let first_axis = axis(seed ^ 0x3524ab);
                let mut second_axis = axis(seed ^ 0x917bed);
                if dot(first_axis, second_axis).abs() > 0.7 {
                    second_axis = normalize(cross(first_axis, second_axis));
                }
                let band = std::array::from_fn(|step| {
                    let seed = seed ^ (step as u64 * 104729);
                    let frequency = frequency * (0.85 + 0.35 * random(seed ^ 0xf8e91));
                    Twist {
                        axis: match step {
                            0 => first_axis,
                            1 => second_axis,
                            _ => axis(seed ^ 0x451ebc),
                        },
                        seed,
                        frequency,
                        strength: if config.radius_m / frequency >= config.coast_reference_spacing_m
                        {
                            strength
                        } else {
                            0.0
                        },
                        offset: 127.0 * random(seed ^ 0x91735),
                    }
                });
                band.iter().any(|t| t.strength > 0.0).then_some(band)
            })
            .collect();
        Self { bands }
    }

    fn apply(&self, mut p: [f64; 3], inverse: bool) -> [f64; 3] {
        if inverse {
            for band in self.bands.iter().rev() {
                for twist in band.iter().rev() {
                    p = twist.apply(p, -1.0);
                }
            }
        } else {
            for band in &self.bands {
                for twist in band {
                    p = twist.apply(p, 1.0);
                }
            }
        }
        normalize(p)
    }
}

thread_local! {
    // Per-thread immutable coefficients avoid a lock or repeated seed/trig/pow
    // setup in every sample. The key includes all parameters used by the warp.
    // Changing worlds/configuration rebuilds it; thread scheduling changes none
    // of the coefficients or evaluation order.
    static WARP: RefCell<Option<(WarpKey, CoastWarp)>> = const { RefCell::new(None) };
}

fn noise_1d(seed: u64, coordinate: f64) -> f64 {
    let at = |i: i64| {
        let key = seed ^ (i as u64).wrapping_mul(0x9e3779b97f4a7c15);
        (
            i as f64 + 0.25 + 0.5 * random(key ^ 0x7358cb),
            2.0 * random(key) - 1.0,
        )
    };
    let mut i = coordinate.floor() as i64;
    if coordinate < at(i).0 {
        i -= 1;
    }
    let (x0, v0) = at(i - 1);
    let (x1, v1) = at(i);
    let (x2, v2) = at(i + 1);
    let (x3, v3) = at(i + 2);
    let length = x2 - x1;
    let t = (coordinate - x1) / length;
    // Shared Catmull-Rom slopes on irregular knots: C1 without the old zero
    // slope at every point of a regular value-noise lattice. Independent
    // frequencies and three axes also remove the equal-spacing octave plaid.
    let m1 = (v2 - v0) / (x2 - x0) * length;
    let m2 = (v3 - v1) / (x3 - x1) * length;
    let t2 = t * t;
    let t3 = t2 * t;
    (2.0 * t3 - 3.0 * t2 + 1.0) * v1
        + (t3 - 2.0 * t2 + t) * m1
        + (-2.0 * t3 + 3.0 * t2) * v2
        + (t3 - t2) * m2
}

impl Twist {
    fn apply(&self, p: [f64; 3], sign: f64) -> [f64; 3] {
        if self.strength == 0.0 {
            return p;
        }
        let z = dot(self.axis, p);
        let angle = sign * self.strength / self.frequency
            * noise_1d(self.seed, z * self.frequency + self.offset);
        let (s, c) = angle.sin_cos();
        let tangent = cross(self.axis, p);
        std::array::from_fn(|i| p[i] * c + tangent[i] * s + self.axis[i] * z * (1.0 - c))
    }
}

fn band_count(config: &WorldConfig) -> u32 {
    let wavelength = config.radius_m / (f64::from(config.resolution) * 0.75);
    ((wavelength / config.coast_reference_spacing_m)
        .log2()
        .floor()
        + 1.0)
        .clamp(0.0, 27.0) as u32
}

fn apply(config: &WorldConfig, p: [f64; 3], inverse: bool) -> [f64; 3] {
    if config.coast_detail_ratio == 0.0 {
        return p;
    }
    WARP.with(|slot| {
        let mut cached = slot.borrow_mut();
        let key = WarpKey::from(config);
        if cached.as_ref().is_none_or(|(old, _)| *old != key) {
            *cached = Some((key, CoastWarp::new(config)));
        }
        cached.as_ref().unwrap().1.apply(p, inverse)
    })
}

pub(crate) fn remap(config: &WorldConfig, p: [f64; 3]) -> [f64; 3] {
    apply(config, p, false)
}

pub(crate) fn inverse(config: &WorldConfig, p: [f64; 3]) -> [f64; 3] {
    apply(config, p, true)
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
                q = inverse(&config, q);
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
                q = inverse(&config, q);
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
        assert!(distance > 0.25 && distance < 64.0, "distance={distance}");
    }

    #[test]
    fn coastal_orientation_is_seeded_without_a_fixed_axis_grid() {
        let mut mean = [0.0; 3];
        let mut second = [0.0; 3];
        for seed in 0..2048 {
            let config = WorldConfig {
                seed,
                ..Default::default()
            };
            let warp = CoastWarp::new(&config);
            let a = warp.bands[0][0];
            let b = warp.bands[0][1];
            assert!(dot(a.axis, b.axis).abs() <= 0.7 + 1e-12);
            for j in 0..3 {
                mean[j] += a.axis[j] / 2048.0;
                second[j] += a.axis[j] * a.axis[j] / 2048.0;
            }
        }
        assert!(mean.iter().all(|v| v.abs() < 0.06), "mean={mean:?}");
        assert!(
            second.iter().all(|v| (*v - 1.0 / 3.0).abs() < 0.04),
            "second={second:?}"
        );
    }

    #[test]
    fn switching_configs_or_threads_preserves_the_same_coastal_field() {
        let p = normalize([0.31829718, -0.70244895, -0.63659436]);
        let config = WorldConfig {
            seed: 2386084681,
            resolution: 512,
            ..Default::default()
        };
        let expected = remap(&config, p);
        let other = WorldConfig {
            seed: 92,
            radius_m: 700000.0,
            ..config.clone()
        };
        assert_ne!(remap(&other, p), expected);
        assert_eq!(remap(&config, p), expected);
        let results: Vec<_> = (0..4)
            .map(|_| {
                let config = config.clone();
                std::thread::spawn(move || remap(&config, p))
            })
            .collect();
        for result in results {
            assert_eq!(result.join().unwrap(), expected);
        }
    }

    #[test]
    fn irregular_coastal_knots_have_shared_slopes_without_lattice_plateaus() {
        for seed in 0..32 {
            for i in -12_i64..12 {
                let key = seed ^ (i as u64).wrapping_mul(0x9e3779b97f4a7c15);
                let x = i as f64 + 0.25 + 0.5 * random(key ^ 0x7358cb);
                let expected = 2.0 * random(key) - 1.0;
                let h = 1e-6;
                let at = noise_1d(seed, x);
                assert!((at - expected).abs() < 1e-12);
                let left = (at - noise_1d(seed, x - h)) / h;
                let right = (noise_1d(seed, x + h) - at) / h;
                assert!((left - right).abs() < 1e-4);
            }
        }
    }
}
