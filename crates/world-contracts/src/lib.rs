//! CPU reference for deterministic, conservative terrain refinement.
//!
//! A family is compiled as one grid and then split into four views. Every
//! inherited vertex and outside edge stays fixed. Per-parent-cell altitude
//! means are conserved using a documented bilinear/trapezoidal quadrature.
//! This geometric certificate does not certify drainage or sediment mass.

use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};

pub const ALGORITHM_VERSION: &str = "cell-bubble-v1";

/// A square vertex grid. Weights are positive cell areas, or positive nodal
/// metric samples that are averaged into cell areas for compatibility.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Grid {
    pub size: usize,
    pub heights_m: Vec<f64>,
    pub weights: Vec<f64>,
}

impl Grid {
    pub fn uniform(size: usize, heights_m: Vec<f64>) -> Self {
        Self {
            size,
            heights_m,
            weights: vec![1.0; size.saturating_sub(1).pow(2)],
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            (3..=513).contains(&self.size),
            "grid size must be 3..=513 vertices"
        );
        ensure!(
            self.heights_m.len() == self.size * self.size,
            "height count differs from grid size"
        );
        ensure!(
            self.heights_m.iter().all(|v| v.is_finite()),
            "heights must be finite"
        );
        let cells = (self.size - 1).pow(2);
        ensure!(
            self.weights.len() == cells || self.weights.len() == self.size.pow(2),
            "weights must contain one positive area per cell or metric per vertex"
        );
        ensure!(
            self.weights.iter().all(|v| v.is_finite() && *v > 0.0),
            "weights must be finite and positive"
        );
        ensure!(
            self.weights.iter().sum::<f64>().is_finite(),
            "total grid area exceeds finite numeric range"
        );
        Ok(())
    }

    pub fn height(&self, x: usize, y: usize) -> f64 {
        self.heights_m[y * self.size + x]
    }

    pub fn cell_area(&self, x: usize, y: usize) -> f64 {
        if self.weights.len() == self.size.pow(2) {
            [
                self.weights[y * self.size + x],
                self.weights[y * self.size + x + 1],
                self.weights[(y + 1) * self.size + x],
                self.weights[(y + 1) * self.size + x + 1],
            ]
            .iter()
            .sum::<f64>()
                * 0.25
        } else {
            self.weights[y * (self.size - 1) + x]
        }
    }

    pub fn weighted_mean(&self) -> f64 {
        let mut area = 0.0;
        let mut integral = 0.0;
        for y in 0..self.size - 1 {
            for x in 0..self.size - 1 {
                let a = self.cell_area(x, y);
                integral += a * cell_mean(self, x, y);
                area += a;
            }
        }
        integral / area
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefinementOptions {
    pub amplitude_m: f64,
    pub quantization_m: f64,
    pub mean_tolerance_m: f64,
    pub preserve_global_first_moments: bool,
}

impl Default for RefinementOptions {
    fn default() -> Self {
        Self {
            amplitude_m: 50.0,
            quantization_m: 0.01,
            mean_tolerance_m: 0.01,
            preserve_global_first_moments: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Certificate {
    pub passed: bool,
    pub algorithm_version: String,
    pub quantization_m: f64,
    pub max_parent_cell_mean_error_m: f64,
    pub weighted_mean_error_m: f64,
    pub max_first_moment_error_m: f64,
    pub max_boundary_error_m: f64,
    pub max_parent_anchor_error_m: f64,
    pub max_child_seam_error_m: f64,
    pub max_residual_m: f64,
    pub input_quantization_error_m: f64,
    pub amplitude_scale: f64,
    pub digest: String,
    pub drainage_constrained: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefinedFamily {
    /// Child order: northwest, northeast, southwest, southeast (x increases east).
    pub children: [Grid; 4],
    pub certificate: Certificate,
}

impl RefinedFamily {
    /// Assemble a family without averaging duplicated edges. Mismatched edges
    /// are corruption, and are rejected instead of silently repaired.
    pub fn assemble(&self) -> Result<Grid> {
        for child in &self.children {
            child.validate()?;
        }
        let n = self.children[0].size;
        ensure!(
            self.children.iter().all(|c| c.size == n),
            "child dimensions differ"
        );
        let side = 2 * (n - 1) + 1;
        let mut heights = vec![f64::NAN; side * side];
        let mut weights = vec![0.0; (side - 1).pow(2)];
        for (k, child) in self.children.iter().enumerate() {
            let ox = (k % 2) * (n - 1);
            let oy = (k / 2) * (n - 1);
            for y in 0..n {
                for x in 0..n {
                    let index = (oy + y) * side + ox + x;
                    let v = child.height(x, y);
                    ensure!(
                        heights[index].is_nan() || heights[index].to_bits() == v.to_bits(),
                        "child seam mismatch"
                    );
                    heights[index] = v;
                }
            }
            for y in 0..n - 1 {
                for x in 0..n - 1 {
                    weights[(oy + y) * (side - 1) + ox + x] = child.cell_area(x, y);
                }
            }
        }
        Ok(Grid {
            size: side,
            heights_m: heights,
            weights,
        })
    }
}

pub fn refine(parent: &Grid, seed: u64, amplitude_m: f64) -> Result<RefinedFamily> {
    refine_with_options(
        parent,
        seed,
        &RefinementOptions {
            amplitude_m,
            ..Default::default()
        },
    )
}

pub fn refine_with_options(
    parent: &Grid,
    seed: u64,
    options: &RefinementOptions,
) -> Result<RefinedFamily> {
    parent.validate()?;
    ensure!(
        options.amplitude_m.is_finite() && options.amplitude_m >= 0.0,
        "amplitude must be finite and nonnegative"
    );
    ensure!(
        options.quantization_m.is_finite() && options.quantization_m > 0.0,
        "quantization must be positive"
    );
    ensure!(
        options.mean_tolerance_m.is_finite()
            && options.mean_tolerance_m >= options.quantization_m * 0.5,
        "mean tolerance must cover half a quantization step"
    );

    // Canonical integer storage is decoded before any constraints are built.
    let mut canonical = parent.clone();
    let mut input_error: f64 = 0.0;
    for (dst, src) in canonical.heights_m.iter_mut().zip(&parent.heights_m) {
        *dst = quantize(*src, options.quantization_m)?;
        input_error = input_error.max((*dst - *src).abs());
    }
    let base = reconstruct(&canonical);
    let side = base.size;
    let mut residual = vec![0.0; side * side];
    // Only new internal edge midpoints get independent degrees of freedom.
    // Cell centers compensate their adjacent edge values analytically.
    for y in 1..side - 1 {
        for x in 1..side - 1 {
            if (x % 2) != (y % 2) {
                let dx = (x.min(side - 1 - x) as f64 / 4.0).min(1.0);
                let dy = (y.min(side - 1 - y) as f64 / 4.0).min(1.0);
                residual[y * side + x] = signed_noise(seed, x, y) * dx.powi(2) * dy.powi(2);
            }
        }
    }
    compensate_cell_means(&mut residual, side);
    if options.preserve_global_first_moments {
        project_first_moments(&mut residual, &base);
    }
    let maximum = residual.iter().fold(0.0_f64, |a, v| a.max(v.abs()));
    let amplitude_scale = if maximum > 0.0 {
        options.amplitude_m / maximum
    } else {
        0.0
    };
    let mut fine = base.clone();
    for (i, height) in fine.heights_m.iter_mut().enumerate() {
        *height = quantize(
            base.heights_m[i] + amplitude_scale * residual[i],
            options.quantization_m,
        )?;
    }
    let children = split(&fine, parent.size);
    let mut family = RefinedFamily {
        children,
        certificate: Certificate {
            passed: false,
            algorithm_version: ALGORITHM_VERSION.to_string(),
            quantization_m: options.quantization_m,
            max_parent_cell_mean_error_m: 0.0,
            weighted_mean_error_m: 0.0,
            max_first_moment_error_m: 0.0,
            max_boundary_error_m: 0.0,
            max_parent_anchor_error_m: 0.0,
            max_child_seam_error_m: 0.0,
            max_residual_m: 0.0,
            input_quantization_error_m: input_error,
            amplitude_scale,
            digest: String::new(),
            drainage_constrained: false,
        },
    };
    let assembled = family.assemble()?;
    let c = &mut family.certificate;
    for y in 0..parent.size - 1 {
        for x in 0..parent.size - 1 {
            let reduced = (cell_mean(&assembled, 2 * x, 2 * y)
                + cell_mean(&assembled, 2 * x + 1, 2 * y)
                + cell_mean(&assembled, 2 * x, 2 * y + 1)
                + cell_mean(&assembled, 2 * x + 1, 2 * y + 1))
                * 0.25;
            c.max_parent_cell_mean_error_m = c
                .max_parent_cell_mean_error_m
                .max((reduced - cell_mean(&canonical, x, y)).abs());
        }
    }
    for y in 0..side {
        for x in 0..side {
            let i = y * side + x;
            let deviation = (assembled.heights_m[i] - base.heights_m[i]).abs();
            c.max_residual_m = c.max_residual_m.max(deviation);
            if x == 0 || y == 0 || x == side - 1 || y == side - 1 {
                c.max_boundary_error_m = c.max_boundary_error_m.max(
                    (assembled.heights_m[i] - quantize(base.heights_m[i], options.quantization_m)?)
                        .abs(),
                );
            }
            if x % 2 == 0 && y % 2 == 0 {
                c.max_parent_anchor_error_m = c.max_parent_anchor_error_m.max(deviation);
            }
        }
    }
    c.weighted_mean_error_m = (assembled.weighted_mean() - canonical.weighted_mean()).abs();
    let difference: Vec<_> = assembled
        .heights_m
        .iter()
        .zip(&base.heights_m)
        .map(|(a, b)| a - b)
        .collect();
    let moments = first_moments(&difference, &base);
    c.max_first_moment_error_m = moments[0].abs().max(moments[1].abs());
    let mut hash = blake3::Hasher::new();
    hash.update(ALGORITHM_VERSION.as_bytes());
    hash.update(&options.quantization_m.to_le_bytes());
    hash.update(&(assembled.size as u64).to_le_bytes());
    for height in &assembled.heights_m {
        hash.update(&((*height / options.quantization_m).round() as i32).to_le_bytes());
    }
    for weight in &assembled.weights {
        hash.update(&weight.to_le_bytes());
    }
    c.digest = hash.finalize().to_hex().to_string();
    c.passed = c.max_parent_cell_mean_error_m <= options.mean_tolerance_m + 1e-9
        && c.weighted_mean_error_m <= options.mean_tolerance_m + 1e-9
        && c.max_boundary_error_m == 0.0
        && c.max_parent_anchor_error_m == 0.0
        && c.max_residual_m <= options.amplitude_m + options.quantization_m * 0.5 + 1e-9
        && (!options.preserve_global_first_moments
            || c.max_first_moment_error_m <= options.mean_tolerance_m + 1e-9);
    ensure!(c.passed, "quantized refinement certificate failed: {c:?}");
    Ok(family)
}

fn quantize(height: f64, step: f64) -> Result<f64> {
    let integer = (height / step).round();
    if !integer.is_finite() || integer < i32::MIN as f64 || integer > i32::MAX as f64 {
        bail!("altitude exceeds canonical signed 32-bit storage");
    }
    Ok(integer * step)
}

fn cell_mean(grid: &Grid, x: usize, y: usize) -> f64 {
    (grid.height(x, y) + grid.height(x + 1, y) + grid.height(x, y + 1) + grid.height(x + 1, y + 1))
        * 0.25
}

fn reconstruct(parent: &Grid) -> Grid {
    let side = 2 * (parent.size - 1) + 1;
    let mut heights = vec![0.0; side * side];
    for y in 0..side {
        for x in 0..side {
            let px = (x / 2).min(parent.size - 2);
            let py = (y / 2).min(parent.size - 2);
            let u = x as f64 * 0.5 - px as f64;
            let v = y as f64 * 0.5 - py as f64;
            heights[y * side + x] = parent.height(px, py) * (1.0 - u) * (1.0 - v)
                + parent.height(px + 1, py) * u * (1.0 - v)
                + parent.height(px, py + 1) * (1.0 - u) * v
                + parent.height(px + 1, py + 1) * u * v;
        }
    }
    let mut weights = vec![0.0; (side - 1).pow(2)];
    for y in 0..side - 1 {
        for x in 0..side - 1 {
            weights[y * (side - 1) + x] = parent.cell_area(x / 2, y / 2) * 0.25;
        }
    }
    Grid {
        size: side,
        heights_m: heights,
        weights,
    }
}

fn compensate_cell_means(values: &mut [f64], side: usize) {
    for y in (1..side - 1).step_by(2) {
        for x in (1..side - 1).step_by(2) {
            values[y * side + x] = -0.5
                * (values[(y - 1) * side + x]
                    + values[(y + 1) * side + x]
                    + values[y * side + x - 1]
                    + values[y * side + x + 1]);
        }
    }
}

fn signed_noise(seed: u64, x: usize, y: usize) -> f64 {
    let mut value = seed
        ^ (x as u64).wrapping_mul(0x9e3779b97f4a7c15)
        ^ (y as u64).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^= value >> 31;
    (value >> 11) as f64 * (2.0 / (1_u64 << 53) as f64) - 1.0
}

// First moments of the residual, normalized by family area. Coordinates lie
// in [-1, 1], so the certificate keeps an interpretable metre-valued error.
// The same trapezoidal quadrature is used for proposal and verification.
fn first_moments(values: &[f64], grid: &Grid) -> [f64; 2] {
    let side = grid.size;
    let mut result = [0.0; 2];
    let mut area = 0.0;
    for y in 0..side - 1 {
        for x in 0..side - 1 {
            let a = grid.cell_area(x, y);
            area += a;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let px = x + dx;
                let py = y + dy;
                let mass = a * 0.25 * values[py * side + px];
                result[0] += mass * (2.0 * px as f64 / (side - 1) as f64 - 1.0);
                result[1] += mass * (2.0 * py as f64 / (side - 1) as f64 - 1.0);
            }
        }
    }
    [result[0] / area, result[1] / area]
}

fn project_first_moments(residual: &mut [f64], base: &Grid) {
    let side = base.size;
    // Two directions already in the kernel of all parent-cell means. An edge
    // shared by equal-area cells contributes no global first-moment error;
    // unequal areas give an exact moment gradient in its transverse axis.
    let mut bx = vec![0.0; side * side];
    let mut by = vec![0.0; side * side];
    for y in 1..side - 1 {
        for x in 1..side - 1 {
            if x % 2 == 1 && y % 2 == 0 {
                by[y * side + x] = base.cell_area(x, y - 1) - base.cell_area(x, y);
            } else if x % 2 == 0 && y % 2 == 1 {
                bx[y * side + x] = base.cell_area(x - 1, y) - base.cell_area(x, y);
            }
        }
    }
    compensate_cell_means(&mut bx, side);
    compensate_cell_means(&mut by, side);
    let a = first_moments(&bx, base);
    let b = first_moments(&by, base);
    let r = first_moments(residual, base);
    // Each direction controls only its own axis analytically. Degenerate
    // axes already have zero residual moment, so they need no correction.
    let cx = if a[0].abs() > 1e-20 { r[0] / a[0] } else { 0.0 };
    let cy = if b[1].abs() > 1e-20 { r[1] / b[1] } else { 0.0 };
    for i in 0..residual.len() {
        residual[i] -= cx * bx[i] + cy * by[i];
    }
    compensate_cell_means(residual, side);
}

fn split(fine: &Grid, child_size: usize) -> [Grid; 4] {
    std::array::from_fn(|k| {
        let ox = k % 2 * (child_size - 1);
        let oy = k / 2 * (child_size - 1);
        let mut heights = Vec::with_capacity(child_size.pow(2));
        let mut weights = Vec::with_capacity((child_size - 1).pow(2));
        for y in 0..child_size {
            for x in 0..child_size {
                heights.push(fine.height(ox + x, oy + y));
            }
        }
        for y in 0..child_size - 1 {
            for x in 0..child_size - 1 {
                weights.push(fine.cell_area(ox + x, oy + y));
            }
        }
        Grid {
            size: child_size,
            heights_m: heights,
            weights,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent() -> Grid {
        let size: usize = 9;
        let heights = (0..size * size)
            .map(|i| {
                let x = (i % size) as f64;
                let y = (i / size) as f64;
                140.0 + 3.1 * x - 7.4 * y + 0.24 * x * y
            })
            .collect();
        let weights = (0..(size - 1).pow(2))
            .map(|i| 1.0 + (i % 7) as f64 * 0.2)
            .collect();
        Grid {
            size,
            heights_m: heights,
            weights,
        }
    }

    #[test]
    fn family_preserves_local_moments_and_exact_edges_after_quantization() {
        let p = parent();
        let family = refine(&p, 42, 20.0).unwrap();
        assert!(family.certificate.passed);
        assert!(family.certificate.max_parent_cell_mean_error_m <= 0.005 + 1e-9);
        assert!(family.certificate.weighted_mean_error_m <= 0.005 + 1e-9);
        assert_eq!(family.certificate.max_child_seam_error_m, 0.0);
        assert_eq!(family.certificate.max_parent_anchor_error_m, 0.0);
        let combined = family.assemble().unwrap();
        assert_eq!(combined.size, 17);
        assert!(family.certificate.max_residual_m > 10.0);
        assert!(family.certificate.max_residual_m <= 20.005 + 1e-9);
    }

    #[test]
    fn repeated_generation_and_order_do_not_change_packet() {
        let p = parent();
        let first = refine(&p, 1, 12.0).unwrap();
        let other = refine(&p, 2, 12.0).unwrap();
        let repeated = refine(&p, 1, 12.0).unwrap();
        assert_eq!(first.certificate.digest, repeated.certificate.digest);
        assert_ne!(first.certificate.digest, other.certificate.digest);
        for i in [3, 1, 0, 2] {
            assert_eq!(first.children[i].heights_m, repeated.children[i].heights_m);
        }
    }

    #[test]
    fn independent_neighbor_parents_share_identical_refined_boundary() {
        let left = parent();
        let mut right = parent();
        for y in 0..left.size {
            right.heights_m[y * right.size] = left.height(left.size - 1, y);
        }
        let a = refine(&left, 17, 40.0).unwrap().assemble().unwrap();
        let b = refine(&right, 29, 80.0).unwrap().assemble().unwrap();
        for y in 0..a.size {
            assert_eq!(a.height(a.size - 1, y).to_bits(), b.height(0, y).to_bits());
        }
    }

    #[test]
    fn malformed_and_out_of_range_inputs_are_rejected() {
        let mut p = parent();
        assert!(refine(&p, 0, f64::NAN).is_err());
        p.weights[2] = -1.0;
        assert!(refine(&p, 0, 20.0).is_err());
        p.weights[2] = 1.0;
        p.heights_m[3] = 30_000_000.0;
        assert!(refine(&p, 0, 20.0).is_err());
    }

    #[test]
    fn zero_amplitude_is_quantized_inherited_surface() {
        let mut p = parent();
        p.weights = vec![1.0; p.size.pow(2)];
        let family = refine(&p, 0, 0.0).unwrap();
        assert!(family.certificate.max_residual_m <= 0.005 + 1e-9);
        assert!(family.certificate.passed);
    }

    #[test]
    fn corrupted_shared_edge_is_rejected() {
        let mut family = refine(&parent(), 7, 10.0).unwrap();
        family.children[1].heights_m[0] += 0.01;
        assert!(family.assemble().is_err());
    }

    #[test]
    fn small_and_asymmetric_families_keep_all_moment_contracts() {
        for size in [3_usize, 4, 5, 17, 65] {
            for seed in 0..6 {
                let p = Grid {
                    size,
                    heights_m: (0..size.pow(2))
                        .map(|i| signed_noise(seed + 10, i % size, i / size) * 1200.0)
                        .collect(),
                    weights: (0..(size - 1).pow(2))
                        .map(|i| 1.0 + (i % (size - 1)) as f64 * 0.15)
                        .collect(),
                };
                let family = refine(&p, seed, 123.0).unwrap();
                assert!(family.certificate.passed);
                assert!(family.certificate.max_first_moment_error_m <= 0.005 + 1e-9);
            }
        }
    }
}
