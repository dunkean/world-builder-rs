//! Bounded presentation mesh; the geological atlas and its identity stay intact.
use anyhow::{bail, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use world_core::{geometry::direction, Sample, World, WorldConfig, WorldStats};

#[derive(Clone, Copy, Deserialize)]
pub struct PreviewQuery {
    #[serde(default = "default_resolution")]
    pub resolution: u32,
    pub phase: Option<f64>,
}

fn default_resolution() -> u32 {
    128
}

#[derive(Serialize)]
pub struct WorldPreview {
    pub id: String,
    pub config: WorldConfig,
    pub stats: WorldStats,
    pub sea_level_m: f32,
    pub seasonal_phase: Option<f64>,
    pub render_resolution: u32,
    pub cells: Vec<Sample>,
}

pub fn build(world: &World, query: PreviewQuery) -> Result<WorldPreview> {
    if !(8..=128).contains(&query.resolution) {
        bail!("preview resolution must be between 8 and 128");
    }
    if query.phase.is_some_and(|phase| !phase.is_finite()) {
        bail!("phase must be finite");
    }
    let n = query.resolution;
    let per_face = n as usize * n as usize;
    let cells = (0..6 * per_face)
        .into_par_iter()
        .map(|index| {
            let face = (index / per_face) as u8;
            let local = index % per_face;
            let p = direction(
                face,
                2.0 * ((local % n as usize) as f64 + 0.5) / f64::from(n) - 1.0,
                2.0 * ((local / n as usize) as f64 + 0.5) / f64::from(n) - 1.0,
            );
            query
                .phase
                .map_or_else(|| world.sample(p), |phase| world.seasonal_sample(p, phase))
        })
        .collect();
    Ok(WorldPreview {
        id: world.id.clone(),
        config: world.config.clone(),
        stats: world.stats.clone(),
        sea_level_m: world.sea_level_m,
        seasonal_phase: query.phase.map(|phase| phase.rem_euclid(1.0)),
        render_resolution: n,
        cells,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_preserves_atlas_identity_and_does_not_change_the_world() {
        let world = World::generate(WorldConfig {
            resolution: 16,
            ..Default::default()
        })
        .unwrap();
        let preview = build(
            &world,
            PreviewQuery {
                resolution: 8,
                phase: None,
            },
        )
        .unwrap();
        assert_eq!(preview.id, world.id);
        assert_eq!(preview.config.resolution, 16);
        assert_eq!(preview.stats.cells, 6 * 16 * 16);
        assert_eq!(preview.render_resolution, 8);
        assert_eq!(preview.cells.len(), 6 * 8 * 8);
        let expected = world.sample(preview.cells[0].direction);
        assert_eq!(preview.cells[0].height_m, expected.height_m);
        assert_eq!(preview.cells[0].water_m, expected.water_m);
        assert!(build(
            &world,
            PreviewQuery {
                resolution: 256,
                phase: None
            }
        )
        .is_err());
        assert!(build(
            &world,
            PreviewQuery {
                resolution: 8,
                phase: Some(f64::NAN)
            }
        )
        .is_err());
    }
}
