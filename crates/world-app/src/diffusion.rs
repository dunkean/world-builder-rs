//! Original spherical world as the pretrained terrain model's five coarse inputs.
//! Precipitation and seasonal variability are explicit climate proxies, not observations.

use anyhow::{ensure, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use world_core::{hybrid::chart_direction, World};

pub const PROJECTION: &str = "stereo-six-v1";
pub const REVISION: &str = "procedural-climate-proxies-cellmean4-v2";

#[derive(Clone, Debug, Deserialize)]
pub struct InputQuery {
    pub face: u8,
    pub i0: i32,
    pub j0: i32,
    pub height: usize,
    pub width: usize,
    pub step: u32,
}

impl InputQuery {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.face < 6, "chart face must be 0..5");
        ensure!(
            (1..=128).contains(&self.height) && (1..=128).contains(&self.width),
            "input dimensions must be 1..128"
        );
        ensure!(
            self.step == 1 || self.step == 256,
            "input step must be 1 or 256"
        );
        ensure!(
            self.i0.unsigned_abs() <= 1_000_000 && self.j0.unsigned_abs() <= 1_000_000,
            "input origin out of range"
        );
        Ok(())
    }
}

#[derive(Serialize)]
pub struct InputWindow {
    pub world_id: String,
    pub config_fingerprint: String,
    pub radius_m: f64,
    pub seed: u64,
    pub face: u8,
    pub i0: i32,
    pub j0: i32,
    pub height: usize,
    pub width: usize,
    pub step: u32,
    pub projection: &'static str,
    pub conditioning_revision: &'static str,
    pub units: [&'static str; 5],
    pub climate_source: &'static str,
    pub sampling: &'static str,
    pub channels: Vec<Vec<f32>>,
}

pub fn fingerprint(world: &World) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(world.id.as_bytes());
    hash.update(world_core::GENERATOR_VERSION.as_bytes());
    hash.update(super::SURFACE_REVISION.as_bytes());
    hash.update(PROJECTION.as_bytes());
    hash.update(REVISION.as_bytes());
    hash.finalize().to_hex().to_string()
}

pub fn build(world: &World, query: InputQuery) -> Result<InputWindow> {
    query.validate()?;
    let spacing = f64::from(query.step) * world_core::hybrid::NATIVE_MPP;
    let rows: Vec<[f32; 5]> = (0..query.height * query.width)
        .into_par_iter()
        .map(|index| {
            let subdivisions = if query.step == 256 { 4 } else { 1 };
            let mut mean = [0.0_f64; 5];
            for row in 0..subdivisions {
                for column in 0..subdivisions {
                    let x = (f64::from(query.j0)
                        + (index % query.width) as f64
                        + (column as f64 + 0.5) / subdivisions as f64)
                        * spacing;
                    let y = (f64::from(query.i0)
                        + (index / query.width) as f64
                        + (row as f64 + 0.5) / subdivisions as f64)
                        * spacing;
                    let channels = point_channels(world, query.face, x, y);
                    for channel in 0..5 {
                        mean[channel] += f64::from(channels[channel]);
                    }
                }
            }
            mean.map(|sum| (sum / (subdivisions * subdivisions) as f64) as f32)
        })
        .collect();
    ensure!(
        rows.iter().flatten().all(|v| v.is_finite()),
        "non-finite original world conditioning"
    );
    let channels = (0..5)
        .map(|channel| rows.iter().map(|row| row[channel]).collect())
        .collect();
    Ok(InputWindow {
        world_id: world.id.clone(), config_fingerprint: fingerprint(world), radius_m: world.config.radius_m,
        seed: world.config.seed, face: query.face, i0: query.i0, j0: query.j0,
        height: query.height, width: query.width, step: query.step, projection: PROJECTION,
        conditioning_revision: REVISION,
        units: ["metres_signed", "degrees_C", "degrees_C_times_100", "millimetres_per_year", "percent_coefficient_of_variation"],
        climate_source: "procedural temperature/moisture; seasonal and precipitation proxies; no soil/glacier model channels",
        sampling: if query.step == 256 { "stratified-4x4-cell-mean" } else { "native-point-centre" },
        channels,
    })
}

fn point_channels(world: &World, face: u8, x: f64, y: f64) -> [f32; 5] {
    let direction = chart_direction(face, x, y, world.config.radius_m);
    let sample = world.sample(direction);
    let moisture = sample.moisture.clamp(0.0, 1.0);
    let seasonal_std_c =
        3.0 + 12.0 * direction[1].abs() * world.config.axial_tilt_deg.to_radians().sin();
    [
        sample.height_m,
        sample.temperature_c,
        (seasonal_std_c * 100.0) as f32,
        100.0 + 2900.0 * moisture,
        15.0 + 35.0 * (1.0 - moisture),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conditioning_bounds_are_explicit() {
        let mut q = InputQuery {
            face: 0,
            i0: -64,
            j0: -64,
            height: 64,
            width: 64,
            step: 256,
        };
        assert!(q.validate().is_ok());
        q.height = 129;
        assert!(q.validate().is_err());
        q.height = 64;
        q.step = 2;
        assert!(q.validate().is_err());
    }

    #[test]
    fn coarse_conditioning_averages_before_signed_square_root() {
        let world = World::generate(world_core::WorldConfig {
            resolution: 8,
            erosion_steps: 0,
            detail_m: 800.0,
            ..Default::default()
        })
        .unwrap();
        let query = InputQuery {
            face: 4,
            i0: -1,
            j0: 0,
            height: 1,
            width: 1,
            step: 256,
        };
        let window = build(&world, query).unwrap();
        let mut expected = [0.0; 5];
        for row in 0..4 {
            for column in 0..4 {
                let point = point_channels(
                    &world,
                    4,
                    (column as f64 + 0.5) * 1920.0,
                    -7680.0 + (row as f64 + 0.5) * 1920.0,
                );
                for channel in 0..5 {
                    expected[channel] += f64::from(point[channel]) / 16.0;
                }
            }
        }
        for (channel, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                window.channels[channel][0].to_bits(),
                (expected as f32).to_bits()
            );
        }
        assert_eq!(window.sampling, "stratified-4x4-cell-mean");
    }
}
