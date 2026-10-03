//! Local lake-district measurements and CSVs; no renderer or water-budget claim.
use anyhow::{Context, Result};
use serde_json::json;
use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, BufWriter, Write},
    time::Instant,
};
use world_core::{
    geometry::{cross, normalize},
    landforms, Biome, Sample, World, WorldConfig,
};

fn point(center: [f64; 3], radius: f64, x: f64, y: f64) -> [f64; 3] {
    let axis = if center[1].abs() < 0.9 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let east = normalize(cross(axis, center));
    let north = cross(center, east);
    normalize(std::array::from_fn(|i| {
        center[i] + (east[i] * x + north[i] * y) / radius
    }))
}

fn micro(sample: &Sample) -> bool {
    sample.lake
        && sample
            .water_body_id
            .is_some_and(|id| ((1u64 << 40)..(1u64 << 41)).contains(&id))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let requested_center = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--center="))
        .map(|value| -> Result<[f64; 3]> {
            let coordinates = value
                .split(',')
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            coordinates.try_into().map_err(|_| {
                anyhow::anyhow!("--center requires three comma-separated direction coordinates")
            })
        })
        .transpose()?;
    let province_limit = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--provinces="))
        .map(str::parse::<usize>)
        .transpose()?
        .unwrap_or(32);
    let prepare_source = if args.first().is_some_and(|s| s.ends_with(".json")) {
        "load_saved_atlas"
    } else {
        "generate_atlas"
    };
    let prepare = Instant::now();
    let world: World = if let Some(path) = args.first().filter(|s| s.ends_with(".json")) {
        serde_json::from_reader(BufReader::new(File::open(path)?)).context("load world")?
    } else {
        World::generate(WorldConfig {
            seed: 2_386_084_681,
            resolution: args.first().map(|s| s.parse()).transpose()?.unwrap_or(128),
            erosion_steps: 25,
            ..Default::default()
        })?
    };
    let prepare_ms = prepare.elapsed().as_secs_f64() * 1000.0;
    let hydraulic = Instant::now();
    let _ = world.river_reach(0);
    let hydraulic_prepare_ms = hydraulic.elapsed().as_secs_f64() * 1000.0;
    let provinces = landforms::lake_provinces(&world);
    let started = Instant::now();
    let mut surveyed = Vec::new();
    let mut selected = None;
    let mut best_score = 0;
    for (index, province) in provinces.iter().take(province_limit).enumerate() {
        let mut wet = 0;
        let mut wetland = 0;
        for y in -4..=4 {
            for x in -4..=4 {
                let p = point(
                    province.direction,
                    world.config.radius_m,
                    x as f64 * 1_250.0,
                    y as f64 * 1_250.0,
                );
                let s = world.sample(p);
                wet += usize::from(micro(&s));
                wetland += usize::from(s.biome == Biome::Wetland);
            }
        }
        let score = wet * 2 + wetland;
        surveyed.push(json!({"province_index": index, "center": province.direction, "coarse_micro_lake_points": wet, "coarse_wetland_points": wetland, "sample_count":81}));
        if selected.is_none() || score > best_score {
            best_score = score;
            selected = Some((index, province.direction));
        }
    }
    let survey_ms = started.elapsed().as_secs_f64() * 1000.0;
    let Some((selected_index, surveyed_center)) = selected else {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"world_id":world.id,"seed":world.config.seed,"resolution":world.config.resolution,"province_count":0,"prepare_ms":prepare_ms,"hydraulic_prepare_ms":hydraulic_prepare_ms})
            )?
        );
        return Ok(());
    };
    let center = requested_center.unwrap_or(surveyed_center);
    let output_prefix = args
        .get(1)
        .map(String::as_str)
        .unwrap_or("/tmp/wb-local-water");
    let mut previous = None::<Vec<Sample>>;
    let mut results = Vec::new();
    for segments in [128usize, 256, 512] {
        let side = segments + 1;
        let mut grid = Vec::with_capacity(side * side);
        let mut levels = HashMap::<u64, f32>::new();
        let mut wet = 0;
        let mut wetland = 0;
        let mut max_depth = 0.0_f32;
        let started = Instant::now();
        for y in 0..side {
            for x in 0..side {
                let p = point(
                    center,
                    world.config.radius_m,
                    (x as f64 / segments as f64 - 0.5) * 10_000.0,
                    (y as f64 / segments as f64 - 0.5) * 10_000.0,
                );
                let s = world.sample(p);
                if micro(&s) {
                    wet += 1;
                    max_depth = max_depth.max(s.water_m - s.height_m);
                    if let Some(old) = levels.insert(s.water_body_id.unwrap(), s.water_m) {
                        anyhow::ensure!(
                            old.to_bits() == s.water_m.to_bits(),
                            "a micro lake changed hydraulic level"
                        );
                    }
                }
                wetland += usize::from(s.biome == Biome::Wetland);
                grid.push(s);
            }
        }
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut shared_changes = 0;
        if let Some(old) = previous.as_ref() {
            let old_side = segments / 2 + 1;
            for y in 0..old_side {
                for x in 0..old_side {
                    let a = &old[y * old_side + x];
                    let b = &grid[(y * 2) * side + x * 2];
                    shared_changes += usize::from(
                        a.height_m.to_bits() != b.height_m.to_bits()
                            || a.water_m.to_bits() != b.water_m.to_bits()
                            || a.water_body_id != b.water_body_id
                            || a.lake != b.lake,
                    );
                }
            }
        }
        let path = format!("{output_prefix}-{segments}.csv");
        let mut csv = BufWriter::new(File::create(&path)?);
        writeln!(csv, "x_m,y_m,height_m,water_m,micro_lake,wetland,body_id")?;
        for y in 0..side {
            for x in 0..side {
                let s = &grid[y * side + x];
                writeln!(
                    csv,
                    "{},{},{},{},{},{},{}",
                    (x as f64 / segments as f64 - 0.5) * 10_000.0,
                    (y as f64 / segments as f64 - 0.5) * 10_000.0,
                    s.height_m,
                    s.water_m,
                    usize::from(micro(s)),
                    s.biome_mix[Biome::Wetland as usize],
                    s.water_body_id
                        .map_or_else(String::new, |id| id.to_string())
                )?;
            }
        }
        csv.flush()?;
        results.push(json!({"segments":segments,"spacing_m":10_000.0 / segments as f64,"sample_count":grid.len(),"micro_lake_points":wet,"micro_lake_fraction":wet as f64 / grid.len() as f64,"wetland_points":wetland,"lake_id_count":levels.len(),"max_depth_m":max_depth,"shared_sample_changes":shared_changes,"sampling_ms":elapsed_ms,"csv":path}));
        previous = Some(grid);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"world_id":world.id,"seed":world.config.seed,"resolution":world.config.resolution,"config":world.config,"prepare_ms":prepare_ms,"prepare_source":prepare_source,"hydraulic_prepare_ms":hydraulic_prepare_ms,"province_count":provinces.len(),"survey_ms":survey_ms,"surveyed":surveyed,"selected_province_index":selected_index,"requested_center":requested_center,"center":center,"extent_m":10_000.0,"grids":results,"interpretation":"Nested sample points compare the same frozen field. Extra visible ponds at smaller spacing are resolution of existing objects, not creation by zoom. These unresolved basins do not add a validated global hydrological budget."})
        )?
    );
    Ok(())
}
