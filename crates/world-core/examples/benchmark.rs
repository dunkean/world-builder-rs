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
        }))?
    );
    Ok(())
}
