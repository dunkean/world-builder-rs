//! Canonical regional erosion patches, independent of display tile resolution.

use crate::{geometry, Biome, Sample, Tile, TileKey, World};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegionPatch {
    pub key: TileKey,
    /// Number of intervals across the interior, not including the halo.
    pub segments: u32,
    pub halo_cells: u32,
    /// Row-major heights; side = segments + 1 + 2 * halo_cells.
    pub heights_m: Vec<f32>,
    /// Additive erosion delta keeps analytic detail below the raster spacing.
    /// Empty only for legacy absolute-height patches.
    #[serde(default)]
    pub residual_m: Vec<f32>,
    pub source_world_id: String,
    pub algorithm_version: String,
}

impl RegionPatch {
    pub fn validate(&self, world: &World) -> Result<()> {
        ensure!(
            self.source_world_id == world.id,
            "region belongs to another physical world"
        );
        ensure!(
            !self.algorithm_version.is_empty(),
            "region algorithm version is missing"
        );
        ensure!(
            self.key.face < 6 && self.key.level <= 24,
            "invalid region key"
        );
        let count = 1_u32 << self.key.level;
        ensure!(
            self.key.x < count && self.key.y < count,
            "invalid region coordinates"
        );
        ensure!(
            (4..=4096).contains(&self.segments) && self.halo_cells <= 256,
            "invalid region grid dimensions"
        );
        let side = (self.segments + 1 + 2 * self.halo_cells) as usize;
        ensure!(
            self.heights_m.len() == side * side,
            "region height count differs from declared halo grid"
        );
        ensure!(
            self.heights_m.iter().all(|h| h.is_finite()),
            "region contains nonfinite heights"
        );
        ensure!(
            self.residual_m.is_empty() || self.residual_m.len() == side * side,
            "region residual count differs from declared halo grid"
        );
        ensure!(
            self.residual_m.iter().all(|h| h.is_finite()),
            "region contains nonfinite residuals"
        );
        Ok(())
    }

    /// Position of a grid vertex, including exterior halo vertices.
    pub fn direction_at(&self, x: u32, y: u32) -> [f64; 3] {
        let count = f64::from(1_u32 << self.key.level);
        let local_x = (f64::from(x) - f64::from(self.halo_cells)) / f64::from(self.segments);
        let local_y = (f64::from(y) - f64::from(self.halo_cells)) / f64::from(self.segments);
        geometry::direction(
            self.key.face,
            2.0 * (f64::from(self.key.x) + local_x) / count - 1.0,
            2.0 * (f64::from(self.key.y) + local_y) / count - 1.0,
        )
    }

    fn height_and_weight(&self, p: [f64; 3]) -> Option<(f64, f64)> {
        let [normal, u_axis, v_axis] = geometry::FACE_BASES[usize::from(self.key.face)];
        let denominator = geometry::dot(p, normal);
        if denominator <= 0.0 {
            return None;
        }
        let count = f64::from(1_u32 << self.key.level);
        let x =
            (geometry::dot(p, u_axis) / denominator + 1.0) * 0.5 * count - f64::from(self.key.x);
        let y =
            (geometry::dot(p, v_axis) / denominator + 1.0) * 0.5 * count - f64::from(self.key.y);
        // Exact border belongs to the shared analytic parent, avoiding any
        // dependency on the neighbor's independently computed erosion patch.
        if x <= 0.0 || y <= 0.0 || x >= 1.0 || y >= 1.0 {
            return None;
        }
        let side = (self.segments + 1 + 2 * self.halo_cells) as usize;
        let fx = x * f64::from(self.segments) + f64::from(self.halo_cells);
        let fy = y * f64::from(self.segments) + f64::from(self.halo_cells);
        let ix = fx.floor() as usize;
        let iy = fy.floor() as usize;
        let tx = fx - ix as f64;
        let ty = fy - iy as f64;
        let values = if self.residual_m.is_empty() {
            &self.heights_m
        } else {
            &self.residual_m
        };
        let read = |dx: usize, dy: usize| f64::from(values[(iy + dy) * side + ix + dx]);
        let height = read(0, 0) * (1.0 - tx) * (1.0 - ty)
            + read(1, 0) * tx * (1.0 - ty)
            + read(0, 1) * (1.0 - tx) * ty
            + read(1, 1) * tx * ty;
        let collar = f64::from((self.segments / 4).clamp(1, 16));
        let fade = |t: f64| {
            let q = (t * f64::from(self.segments) / collar).clamp(0.0, 1.0);
            q * q * (3.0 - 2.0 * q)
        };
        let weight = fade(x) * fade(1.0 - x) * fade(y) * fade(1.0 - y);
        Some((height, weight))
    }
}

impl World {
    /// Patches must be validated before publication. Smaller-scale regions
    /// compose over their parents in spatial order, never request order.
    pub fn sample_with_patches(&self, p: [f64; 3], patches: &[RegionPatch]) -> Sample {
        let mut sample = self.sample(p);
        let mut ordered: Vec<_> = patches
            .iter()
            .filter(|p| p.source_world_id == self.id)
            .collect();
        ordered.sort_by_key(|p| (p.key.level, p.key.face, p.key.x, p.key.y));
        apply(&mut sample, &ordered);
        sample
    }

    pub fn tile_with_patches(
        &self,
        key: TileKey,
        segments: u32,
        patches: &[RegionPatch],
    ) -> Result<Tile> {
        for (i, patch) in patches.iter().enumerate() {
            patch.validate(self)?;
            ensure!(
                !patches[..i].iter().any(|old| old.key == patch.key),
                "duplicate canonical region key"
            );
        }
        let mut ordered: Vec<_> = patches.iter().collect();
        ordered.sort_by_key(|p| (p.key.level, p.key.face, p.key.x, p.key.y));
        let mut tile = self.tile(key, segments)?;
        for sample in &mut tile.vertices {
            apply(sample, &ordered);
        }
        Ok(tile)
    }
}

fn apply(sample: &mut Sample, patches: &[&RegionPatch]) {
    // A narrow hydraulic feature can pass between four dry raster nodes.
    // Reapply its continuous constraint when publishing the interpolated delta.
    // Solver budgets describe the unmasked field, not this protected surface.
    let hydraulic_weight = publication_weight(sample);
    let mut height = f64::from(sample.height_m);
    for patch in patches {
        if let Some((target, weight)) = patch.height_and_weight(sample.direction) {
            let weight = weight * hydraulic_weight;
            if patch.residual_m.is_empty() {
                height += weight * (target - height);
            } else {
                height += weight * target;
            }
        }
    }
    sample.height_m = height as f32;
    let level = sample.water_level_m.unwrap_or(sample.height_m);
    let wet = level > sample.height_m
        && (sample.water_body_id.is_some_and(|id| id < 1_u64 << 52) || sample.river > 0.0);
    sample.ocean = wet && sample.water_body_id == Some(0);
    sample.lake = wet
        && sample
            .water_body_id
            .is_some_and(|id| id > 0 && id < 1_u64 << 52);
    sample.water_m = if wet { level } else { sample.height_m };
    if sample.ocean || sample.lake {
        sample.biome = if sample.ocean {
            Biome::Ocean
        } else {
            Biome::Lake
        };
        sample.biome_mix = [0.0; 10];
        sample.biome_mix[sample.biome as usize] = 1.0;
    } else if matches!(sample.biome, Biome::Ocean | Biome::Lake) {
        sample.biome = Biome::Rock;
        sample.biome_mix = [0.0; 10];
        sample.biome_mix[Biome::Rock as usize] = 1.0;
    }
}

fn publication_weight(sample: &Sample) -> f64 {
    if sample.ocean || sample.lake {
        return 0.0;
    }
    let mut weight = (1.0 - f64::from(sample.river).clamp(0.0, 1.0)).powi(6);
    if let Some(level) = sample.water_level_m {
        let distance = (f64::from(sample.height_m) - f64::from(level)).abs();
        let t = ((distance - 8.0) / 8.0).clamp(0.0, 1.0);
        weight *= t * t * (3.0 - 2.0 * t);
    }
    weight
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorldConfig;

    fn patch(world: &World, key: TileKey, raise: f32) -> RegionPatch {
        let mut patch = RegionPatch {
            key,
            segments: 16,
            halo_cells: 2,
            heights_m: Vec::new(),
            residual_m: Vec::new(),
            source_world_id: world.id.clone(),
            algorithm_version: "test-region-v1".into(),
        };
        let side = patch.segments + 1 + 2 * patch.halo_cells;
        for y in 0..side {
            for x in 0..side {
                patch
                    .heights_m
                    .push(world.sample(patch.direction_at(x, y)).height_m + raise);
            }
        }
        patch
    }

    #[test]
    fn region_boundary_and_shared_display_samples_remain_canonical() {
        let world = World::generate(WorldConfig {
            resolution: 8,
            ..Default::default()
        })
        .unwrap();
        let region = patch(
            &world,
            TileKey {
                face: 4,
                level: 2,
                x: 1,
                y: 1,
            },
            10.0,
        );
        region.validate(&world).unwrap();
        for y in 0..=16 {
            let p = region.direction_at(2, 2 + y);
            assert_eq!(
                world.sample(p).height_m.to_bits(),
                world
                    .sample_with_patches(p, std::slice::from_ref(&region))
                    .height_m
                    .to_bits()
            );
        }
        let a = world
            .tile_with_patches(
                TileKey {
                    face: 4,
                    level: 3,
                    x: 2,
                    y: 2,
                },
                8,
                std::slice::from_ref(&region),
            )
            .unwrap();
        let b = world
            .tile_with_patches(
                TileKey {
                    face: 4,
                    level: 3,
                    x: 3,
                    y: 2,
                },
                8,
                std::slice::from_ref(&region),
            )
            .unwrap();
        for y in 0..=8 {
            assert_eq!(
                a.vertices[y * 9 + 8].height_m.to_bits(),
                b.vertices[y * 9].height_m.to_bits()
            );
        }
    }

    #[test]
    fn region_order_does_not_change_nested_refinement() {
        let world = World::generate(WorldConfig {
            resolution: 8,
            ..Default::default()
        })
        .unwrap();
        let parent = patch(
            &world,
            TileKey {
                face: 4,
                level: 1,
                x: 0,
                y: 0,
            },
            10.0,
        );
        let child = patch(
            &world,
            TileKey {
                face: 4,
                level: 2,
                x: 0,
                y: 0,
            },
            25.0,
        );
        let p = child.direction_at(10, 10);
        let a = world.sample_with_patches(p, &[parent.clone(), child.clone()]);
        let b = world.sample_with_patches(p, &[child, parent]);
        assert_eq!(a.height_m.to_bits(), b.height_m.to_bits());
    }

    #[test]
    fn erosion_residual_preserves_analytic_variation_below_raster_spacing() {
        let mut world = World::generate(WorldConfig {
            resolution: 8,
            ..Default::default()
        })
        .unwrap();
        // Isolate analytic detail from hydraulic constraints in this test.
        for cell in &mut world.cells {
            cell.height_m = 500.0;
            cell.water_m = 500.0;
            cell.ocean = false;
            cell.lake = false;
            cell.receiver = None;
            cell.discharge = 0.0;
        }
        let mut region = patch(
            &world,
            TileKey {
                face: 4,
                level: 2,
                x: 1,
                y: 1,
            },
            1000.0,
        );
        region.residual_m = vec![-12.5; region.heights_m.len()];
        region.validate(&world).unwrap();
        let mut initial = Vec::new();
        let mut eroded = Vec::new();
        // All samples lie within one raster cell, outside the frozen collar.
        for offset in [0.001, 0.009, 0.017, 0.025] {
            let p = geometry::direction(4, -0.25 + offset, -0.25 + offset * 0.3);
            let base = world.sample(p).height_m;
            let refined = world
                .sample_with_patches(p, std::slice::from_ref(&region))
                .height_m;
            assert!((f64::from(refined) - f64::from(base) + 12.5).abs() < 0.001);
            initial.push(base);
            eroded.push(refined);
        }
        assert!(initial.windows(2).any(|pair| pair[0] != pair[1]));
        for i in 1..initial.len() {
            let before = f64::from(initial[i]) - f64::from(initial[0]);
            let after = f64::from(eroded[i]) - f64::from(eroded[0]);
            assert!((after - before).abs() < 0.001);
        }
        region.residual_m.pop();
        assert!(region.validate(&world).is_err());
    }

    #[test]
    fn interpolated_erosion_preserves_subgrid_river_and_lake_profiles() {
        let mut world = World::generate(WorldConfig {
            resolution: 8,
            erosion_steps: 0,
            detail_m: 0.0,
            ..Default::default()
        })
        .unwrap();
        for cell in &mut world.cells {
            cell.height_m = 160.0;
            cell.water_m = 160.0;
            cell.ocean = false;
            cell.lake = false;
            cell.lake_id = None;
            cell.receiver = None;
            cell.discharge = 0.0;
        }
        let lake = geometry::cell_index(4, 2, 2, 8);
        world.cells[lake].height_m = 60.0;
        world.cells[lake].water_m = 100.0;
        world.cells[lake].lake = true;
        world.cells[lake].lake_id = Some(17);
        let river = geometry::cell_index(4, 3, 3, 8);
        let downstream = geometry::cell_index(4, 4, 3, 8);
        world.cells[river].height_m = 80.0;
        world.cells[river].water_m = 80.0;
        world.cells[river].receiver = Some(downstream);
        world.cells[river].discharge = 1000.0;
        world.cells[downstream].height_m = 60.0;
        let mut region = patch(
            &world,
            TileKey {
                face: 4,
                level: 2,
                x: 1,
                y: 1,
            },
            0.0,
        );
        // Represents an interpolated correction from surrounding dry nodes.
        // This stream is much narrower than the coarse region raster spacing.
        region.residual_m = vec![50.0; region.heights_m.len()];
        for index in [river, lake] {
            let p = world.cells[index].position;
            let before = world.sample(p);
            assert!(before.river > 0.99 || before.lake);
            let after = world.sample_with_patches(p, std::slice::from_ref(&region));
            assert_eq!(after.height_m.to_bits(), before.height_m.to_bits());
            assert_eq!(after.water_m.to_bits(), before.water_m.to_bits());
            assert_eq!(after.water_body_id, before.water_body_id);
            assert_eq!(after.biome_mix, before.biome_mix);
        }
    }
}
