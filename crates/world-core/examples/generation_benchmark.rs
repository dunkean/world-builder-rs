//! Atlas-only benchmark: no sampling or river-catalogue warmup.
use anyhow::{Context, Result};
use std::time::Instant;
use world_core::{World, WorldConfig};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut config = WorldConfig::default();
    if let Some(value) = args.first() {
        config.resolution = value.parse().context("resolution must be an integer")?;
    }
    config.seed = args
        .get(1)
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(2386084681);
    if let Some(value) = args.get(2) {
        config.erosion_steps = value.parse().context("steps must be an integer")?;
    }
    if let Some(value) = args.get(3) {
        config.geological_years = value.parse().context("years must be a number")?;
    }
    std::env::set_var("WB_PROFILE_GENERATION", "1");
    let start = Instant::now();
    let world = World::generate(config)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "elapsed_ms": start.elapsed().as_secs_f64() * 1000.0,
            "rayon_threads": rayon::current_num_threads(),
            "config": world.config,
            "stats": world.stats,
            "erosion": world.erosion,
            "world_id": world.id,
        }))?
    );
    Ok(())
}
