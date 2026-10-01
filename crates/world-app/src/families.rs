//! Experimental geometry-only family compiler, exposed separately from the
//! continuous atlas tiles until drainage/coast contracts are implemented.

use anyhow::{ensure, Result};
use serde_json::{json, Value};
use world_contracts::{refine, Grid, ALGORITHM_VERSION};
use world_core::{Biome, TileKey, World};

pub fn compile_family(world: &World, key: TileKey, segments: u32) -> Result<Value> {
    ensure!(
        key.level <= 23,
        "family parent level must be <=23 (children <=24)"
    );
    let parent_tile = world.tile(key, segments)?;
    let side = segments as usize + 1;
    let count = 1_u32 << key.level;
    let denominator = f64::from(count) * f64::from(segments);
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
            // Normalized cube projection: dA = R² du dv / (1+u²+v²)^(3/2).
            // Positive midpoint quadrature avoids tiny-triangle cancellation.
            weights
                .push(world.config.radius_m.powi(2) * du.powi(2) / (1.0 + u * u + v * v).powf(1.5));
        }
    }
    let parent = Grid {
        size: side,
        heights_m: parent_tile
            .vertices
            .iter()
            .map(|v| f64::from(v.height_m))
            .collect(),
        weights,
    };
    let mut seed_hash = blake3::Hasher::new();
    seed_hash.update(world.id.as_bytes());
    seed_hash.update(ALGORITHM_VERSION.as_bytes());
    seed_hash.update(&[key.face, key.level]);
    seed_hash.update(&key.x.to_le_bytes());
    seed_hash.update(&key.y.to_le_bytes());
    seed_hash.update(&segments.to_le_bytes());
    let digest = seed_hash.finalize();
    let seed = u64::from_le_bytes(digest.as_bytes()[..8].try_into()?);
    // Detail amplitude decays with dyadic scale. It is a geometric proposal,
    // not another iteration of geological erosion at each zoom level.
    let amplitude_m = world.config.detail_m * 2.0_f64.powf(-0.7 * f64::from(key.level + 1));
    let refined = refine(&parent, seed, amplitude_m)?;
    let mut tiles = Vec::with_capacity(4);
    let mut canonical_heights_cm = Vec::with_capacity(4);
    let mut render_max_height_error_m = 0.0_f64;
    for (index, child) in refined.children.iter().enumerate() {
        let child_key = TileKey {
            face: key.face,
            level: key.level + 1,
            x: key.x * 2 + (index % 2) as u32,
            y: key.y * 2 + (index / 2) as u32,
        };
        let mut tile = world.tile(child_key, segments)?;
        for (vertex, &canonical_height) in tile.vertices.iter_mut().zip(&child.heights_m) {
            let old_water = vertex.water_m;
            vertex.height_m = canonical_height as f32;
            render_max_height_error_m = render_max_height_error_m
                .max((f64::from(vertex.height_m) - canonical_height).abs());

            // Reuse atlas water labels; there is no new local ocean connectivity
            // or river routing solve. Prevent visibly inverted water surfaces.
            vertex.ocean = vertex.ocean && vertex.height_m <= 0.0;
            vertex.lake = !vertex.ocean && vertex.lake && old_water > vertex.height_m + 0.01;
            if vertex.ocean {
                vertex.water_m = 0.0;
                vertex.river = 0.0;
                vertex.biome = Biome::Ocean;
            } else if vertex.lake {
                vertex.water_m = old_water.max(vertex.height_m);
                vertex.river = 0.0;
                vertex.biome = Biome::Lake;
            } else {
                vertex.water_m = vertex.height_m + vertex.river * 1.5;
                if matches!(vertex.biome, Biome::Ocean | Biome::Lake) {
                    vertex.biome = Biome::Rock;
                }
            }
            vertex.water_m = vertex.water_m.max(vertex.height_m);
        }
        canonical_heights_cm.push(
            child
                .heights_m
                .iter()
                .map(|h| (h * 100.0).round() as i32)
                .collect::<Vec<_>>(),
        );
        tiles.push(tile);
    }
    Ok(json!({
        "world_id": world.id,
        "parent_key": key,
        "segments": segments,
        "children": tiles,
        "canonical_heights_cm": canonical_heights_cm,
        "certificate": refined.certificate,
        "render_max_height_error_m": render_max_height_error_m,
        "scope": {
            "status": "experimental geometry-only family",
            "metric": "cubed-sphere midpoint cell areas; equal quarter-area refinement",
            "height_contract": "per-parent-cell weighted means, inherited anchors and exact canonical boundaries",
            "height_encoding": "signed i32 centimetres; f32 render samples supplied separately",
            "child_order": ["NW", "NE", "SW", "SE"],
            "amplitude_m": amplitude_m,
            "drainage_constrained": false,
            "coast_constrained": false,
            "water": "atlas labels with water surfaces above refined ground; no local flow certification"
        }
    }))
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

    #[test]
    fn family_children_keep_canonical_boundaries_and_valid_water_surfaces() {
        let world = world();
        let result = compile_family(
            &world,
            TileKey {
                face: 0,
                level: 1,
                x: 0,
                y: 0,
            },
            8,
        )
        .unwrap();
        assert_eq!(result["certificate"]["passed"], true);
        assert_eq!(result["certificate"]["drainage_constrained"], false);
        let children: Vec<Tile> = serde_json::from_value(result["children"].clone()).unwrap();
        assert_eq!(children.len(), 4);
        for child in &children {
            assert_eq!(child.key.level, 2);
            assert_eq!(child.vertices.len(), 81);
            for vertex in &child.vertices {
                assert!(vertex.water_m >= vertex.height_m);
            }
        }
        for y in 0..=8 {
            assert_eq!(
                children[0].vertices[y * 9 + 8].height_m.to_bits(),
                children[1].vertices[y * 9].height_m.to_bits()
            );
            assert_eq!(
                children[0].vertices[y * 9 + 8].direction,
                children[1].vertices[y * 9].direction
            );
        }
        let repeated = compile_family(
            &world,
            TileKey {
                face: 0,
                level: 1,
                x: 0,
                y: 0,
            },
            8,
        )
        .unwrap();
        assert_eq!(
            result["certificate"]["digest"],
            repeated["certificate"]["digest"]
        );
    }

    #[test]
    fn independently_compiled_neighbor_families_keep_shared_edge() {
        let world = world();
        let left = compile_family(
            &world,
            TileKey {
                face: 4,
                level: 2,
                x: 1,
                y: 1,
            },
            8,
        )
        .unwrap();
        let right = compile_family(
            &world,
            TileKey {
                face: 4,
                level: 2,
                x: 2,
                y: 1,
            },
            8,
        )
        .unwrap();
        let a: Vec<Tile> = serde_json::from_value(left["children"].clone()).unwrap();
        let b: Vec<Tile> = serde_json::from_value(right["children"].clone()).unwrap();
        for (left_index, right_index) in [(1, 0), (3, 2)] {
            for y in 0..=8 {
                let x = &a[left_index].vertices[y * 9 + 8];
                let z = &b[right_index].vertices[y * 9];
                assert_eq!(x.height_m.to_bits(), z.height_m.to_bits());
                assert_eq!(x.direction, z.direction);
            }
        }
    }

    #[test]
    fn terminal_level_cannot_compile_children() {
        let world = world();
        assert!(compile_family(
            &world,
            TileKey {
                face: 0,
                level: 24,
                x: 0,
                y: 0
            },
            8
        )
        .is_err());
    }
}
