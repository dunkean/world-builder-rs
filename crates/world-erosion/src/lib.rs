//! Conservative regional hydraulic erosion with CPU and CUDA backends.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

#[cfg(feature = "cuda")]
mod cuda;

pub const ALGORITHM: &str = "regional-finite-volume-2";

#[derive(Clone, Copy, Debug)]
pub enum Backend {
    Auto,
    Cpu,
    Cuda,
}

#[derive(Clone, Debug)]
pub struct HeightField {
    pub width: usize,
    pub height: usize,
    pub spacing_m: f32,
    pub heights_m: Vec<f32>,
    /// Physical fixed-height constraints; water and sediment may cross them.
    /// The external export boundary stays unmodified independently of this mask.
    pub protected: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ErosionConfig {
    pub steps: u32,
    pub dt_s: f32,
    pub rainfall_m_per_step: f32,
    pub conductivity_m_s: f32,
    pub capacity: f32,
    pub erosion_rate_s: f32,
    pub deposition_rate_s: f32,
    pub max_erosion_m_per_step: f32,
}
impl Default for ErosionConfig {
    fn default() -> Self {
        Self {
            steps: 160,
            dt_s: 1.0,
            rainfall_m_per_step: 0.015,
            conductivity_m_s: 15.0,
            capacity: 35.0,
            erosion_rate_s: 0.5,
            deposition_rate_s: 0.35,
            max_erosion_m_per_step: 0.35,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErosionReport {
    pub algorithm: String,
    pub backend: String,
    pub fallback_reason: Option<String>,
    pub elapsed_ms: f64,
    pub steps: u32,
    /// Requested conductance is capped to keep the 2D explicit water update stable.
    pub effective_conductivity_m_s: f32,
    pub rainfall_m3: f64,
    pub water_remaining_m3: f64,
    pub water_export_m3: f64,
    pub water_relative_error: f64,
    pub eroded_m3: f64,
    pub deposited_m3: f64,
    pub suspended_m3: f64,
    pub sediment_export_m3: f64,
    pub solid_relative_error: f64,
    pub height_quantization_m3: f64,
    pub max_height_change_m: f32,
}
pub struct ErosionResult {
    pub heights_m: Vec<f32>,
    pub residual_m: Vec<f32>,
    pub water_m: Vec<f32>,
    pub suspended_sediment_m: Vec<f32>,
    pub report: ErosionReport,
}

pub(crate) struct State {
    pub delta: Vec<f32>,
    pub water: Vec<f32>,
    pub sediment: Vec<f32>,
    pub exported_water: Vec<f32>,
    pub exported_sediment: Vec<f32>,
    pub eroded: Vec<f32>,
    pub deposited: Vec<f32>,
}
impl State {
    fn new(count: usize) -> Self {
        Self {
            delta: vec![0.0; count],
            water: vec![0.0; count],
            sediment: vec![0.0; count],
            exported_water: vec![0.0; count],
            exported_sediment: vec![0.0; count],
            eroded: vec![0.0; count],
            deposited: vec![0.0; count],
        }
    }
}

pub fn erode(
    field: &HeightField,
    config: ErosionConfig,
    backend: Backend,
) -> Result<ErosionResult> {
    validate(field, config)?;
    let start = std::time::Instant::now();
    let (state, backend, fallback_reason) = match backend {
        Backend::Cpu => (cpu(field, config), "cpu-f32".to_string(), None),
        Backend::Auto | Backend::Cuda => {
            #[cfg(feature = "cuda")]
            {
                match cuda::run(field, config) {
                    Ok(state) => (state, "cuda-f32".to_string(), None),
                    Err(error) if matches!(backend, Backend::Auto) => (
                        cpu(field, config),
                        "cpu-f32".to_string(),
                        Some(format!("{error:#}")),
                    ),
                    Err(error) => return Err(error),
                }
            }
            #[cfg(not(feature = "cuda"))]
            {
                if matches!(backend, Backend::Cuda) {
                    bail!("CUDA feature is disabled");
                }
                (
                    cpu(field, config),
                    "cpu-f32".to_string(),
                    Some("CUDA feature disabled".into()),
                )
            }
        }
    };
    let mut result = finish(
        field,
        config,
        state,
        backend,
        start.elapsed().as_secs_f64() * 1000.0,
    )?;
    result.report.fallback_reason = fallback_reason;
    Ok(result)
}

fn validate(field: &HeightField, c: ErosionConfig) -> Result<()> {
    if !(3..=2049).contains(&field.width)
        || !(3..=2049).contains(&field.height)
        || field.heights_m.len() != field.width * field.height
        || field.protected.len() != field.heights_m.len()
        || !field.spacing_m.is_finite()
        || field.spacing_m <= 0.0
        || field.heights_m.iter().any(|v| !v.is_finite())
    {
        bail!("invalid regional height field");
    }
    let values = [
        c.dt_s,
        c.rainfall_m_per_step,
        c.conductivity_m_s,
        c.capacity,
        c.erosion_rate_s,
        c.deposition_rate_s,
        c.max_erosion_m_per_step,
    ];
    if c.steps > 4096
        || values.iter().any(|x| !x.is_finite() || *x < 0.0)
        || c.dt_s <= 0.0
        || c.erosion_rate_s * c.dt_s > 1.0
        || c.deposition_rate_s * c.dt_s > 1.0
    {
        bail!("invalid regional erosion time step or coefficient");
    }
    Ok(())
}
fn boundary(i: usize, f: &HeightField) -> bool {
    i < f.width || i >= f.width * (f.height - 1) || i % f.width == 0 || i % f.width == f.width - 1
}
fn neighbours(i: usize, f: &HeightField) -> [usize; 4] {
    [i - 1, i + 1, i - f.width, i + f.width]
}

fn conductance(f: &HeightField, c: ErosionConfig) -> f32 {
    // For a flat bed, water transport is an explicit four-neighbour diffusion.
    // Its alternating mode has amplification 1 - 8*q; q < 1/8 damps it without
    // changing its sign. The available-water limiter alone only enforces positivity.
    (c.dt_s * c.conductivity_m_s / f.spacing_m).min(0.12)
}

fn neighbour_bed(i: usize, j: usize, f: &HeightField, delta: &[f32]) -> f32 {
    // Continue the local displacement into an artificial outflow ghost cell.
    // Leaving its displacement at zero would erect a wall after interior incision.
    // Real water/channel ports retain their imposed physical bed.
    f.heights_m[j]
        + if boundary(j, f) && f.protected[j] == 0 {
            delta[i]
        } else {
            delta[j]
        }
}

fn cpu(f: &HeightField, c: ErosionConfig) -> State {
    let count = f.heights_m.len();
    let mut state = State::new(count);
    let mut next_water = vec![0.0; count];
    let mut next_sediment = vec![0.0; count];
    let mut next_delta = vec![0.0; count];
    let mut flux = vec![0.0f32; count * 4];
    let mut sediment_flux = vec![0.0f32; count * 4];
    let q = conductance(f, c);
    for _ in 0..c.steps {
        for i in 0..count {
            if boundary(i, f) {
                continue;
            }
            let available = state.water[i] + c.rainfall_m_per_step;
            let head = f.heights_m[i] + state.delta[i] + available;
            let mut sum = 0.0;
            for (k, j) in neighbours(i, f).into_iter().enumerate() {
                let rain = if boundary(j, f) {
                    0.0
                } else {
                    c.rainfall_m_per_step
                };
                let drop = (head - (neighbour_bed(i, j, f, &state.delta) + state.water[j] + rain))
                    .max(0.0);
                flux[i * 4 + k] = q * drop;
                sum += flux[i * 4 + k];
            }
            let scale = if sum > 0.0 {
                (0.9 * available / sum).min(1.0)
            } else {
                0.0
            };
            for k in 0..4 {
                flux[i * 4 + k] *= scale;
                sediment_flux[i * 4 + k] = if available > 1e-12 {
                    flux[i * 4 + k] * state.sediment[i] / available
                } else {
                    0.0
                };
            }
        }
        for i in 0..count {
            next_delta[i] = state.delta[i];
            let x = i % f.width;
            let y = i / f.width;
            let incoming = [
                if x > 0 { Some((i - 1, 1)) } else { None },
                if x + 1 < f.width {
                    Some((i + 1, 0))
                } else {
                    None
                },
                if y > 0 { Some((i - f.width, 3)) } else { None },
                if y + 1 < f.height {
                    Some((i + f.width, 2))
                } else {
                    None
                },
            ];
            let mut in_water = 0.0;
            let mut in_sediment = 0.0;
            for (j, k) in incoming.into_iter().flatten() {
                in_water += flux[j * 4 + k];
                in_sediment += sediment_flux[j * 4 + k];
            }
            if boundary(i, f) {
                state.exported_water[i] += in_water;
                state.exported_sediment[i] += in_sediment;
                next_water[i] = 0.0;
                next_sediment[i] = 0.0;
                continue;
            }
            let out_water: f32 = flux[i * 4..i * 4 + 4].iter().sum();
            let out_sediment: f32 = sediment_flux[i * 4..i * 4 + 4].iter().sum();
            next_water[i] =
                (state.water[i] + c.rainfall_m_per_step + in_water - out_water).max(0.0);
            let mut sediment = (state.sediment[i] + in_sediment - out_sediment).max(0.0);
            if f.protected[i] == 0 {
                let h = f.heights_m[i] + state.delta[i];
                let slope = neighbours(i, f)
                    .into_iter()
                    .map(|j| ((h - neighbour_bed(i, j, f, &state.delta)) / f.spacing_m).max(0.0))
                    .fold(0.0, f32::max);
                let capacity = c.capacity * out_water.max(in_water) * slope;
                if sediment > capacity {
                    let deposit = (sediment - capacity) * c.deposition_rate_s * c.dt_s;
                    next_delta[i] += deposit;
                    sediment -= deposit;
                    state.deposited[i] += deposit;
                } else {
                    let erosion = ((capacity - sediment) * c.erosion_rate_s * c.dt_s)
                        .min(c.max_erosion_m_per_step);
                    next_delta[i] -= erosion;
                    sediment += erosion;
                    state.eroded[i] += erosion;
                }
            }
            next_sediment[i] = sediment;
        }
        std::mem::swap(&mut state.water, &mut next_water);
        std::mem::swap(&mut state.sediment, &mut next_sediment);
        std::mem::swap(&mut state.delta, &mut next_delta);
    }
    state
}

fn finish(
    f: &HeightField,
    c: ErosionConfig,
    s: State,
    backend: String,
    elapsed_ms: f64,
) -> Result<ErosionResult> {
    let area = f64::from(f.spacing_m).powi(2);
    let volume = |v: &[f32]| v.iter().map(|&x| f64::from(x) * area).sum::<f64>();
    let water_remaining = volume(&s.water);
    let water_export = volume(&s.exported_water);
    let rainfall = (f.width - 2) * (f.height - 2);
    let rainfall = rainfall as f64 * f64::from(c.rainfall_m_per_step) * f64::from(c.steps) * area;
    let eroded = volume(&s.eroded);
    let deposited = volume(&s.deposited);
    let suspended = volume(&s.sediment);
    let sediment_export = volume(&s.exported_sediment);
    let solid_delta = volume(&s.delta);
    let heights: Vec<_> = f
        .heights_m
        .iter()
        .zip(&s.delta)
        .map(|(&a, &b)| a + b)
        .collect();
    if heights.iter().any(|x| !x.is_finite()) {
        bail!("erosion produced nonfinite relief");
    }
    let quantization = heights
        .iter()
        .zip(&f.heights_m)
        .zip(&s.delta)
        .map(|((&a, &b), &d)| (f64::from(a) - f64::from(b) - f64::from(d)) * area)
        .sum();
    let report = ErosionReport {
        algorithm: ALGORITHM.into(),
        backend,
        fallback_reason: None,
        elapsed_ms,
        steps: c.steps,
        effective_conductivity_m_s: conductance(f, c) * f.spacing_m / c.dt_s,
        rainfall_m3: rainfall,
        water_remaining_m3: water_remaining,
        water_export_m3: water_export,
        water_relative_error: (rainfall - water_remaining - water_export).abs() / rainfall.max(1.0),
        eroded_m3: eroded,
        deposited_m3: deposited,
        suspended_m3: suspended,
        sediment_export_m3: sediment_export,
        solid_relative_error: (solid_delta + suspended + sediment_export).abs() / eroded.max(1.0),
        height_quantization_m3: quantization,
        max_height_change_m: s.delta.iter().fold(0.0f32, |a, &b| a.max(b.abs())),
    };
    if report.water_relative_error > 5e-5 || report.solid_relative_error > 5e-5 {
        bail!(
            "regional erosion failed conservation: water={}, solid={}",
            report.water_relative_error,
            report.solid_relative_error
        );
    }
    Ok(ErosionResult {
        heights_m: heights,
        residual_m: s.delta,
        water_m: s.water,
        suspended_sediment_m: s.sediment,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn slope() -> HeightField {
        let n = 33;
        HeightField {
            width: n,
            height: n,
            spacing_m: 10.0,
            heights_m: (0..n * n)
                .map(|i| 100.0 - 2.0 * (i / n) as f32 + ((i % n) as f32 * 0.5).sin())
                .collect(),
            protected: vec![0; n * n],
        }
    }
    #[test]
    fn rainfall_erodes_transports_and_deposits_without_losing_solid() {
        let f = slope();
        let r = erode(&f, ErosionConfig::default(), Backend::Cpu).unwrap();
        assert!(r.report.eroded_m3 > 1.0);
        assert!(r.report.deposited_m3 > 0.0);
        assert!(r.report.sediment_export_m3 > 0.0);
        assert!(r.report.water_relative_error < 5e-5);
        assert!(r.report.solid_relative_error < 5e-5);
        for i in 0..f.heights_m.len() {
            if boundary(i, &f) {
                assert_eq!(r.heights_m[i], f.heights_m[i]);
            }
        }
    }
    #[test]
    fn protected_channels_and_flat_dry_relief_are_not_mutated() {
        let mut f = slope();
        for y in 0..f.height {
            f.protected[y * f.width + 16] = 1;
        }
        let r = erode(&f, ErosionConfig::default(), Backend::Cpu).unwrap();
        for y in 0..f.height {
            assert_eq!(r.heights_m[y * f.width + 16], f.heights_m[y * f.width + 16]);
        }
        f.heights_m.fill(10.0);
        let r = erode(
            &f,
            ErosionConfig {
                rainfall_m_per_step: 0.0,
                ..Default::default()
            },
            Backend::Cpu,
        )
        .unwrap();
        assert_eq!(r.report.eroded_m3, 0.0);
        assert_eq!(r.heights_m, f.heights_m);
    }

    #[test]
    fn excessive_conductivity_keeps_flat_rainfall_smooth_and_conservative() {
        let n = 17;
        let f = HeightField {
            width: n,
            height: n,
            spacing_m: 1.0,
            heights_m: vec![10.0; n * n],
            protected: vec![0; n * n],
        };
        let config = ErosionConfig {
            steps: 128,
            conductivity_m_s: 1_000_000.0,
            ..Default::default()
        };
        let r = erode(&f, config, Backend::Cpu).unwrap();
        assert_eq!(r.report.effective_conductivity_m_s, 0.12);
        assert_eq!(r.heights_m, f.heights_m);
        assert!(r.report.water_relative_error < 5e-5);
        assert!(r.report.solid_relative_error < 5e-5);
        assert!(r.report.water_export_m3 > 0.0);
        let rainfall_depth = config.steps as f32 * config.rainfall_m_per_step;
        assert!(r.water_m.iter().all(|w| *w >= 0.0 && *w <= rainfall_depth));
        // A uniform rainstorm on a flat square has a monotone depth profile
        // from each open edge towards its centre, rather than alternating ridges.
        for y in 1..n - 1 {
            for x in 0..n / 2 {
                assert!(r.water_m[y * n + x + 1] + 1e-6 >= r.water_m[y * n + x]);
                assert!(r.water_m[x * n + y + n] + 1e-6 >= r.water_m[x * n + y]);
            }
        }
    }
}
