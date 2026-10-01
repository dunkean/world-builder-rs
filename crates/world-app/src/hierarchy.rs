//! Hierarchical geometry generator. A tile is derived from its previously
//! refined ancestors; the raw atlas is sampled for relief only at the root.

use anyhow::{ensure, Result};
use serde_json::{json, Value};
use world_contracts::{refine, Certificate, Grid, ALGORITHM_VERSION};
use world_core::{Biome, TileKey, World};

pub fn compile_tile(world: &World, key: TileKey, segments: u32) -> Result<Value> {
    ensure!(
        key.face < 6 && key.level <= 18,
        "contract tiles require face 0..5 and level <=18"
    );
    ensure!(
        (2..=128).contains(&segments),
        "contract tiles require 2..=128 segments"
    );
    ensure!(
        key.x < 1_u32 << key.level && key.y < 1_u32 << key.level,
        "invalid tile coordinates"
    );
    let root_key = TileKey {
        face: key.face,
        level: 0,
        x: 0,
        y: 0,
    };
    let root = world.tile(root_key, segments)?;
    let mut current = Grid {
        size: segments as usize + 1,
        heights_m: root
            .vertices
            .iter()
            .map(|v| (f64::from(v.height_m) * 100.0).round() * 0.01)
            .collect(),
        weights: metric_weights(world, root_key, segments),
    };
    ensure!(
        current.heights_m.iter().all(|h| h.is_finite()
            && *h * 100.0 >= i32::MIN as f64
            && *h * 100.0 <= i32::MAX as f64),
        "root heights exceed canonical signed 32-bit centimetre storage"
    );
    let mut current_key = root_key;
    let mut certificates: Vec<Certificate> = Vec::with_capacity(key.level as usize);
    let mut ancestors = Vec::with_capacity(key.level as usize);
    for level in 1..=key.level {
        let shift = key.level - level;
        let next = TileKey {
            face: key.face,
            level,
            x: key.x >> shift,
            y: key.y >> shift,
        };
        let amplitude = world.config.detail_m * 2.0_f64.powf(-0.7 * f64::from(level));
        let family = refine(
            &current,
            family_seed(world, current_key, segments),
            amplitude,
        )?;
        let child = (next.y % 2 * 2 + next.x % 2) as usize;
        current = family.children[child].clone();
        // The metric is recomputed at the new parent scale. The local reference
        // solver still uses equal quarters inside each parent cell, as stated
        // in the certificate scope; this is not exact spherical integration.
        current.weights = metric_weights(world, next, segments);
        ancestors.push(current_key);
        certificates.push(family.certificate);
        current_key = next;
    }

    let mut tile = world.tile(key, segments)?;
    let mut render_error = 0.0_f64;
    for (sample, &height) in tile.vertices.iter_mut().zip(&current.heights_m) {
        let old_water = sample.water_m;
        sample.height_m = height as f32;
        render_error = render_error.max((height - f64::from(sample.height_m)).abs());
        sample.ocean = sample.ocean && sample.height_m <= 0.0;
        sample.lake = !sample.ocean && sample.lake && old_water > sample.height_m + 0.01;
        if sample.ocean {
            sample.water_m = 0.0;
            sample.biome = Biome::Ocean;
            sample.river = 0.0;
        } else if sample.lake {
            sample.water_m = old_water.max(sample.height_m);
            sample.biome = Biome::Lake;
            sample.river = 0.0;
        } else {
            sample.water_m = sample.height_m + sample.river * 1.5;
            if matches!(sample.biome, Biome::Ocean | Biome::Lake) {
                sample.biome = Biome::Rock;
            }
        }
        sample.water_m = sample.water_m.max(sample.height_m);
    }
    let canonical_heights_cm: Vec<i32> = current
        .heights_m
        .iter()
        .map(|h| (h * 100.0).round() as i32)
        .collect();
    Ok(json!({
        "key": tile.key,
        "segments": tile.segments,
        "vertices": tile.vertices,
        "indices": tile.indices,
        "canonical_heights_cm": canonical_heights_cm,
        "hierarchy_certificate": {
            "all_passed": certificates.iter().all(|c| c.passed),
            "levels": key.level,
            "certificates": certificates,
            "ancestor_keys": ancestors,
            "algorithm_version": ALGORITHM_VERSION,
            "quantization_m": 0.01,
            "render_max_height_error_m": render_error,
            "drainage_constrained": false,
            "coast_constrained": false,
            "scope": "experimental geometry: inherited ancestors, parent-cell means and exact boundaries; midpoint spherical metric with equal-quarter reduction; atlas water labels without local flow certification"
        }
    }))
}

fn metric_weights(world: &World, key: TileKey, segments: u32) -> Vec<f64> {
    let denominator = f64::from(1_u32 << key.level) * f64::from(segments);
    let du = 2.0 / denominator;
    let mut weights = Vec::with_capacity(segments as usize * segments as usize);
    for y in 0..segments {
        for x in 0..segments {
            let u = 2.0 * (f64::from(key.x) * f64::from(segments) + f64::from(x) + 0.5)
                / denominator
                - 1.0;
            let v = 2.0 * (f64::from(key.y) * f64::from(segments) + f64::from(y) + 0.5)
                / denominator
                - 1.0;
            weights
                .push(world.config.radius_m.powi(2) * du.powi(2) / (1.0 + u * u + v * v).powf(1.5));
        }
    }
    weights
}

fn family_seed(world: &World, key: TileKey, segments: u32) -> u64 {
    let mut hash = blake3::Hasher::new();
    hash.update(world.id.as_bytes());
    hash.update(ALGORITHM_VERSION.as_bytes());
    hash.update(&[key.face, key.level]);
    hash.update(&key.x.to_le_bytes());
    hash.update(&key.y.to_le_bytes());
    hash.update(&segments.to_le_bytes());
    let result = hash.finalize();
    u64::from_le_bytes(result.as_bytes()[..8].try_into().expect("eight hash bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use world_core::{Tile, WorldConfig};

    fn world() -> World {
        World::generate(WorldConfig {
            resolution: 8,
            ..Default::default()
        })
        .unwrap()
    }

    fn decode(world: &World, key: TileKey) -> (Value, Tile) {
        let value = compile_tile(world, key, 8).unwrap();
        let tile: Tile = serde_json::from_value(value.clone()).unwrap();
        (value, tile)
    }

    #[test]
    fn adjacent_tiles_from_different_parents_share_identical_boundaries() {
        let world = world();
        let ka = TileKey {
            face: 4,
            level: 3,
            x: 1,
            y: 1,
        };
        let kb = TileKey {
            face: 4,
            level: 3,
            x: 2,
            y: 1,
        };
        let (first, a) = decode(&world, ka);
        let (_, b) = decode(&world, kb);
        for y in 0..=8 {
            assert_eq!(
                a.vertices[y * 9 + 8].height_m.to_bits(),
                b.vertices[y * 9].height_m.to_bits()
            );
            assert_eq!(a.vertices[y * 9 + 8].direction, b.vertices[y * 9].direction);
        }
        assert_eq!(first, compile_tile(&world, ka, 8).unwrap());
        assert_eq!(first["hierarchy_certificate"]["all_passed"], true);
        assert_eq!(first["hierarchy_certificate"]["levels"], 3);
    }

    #[test]
    fn second_level_reduces_to_refined_parent_instead_of_raw_atlas() {
        let world = world();
        let parent_key = TileKey {
            face: 0,
            level: 1,
            x: 0,
            y: 0,
        };
        let (_, parent) = decode(&world, parent_key);
        let raw = world.tile(parent_key, 8).unwrap();
        let difference = raw
            .vertices
            .iter()
            .zip(&parent.vertices)
            .map(|(a, b)| (a.height_m - b.height_m).abs())
            .fold(0.0_f32, f32::max);
        assert!(difference > 0.1);
        let mut fine = vec![f64::NAN; 17 * 17];
        for child in 0..4 {
            let key = TileKey {
                face: 0,
                level: 2,
                x: child % 2,
                y: child / 2,
            };
            let (_, tile) = decode(&world, key);
            let ox = (child % 2) as usize * 8;
            let oy = (child / 2) as usize * 8;
            for y in 0..=8 {
                for x in 0..=8 {
                    fine[(oy + y) * 17 + ox + x] = f64::from(tile.vertices[y * 9 + x].height_m);
                }
            }
        }
        for y in 0..8 {
            for x in 0..8 {
                let px = 2 * x;
                let py = 2 * y;
                let read = |dx, dy| fine[(py + dy) * 17 + px + dx];
                let reduced = (read(0, 0)
                    + read(2, 0)
                    + read(0, 2)
                    + read(2, 2)
                    + 2.0 * (read(1, 0) + read(1, 2) + read(0, 1) + read(2, 1))
                    + 4.0 * read(1, 1))
                    / 16.0;
                let expected = f64::from(
                    parent.vertices[y * 9 + x].height_m
                        + parent.vertices[y * 9 + x + 1].height_m
                        + parent.vertices[(y + 1) * 9 + x].height_m
                        + parent.vertices[(y + 1) * 9 + x + 1].height_m,
                ) * 0.25;
                assert!((reduced - expected).abs() <= 0.01);
            }
        }
    }

    #[test]
    fn request_limits_are_explicit() {
        let world = world();
        assert!(compile_tile(
            &world,
            TileKey {
                face: 0,
                level: 19,
                x: 0,
                y: 0
            },
            8
        )
        .is_err());
        assert!(compile_tile(
            &world,
            TileKey {
                face: 0,
                level: 0,
                x: 0,
                y: 0
            },
            129
        )
        .is_err());
    }
}
