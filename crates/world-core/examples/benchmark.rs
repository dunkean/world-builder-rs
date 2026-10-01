//! CPU measurements only; this does not measure renderer or GPU performance.
use anyhow::{Context, Result};
use std::{hint::black_box, time::Instant};
use world_core::{geometry::normalize, TileKey, World, WorldConfig};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let resolution = args
        .first()
        .map(|s| s.parse::<u32>())
        .transpose()
        .context(
            "usage: cargo run --release -p world-core --example benchmark -- [resolution] [seed]",
        )?
        .unwrap_or(128);
    let seed = args
        .get(1)
        .map(|s| s.parse::<u64>())
        .transpose()
        .context("seed must be a u64")?
        .unwrap_or(42);
    let started = Instant::now();
    let world = World::generate(WorldConfig {
        resolution,
        seed,
        ..Default::default()
    })?;
    let atlas_ms = started.elapsed().as_secs_f64() * 1000.0;

    let started = Instant::now();
    for i in 0..10_000 {
        let y = 1.0 - 2.0 * (f64::from(i) + 0.5) / 10_000.0;
        let angle = f64::from(i) * 2.399963229728653;
        let radius = (1.0 - y * y).sqrt();
        black_box(world.sample(normalize([radius * angle.cos(), y, radius * angle.sin()])));
    }
    let sampling_ms = started.elapsed().as_secs_f64() * 1000.0;

    let batch_started = Instant::now();
    let mut tile_times = Vec::with_capacity(100);
    for i in 0..100u32 {
        if batch_started.elapsed().as_secs_f64() >= 10.0 {
            break;
        }
        let key = TileKey {
            face: (i % 6) as u8,
            level: 10,
            x: i.wrapping_mul(137) % 1024,
            y: i.wrapping_mul(419) % 1024,
        };
        let started = Instant::now();
        black_box(world.tile(key, 64)?);
        tile_times.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    tile_times.sort_by(f64::total_cmp);
    let percentile =
        |fraction: f64| tile_times[((tile_times.len() - 1) as f64 * fraction).round() as usize];
    let peak = world
        .cells
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.ocean && !c.lake)
        .max_by(|(_, a), (_, b)| a.height_m.total_cmp(&b.height_m))
        .map(|(i, _)| i);
    let lake = world
        .lakes
        .iter()
        .max_by(|a, b| a.area_m2.total_cmp(&b.area_m2))
        .and_then(|l| {
            l.cells
                .iter()
                .copied()
                .filter(|&i| world.cells[i].lake)
                .max_by(|&a, &b| {
                    (world.cells[a].water_m - world.cells[a].height_m)
                        .total_cmp(&(world.cells[b].water_m - world.cells[b].height_m))
                })
        });
    let river = world
        .rivers()
        .into_iter()
        .filter(|r| r.start_height_m > 100.0)
        .max_by(|a, b| a.discharge_m3_s.total_cmp(&b.discharge_m3_s))
        .map(|r| r.from);
    let coast = world
        .cells
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            !c.ocean
                && c.temperature_c > 4.0
                && world.sample(c.position).height_m > 5.0
                && !c.lake
                && world_core::geometry::neighbours(*i, resolution)
                    .into_iter()
                    .any(|j| world.cells[j].ocean)
        })
        .min_by(|(_, a), (_, b)| a.height_m.abs().total_cmp(&b.height_m.abs()))
        .map(|(i, _)| i);
    let point = |index: Option<usize>| {
        index.map(|i| {let sample=world.sample(world.cells[i].position);serde_json::json!({"index":i,"direction":world.cells[i].position,"height_m":world.cells[i].height_m,"water_m":world.cells[i].water_m,"discharge_m3_s":world.cells[i].discharge,"sample_height_m":sample.height_m,"sample_water_m":sample.water_m,"sample_river":sample.river,"water_body_id":sample.water_body_id})})
    };
    let mut basin_histogram = [0usize; 4];
    let mut wet_histogram = [0usize; 4];
    let mut depths = Vec::new();
    for l in &world.lakes {
        let wet = l.cells.iter().filter(|&&i| world.cells[i].lake).count();
        basin_histogram[l.cells.len().clamp(1, 4) - 1] += 1;
        wet_histogram[wet.clamp(1, 4) - 1] += 1;
        depths.push(
            l.cells
                .iter()
                .map(|&i| (l.level_m - world.cells[i].height_m).max(0.0))
                .fold(0.0, f32::max),
        );
    }
    depths.sort_by(f32::total_cmp);
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "backend": "Rust CPU / Rayon atlas, serial spatial sampling",
            "resolution": resolution, "seed": seed,
            "atlas_ms": atlas_ms, "cell_count": world.cells.len(),
            "sample_count": 10_000, "sampling_ms": sampling_ms,
            "tile_count": tile_times.len(), "tile_segments": 64,
            "tile_p50_ms": percentile(0.50), "tile_p95_ms": percentile(0.95),
            "stats": world.stats
            ,"world_id":world.id,"erosion":world.erosion,
            "lake_distribution":{"basin_cells_1_2_3_4plus":basin_histogram,"wet_cells_1_2_3_4plus":wet_histogram,"median_depth_m":depths.get(depths.len()/2),"max_depth_m":depths.last()},
            "qa_points":{"mountain":point(peak),"coast":point(coast),"lake":point(lake),"river":point(river)}
        }))?
    );
    Ok(())
}
