//! Bounded presentation mesh; the geological atlas and its identity stay intact.
use anyhow::{bail, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use world_core::{Sample, TileKey, World, WorldConfig, WorldStats};

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
    pub render_grid: &'static str,
    pub surface_revision: &'static str,
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
    // Consume the same canonical vertex grid as Tile L0: no sampling at cell
    // centres and no second interpolation in the browser.
    let faces: Result<Vec<Vec<Sample>>> = (0..6u8)
        .into_par_iter()
        .map(|face| {
            let mut tile = world.tile(
                TileKey {
                    face,
                    level: 0,
                    x: 0,
                    y: 0,
                },
                n,
            )?;
            if let Some(phase) = query.phase {
                for point in &mut tile.vertices {
                    let seasonal = world.seasonal_sample(point.direction, phase);
                    point.temperature_c = seasonal.temperature_c;
                    point.moisture = seasonal.moisture;
                    point.biome = seasonal.biome;
                    point.biome_mix = seasonal.biome_mix;
                }
            }
            Ok(tile.vertices)
        })
        .collect();
    let cells = faces?.into_iter().flatten().collect();
    Ok(WorldPreview {
        id: world.id.clone(),
        config: world.config.clone(),
        stats: world.stats.clone(),
        sea_level_m: world.sea_level_m,
        seasonal_phase: query.phase.map(|phase| phase.rem_euclid(1.0)),
        render_resolution: n,
        render_grid: "vertices",
        surface_revision: crate::SURFACE_REVISION,
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
        assert_eq!(preview.cells.len(), 6 * 9 * 9);
        assert_eq!(preview.render_grid, "vertices");
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
    #[test]
    fn orbital_preview_and_first_child_share_exact_canonical_samples() {
        let world = World::generate(WorldConfig {
            resolution: 16,
            ..Default::default()
        })
        .unwrap();
        let preview = build(
            &world,
            PreviewQuery {
                resolution: 16,
                phase: None,
            },
        )
        .unwrap();
        for face in 0..6u8 {
            let root = world
                .tile(
                    TileKey {
                        face,
                        level: 0,
                        x: 0,
                        y: 0,
                    },
                    16,
                )
                .unwrap();
            let offset = face as usize * 17 * 17;
            assert_eq!(
                serde_json::to_value(&preview.cells[offset..offset + 17 * 17]).unwrap(),
                serde_json::to_value(&root.vertices).unwrap()
            );
            for y in 0..2 {
                for x in 0..2 {
                    let child = world
                        .tile(
                            TileKey {
                                face,
                                level: 1,
                                x,
                                y,
                            },
                            16,
                        )
                        .unwrap();
                    for cy in (0..=16).step_by(2) {
                        for cx in (0..=16).step_by(2) {
                            let parent_index =
                                (y as usize * 8 + cy / 2) * 17 + x as usize * 8 + cx / 2;
                            assert_eq!(
                                serde_json::to_value(&preview.cells[offset + parent_index])
                                    .unwrap(),
                                serde_json::to_value(&child.vertices[cy * 17 + cx]).unwrap()
                            );
                        }
                    }
                }
            }
        }
    }
}
