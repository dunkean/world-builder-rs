//! Deterministic, bounded learned residuals over the authoritative procedural globe.
//! Six overlapping conformal charts avoid a latitude/longitude singularity.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::geometry::{dot, normalize, FACE_BASES};

pub const NATIVE_MPP: f64 = 30.0;
pub const PATCH_CORE: usize = 256;
pub const PATCH_SIDE: usize = 258;
pub const MAX_PATCHES: usize = 96;
pub const MAX_RESIDUAL_M: f64 = 80.0;
pub const MAX_HEIGHT_M: f64 = 20_000.0;
const FEATHER_PIXELS: f64 = 24.0;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    #[default]
    Residual,
    Height,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HybridPatch {
    pub face: u8,
    #[serde(default)]
    pub level: u8,
    pub x: i32,
    pub y: i32,
    /// Row-major native sample centres, including one sample of halo on each side.
    pub values: Vec<f32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HybridField {
    pub world_id: String,
    pub revision: u64,
    radius_m: f64,
    pub kind: SurfaceKind,
    patches: Vec<HybridPatch>,
    #[serde(skip)]
    lookup: HashMap<(u8, u8, i32, i32), usize>,
    #[serde(skip)]
    levels: Vec<u8>,
}

impl<'de> Deserialize<'de> for HybridField {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct WireField {
            world_id: String,
            revision: u64,
            radius_m: f64,
            #[serde(default)]
            kind: SurfaceKind,
            patches: Vec<HybridPatch>,
        }
        let wire = WireField::deserialize(deserializer)?;
        Self::with_kind(
            wire.world_id,
            wire.revision,
            wire.radius_m,
            wire.patches,
            wire.kind,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl HybridField {
    pub fn new(
        world_id: String,
        revision: u64,
        radius_m: f64,
        patches: Vec<HybridPatch>,
    ) -> Result<Self> {
        Self::with_kind(world_id, revision, radius_m, patches, SurfaceKind::Residual)
    }

    pub fn with_kind(
        world_id: String,
        revision: u64,
        radius_m: f64,
        patches: Vec<HybridPatch>,
        kind: SurfaceKind,
    ) -> Result<Self> {
        let mut field = Self {
            world_id,
            revision,
            radius_m,
            kind,
            patches,
            lookup: HashMap::new(),
            levels: Vec::new(),
        };
        field.validate()?;
        field.lookup = field
            .patches
            .iter()
            .enumerate()
            .map(|(i, p)| ((p.face, p.level, p.x, p.y), i))
            .collect();
        field.levels = field.patches.iter().map(|patch| patch.level).collect();
        field.levels.sort_unstable_by(|a, b| b.cmp(a));
        field.levels.dedup();
        Ok(field)
    }

    pub fn from_json(json: &str) -> Result<Self> {
        if json.len() > 100 * 1024 * 1024 {
            bail!("hybrid JSON exceeds 100 MiB");
        }
        Ok(serde_json::from_str(json)?)
    }

    pub fn validate(&self) -> Result<()> {
        if self.world_id.is_empty() || self.world_id.len() > 256 {
            bail!("invalid hybrid world ID");
        }
        if self.revision > 9_007_199_254_740_991 {
            bail!("hybrid revision exceeds JavaScript integer precision");
        }
        if !self.radius_m.is_finite() || !(10_000.0..=100_000_000.0).contains(&self.radius_m) {
            bail!("invalid hybrid radius");
        }
        if self.patches.len() > MAX_PATCHES {
            bail!("too many hybrid patches");
        }
        let mut keys = std::collections::HashSet::new();
        for p in &self.patches {
            if p.face >= 6
                || p.level > 8
                || p.x.unsigned_abs() > 100_000
                || p.y.unsigned_abs() > 100_000
            {
                bail!("invalid hybrid chart coordinate");
            }
            if !keys.insert((p.face, p.level, p.x, p.y)) {
                bail!("duplicate hybrid patch");
            }
            if p.values.len() != PATCH_SIDE * PATCH_SIDE
                || p.values.iter().any(|v| {
                    !v.is_finite()
                        || f64::from(v.abs())
                            > if self.kind == SurfaceKind::Height {
                                MAX_HEIGHT_M
                            } else {
                                MAX_RESIDUAL_M + 0.001
                            }
                })
            {
                bail!("invalid hybrid residual payload");
            }
        }
        Ok(())
    }

    pub fn radius_m(&self) -> f64 {
        self.radius_m
    }
    pub fn patches(&self) -> &[HybridPatch] {
        &self.patches
    }

    pub fn source_spacing(&self, direction: [f64; 3]) -> Option<f64> {
        let p = normalize(direction);
        for &level in self.levels.iter().rev() {
            let display_spacing = NATIVE_MPP * f64::from(1u32 << level);
            for (face, [n, _, _]) in FACE_BASES.iter().enumerate() {
                if dot(p, *n) <= 0.55 {
                    continue;
                }
                let plane = chart_plane(face as u8, p, self.radius_m);
                let x = (plane[0] / (display_spacing * PATCH_CORE as f64)).floor() as i32;
                let y = (plane[1] / (display_spacing * PATCH_CORE as f64)).floor() as i32;
                if self.patch(face as u8, level, x, y).is_some() {
                    return Some(if level <= 2 {
                        30.0
                    } else if level <= 4 {
                        240.0
                    } else {
                        7680.0
                    });
                }
            }
        }
        None
    }

    fn patch(&self, face: u8, level: u8, x: i32, y: i32) -> Option<&HybridPatch> {
        self.lookup
            .get(&(face, level, x, y))
            .map(|&i| &self.patches[i])
    }

    fn node(&self, face: u8, level: u8, i: i32, j: i32) -> Option<f64> {
        let x = j.div_euclid(PATCH_CORE as i32);
        let y = i.div_euclid(PATCH_CORE as i32);
        if let Some(p) = self.patch(face, level, x, y) {
            return Some(f64::from(
                p.values[(i.rem_euclid(256) as usize + 1) * PATCH_SIDE
                    + j.rem_euclid(256) as usize
                    + 1],
            ));
        }
        // A single canonical owner per pixel prevents query-dependent overlap rounding seams.
        // Missing owners can use a deterministically ordered neighbouring halo during arrival.
        for py in y - 1..=y + 1 {
            for px in x - 1..=x + 1 {
                let li = i - py * 256 + 1;
                let lj = j - px * 256 + 1;
                if (0..PATCH_SIDE as i32).contains(&li) && (0..PATCH_SIDE as i32).contains(&lj) {
                    if let Some(p) = self.patch(face, level, px, py) {
                        return Some(f64::from(p.values[li as usize * PATCH_SIDE + lj as usize]));
                    }
                }
            }
        }
        None
    }

    fn chart_sample(&self, face: u8, level: u8, plane: [f64; 2]) -> (f64, f64) {
        let spacing = NATIVE_MPP * f64::from(1u32 << level);
        let gx = plane[0] / spacing;
        let gy = plane[1] / spacing;
        let x = (gx / 256.0).floor() as i32;
        let y = (gy / 256.0).floor() as i32;
        if self.patch(face, level, x, y).is_none() {
            return (0.0, 0.0);
        }
        let dx = [gx - f64::from(x) * 256.0, f64::from(x + 1) * 256.0 - gx];
        let dy = [gy - f64::from(y) * 256.0, f64::from(y + 1) * 256.0 - gy];
        let mut coverage = 1.0;
        for (axis, distances) in [(0, dx), (1, dy)] {
            for (side, distance) in distances.into_iter().enumerate() {
                let offset = if side == 0 { -1 } else { 1 };
                let (nx, ny) = if axis == 0 {
                    (x + offset, y)
                } else {
                    (x, y + offset)
                };
                if self.patch(face, level, nx, ny).is_none() {
                    coverage *= smooth(distance / FEATHER_PIXELS);
                }
            }
        }
        for (sx, distance_x) in dx.into_iter().enumerate() {
            for (sy, distance_y) in dy.into_iter().enumerate() {
                if self
                    .patch(
                        face,
                        level,
                        x + if sx == 0 { -1 } else { 1 },
                        y + if sy == 0 { -1 } else { 1 },
                    )
                    .is_none()
                {
                    coverage *= 1.0
                        - (1.0 - smooth(distance_x / FEATHER_PIXELS))
                            * (1.0 - smooth(distance_y / FEATHER_PIXELS));
                }
            }
        }
        if coverage <= 0.0 {
            return (0.0, 0.0);
        }
        let qx = gx - 0.5;
        let qy = gy - 0.5;
        let j = qx.floor() as i32;
        let i = qy.floor() as i32;
        let tx = qx - f64::from(j);
        let ty = qy - f64::from(i);
        let Some(a) = self.node(face, level, i, j) else {
            return (0.0, 0.0);
        };
        let Some(b) = self.node(face, level, i, j + 1) else {
            return (0.0, 0.0);
        };
        let Some(c) = self.node(face, level, i + 1, j) else {
            return (0.0, 0.0);
        };
        let Some(d) = self.node(face, level, i + 1, j + 1) else {
            return (0.0, 0.0);
        };
        let linear = (a * (1.0 - tx) + b * tx) * (1.0 - ty) + (c * (1.0 - tx) + d * tx) * ty;
        // Cubic sampling keeps the native centres exact and makes derivatives
        // continuous between30m samples. It does not invent learned sub30m data.
        let mut rows = [0.0; 4];
        if self.kind == SurfaceKind::Height {
            for (row, value) in rows.iter_mut().enumerate() {
                let mut samples = [0.0; 4];
                for (column, sample) in samples.iter_mut().enumerate() {
                    let Some(node) =
                        self.node(face, level, i + row as i32 - 1, j + column as i32 - 1)
                    else {
                        return (coverage * linear, coverage);
                    };
                    *sample = node;
                }
                *value = cubic(samples[0], samples[1], samples[2], samples[3], tx);
            }
            let value =
                cubic(rows[0], rows[1], rows[2], rows[3], ty).clamp(-MAX_HEIGHT_M, MAX_HEIGHT_M);
            (coverage * value, coverage)
        } else {
            (coverage * linear, coverage)
        }
    }

    pub fn raw_delta(&self, direction: [f64; 3]) -> f64 {
        self.raw_sample(direction).0
    }

    fn raw_sample(&self, direction: [f64; 3]) -> (f64, f64) {
        let p = normalize(direction);
        let mut total = 0.0;
        let mut weight_sum = 0.0;
        let mut coverage_sum = 0.0;
        for (face, [n, _, _]) in FACE_BASES.into_iter().enumerate() {
            let weight = (dot(p, n) - 0.55).max(0.0).powi(4);
            if weight > 0.0 {
                weight_sum += weight;
                let plane = chart_plane(face as u8, p, self.radius_m);
                let mut value = 0.0;
                let mut coverage = 0.0;
                // Fine fields blend into the previous learned preview, not
                // back into a flat procedural parent at every native tile edge.
                for &level in &self.levels {
                    let (next, available) = self.chart_sample(face as u8, level, plane);
                    value = next + value * (1.0 - available);
                    coverage = available + coverage * (1.0 - available);
                }
                total += weight * value;
                coverage_sum += weight * coverage;
            }
        }
        if weight_sum > 0.0 {
            (total / weight_sum, coverage_sum / weight_sum)
        } else {
            (0.0, 0.0)
        }
    }

    /// Keeps the procedural coastline, reservoirs and river beds authoritative.
    pub fn protected_delta(
        &self,
        direction: [f64; 3],
        height_m: f64,
        water_level_m: Option<f64>,
        water_body_id: Option<u64>,
        river: f64,
        ocean: bool,
        lake: bool,
    ) -> f64 {
        if !height_m.is_finite() || ocean || lake || height_m <= 0.0 || !river.is_finite() {
            return 0.0;
        }
        if self.kind == SurfaceKind::Height {
            let (target, coverage) = self.raw_sample(direction);
            let delta = target - coverage * height_m;
            // Preserve shorelines and standing water while allowing kilometre
            // scale learned valleys inland. River blend stays continuous.
            let shore = smooth(height_m / 200.0);
            let mut floor = 0.5;
            if let Some(level) = water_level_m {
                if !level.is_finite() || height_m <= level {
                    return 0.0;
                }
                if water_body_id.is_some() || level > 0.0 {
                    floor = level + 0.5;
                }
            }
            return delta.max(floor - height_m) * shore * (1.0 - river.clamp(0.0, 1.0)).powi(2);
        }
        let mut limit = MAX_RESIDUAL_M.min(height_m * 0.30);
        if let Some(level) = water_level_m {
            if !level.is_finite() || height_m <= level {
                return 0.0;
            }
            if water_body_id.is_some() || level > 0.0 {
                limit = limit.min((height_m - level) * 0.25);
            }
        }
        let river_weight = (1.0 - river.clamp(0.0, 1.0)).powi(2);
        self.raw_delta(direction).clamp(-limit, limit) * river_weight
    }
}

fn cubic(a: f64, b: f64, c: f64, d: f64, t: f64) -> f64 {
    b + 0.5 * t * (c - a + t * (2.0 * a - 5.0 * b + 4.0 * c - d + t * (3.0 * (b - c) + d - a)))
}

fn smooth(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Conformal chart coordinates in metres. Call only on the chart's visible hemisphere.
pub fn chart_plane(face: u8, direction: [f64; 3], radius_m: f64) -> [f64; 2] {
    let [n, e, b] = FACE_BASES[usize::from(face)];
    let p = normalize(direction);
    let factor = 2.0 * radius_m / (1.0 + dot(p, n));
    [factor * dot(p, e), factor * dot(p, b)]
}

/// Inverse chart, valid beyond cube edges and at both geographic poles.
pub fn chart_direction(face: u8, x_m: f64, y_m: f64, radius_m: f64) -> [f64; 3] {
    let [n, e, b] = FACE_BASES[usize::from(face)];
    let a = x_m / (2.0 * radius_m);
    let b_coord = y_m / (2.0 * radius_m);
    let square = a * a + b_coord * b_coord;
    let denominator = 1.0 + square;
    std::array::from_fn(|axis| {
        ((1.0 - square) * n[axis] + 2.0 * a * e[axis] + 2.0 * b_coord * b[axis]) / denominator
    })
}
