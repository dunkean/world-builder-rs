//! Fixed 10 km/1 km lake-field measurements and CSVs; no renderer claim.
use anyhow::{Context, Result};
use serde_json::{json, Value};
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

fn changed(a: &Sample, b: &Sample) -> bool {
    a.direction.map(f64::to_bits) != b.direction.map(f64::to_bits)
        || a.height_m.to_bits() != b.height_m.to_bits()
        || a.water_m.to_bits() != b.water_m.to_bits()
        || a.water_level_m.map(f32::to_bits) != b.water_level_m.map(f32::to_bits)
        || a.water_body_id != b.water_body_id
        || a.temperature_c.to_bits() != b.temperature_c.to_bits()
        || a.moisture.to_bits() != b.moisture.to_bits()
        || a.discharge.to_bits() != b.discharge.to_bits()
        || a.biome != b.biome
        || a.biome_mix.map(f32::to_bits) != b.biome_mix.map(f32::to_bits)
        || a.material != b.material
        || a.erodability.to_bits() != b.erodability.to_bits()
        || a.ocean != b.ocean
        || a.lake != b.lake
        || a.river.to_bits() != b.river.to_bits()
}

#[derive(Default)]
struct ObservedLake {
    points: usize,
    area_m2: f64,
    x_sum: f64,
    y_sum: f64,
    touches_edge: bool,
}

fn measure_window(
    world: &World,
    center: [f64; 3],
    extent_m: f64,
    segments_list: &[usize],
    output_prefix: &str,
    all_levels: &mut HashMap<u64, f32>,
) -> Result<Vec<Value>> {
    let mut previous = None::<(usize, Vec<Sample>)>;
    let mut results = Vec::new();
    for &segments in segments_list {
        let side = segments + 1;
        let spacing_m = extent_m / segments as f64;
        let mut grid = Vec::with_capacity(side * side);
        let mut lakes = HashMap::<u64, ObservedLake>::new();
        let mut wet = 0;
        let mut wetland = 0;
        let mut wetland_margin = 0;
        let mut max_depth = 0.0_f32;
        let started = Instant::now();
        for y in 0..side {
            for x in 0..side {
                let px = (x as f64 / segments as f64 - 0.5) * extent_m;
                let py = (y as f64 / segments as f64 - 0.5) * extent_m;
                let p = point(center, world.config.radius_m, px, py);
                let s = world.sample(p);
                if micro(&s) {
                    wet += 1;
                    max_depth = max_depth.max(s.water_m - s.height_m);
                    let id = s.water_body_id.unwrap();
                    if let Some(old) = all_levels.insert(id, s.water_m) {
                        anyhow::ensure!(
                            old.to_bits() == s.water_m.to_bits(),
                            "a lake changed hydraulic level"
                        );
                    }
                    let lake = lakes.entry(id).or_default();
                    lake.points += 1;
                    // Half-cell endpoint weights avoid overcounting the outer
                    // row. Equivalent diameters remain sampled estimates.
                    let wx = if x == 0 || x == segments { 0.5 } else { 1.0 };
                    let wy = if y == 0 || y == segments { 0.5 } else { 1.0 };
                    lake.area_m2 += wx * wy * spacing_m * spacing_m;
                    lake.x_sum += px;
                    lake.y_sum += py;
                    lake.touches_edge |= x == 0 || x == segments || y == 0 || y == segments;
                }
                wetland += usize::from(s.biome == Biome::Wetland);
                wetland_margin +=
                    usize::from(!micro(&s) && s.biome_mix[Biome::Wetland as usize] > 0.20);
                grid.push(s);
            }
        }
        let sampling_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut shared_changes = 0;
        let mut shared_count = 0;
        if let Some((old_segments, old)) = previous.as_ref() {
            anyhow::ensure!(segments % old_segments == 0, "grids must nest exactly");
            let ratio = segments / old_segments;
            let old_side = old_segments + 1;
            for y in 0..old_side {
                for x in 0..old_side {
                    shared_count += 1;
                    shared_changes += usize::from(changed(
                        &old[y * old_side + x],
                        &grid[(y * ratio) * side + x * ratio],
                    ));
                }
            }
        }
        let replay_started = Instant::now();
        let mut replay_count = 0;
        let mut replay_changes = 0;
        for index in (0..grid.len()).step_by(97).rev() {
            let x = index % side;
            let y = index / side;
            let p = point(
                center,
                world.config.radius_m,
                (x as f64 / segments as f64 - 0.5) * extent_m,
                (y as f64 / segments as f64 - 0.5) * extent_m,
            );
            replay_changes += usize::from(changed(&grid[index], &world.sample(p)));
            replay_count += 1;
        }
        let replay_ms = replay_started.elapsed().as_secs_f64() * 1000.0;
        let mut tier_counts = [0usize; 4];
        let mut diameter_counts = [0usize; 7];
        let diameter_upper_bounds_m = [25.0, 50.0, 100.0, 250.0, 500.0, 1_000.0];
        let mut complete_diameters = Vec::new();
        let cell_side = 10usize;
        let mut field_counts = vec![0usize; cell_side * cell_side];
        for (&id, lake) in &lakes {
            tier_counts[((id >> 38) & 3) as usize] += 1;
            if !lake.touches_edge {
                let diameter = 2.0 * (lake.area_m2 / std::f64::consts::PI).sqrt();
                let bin = diameter_upper_bounds_m
                    .iter()
                    .position(|&upper| diameter < upper)
                    .unwrap_or(6);
                diameter_counts[bin] += 1;
                complete_diameters.push(diameter);
            }
            let cx = ((lake.x_sum / lake.points as f64 / extent_m + 0.5) * cell_side as f64)
                .clamp(0.0, (cell_side - 1) as f64) as usize;
            let cy = ((lake.y_sum / lake.points as f64 / extent_m + 0.5) * cell_side as f64)
                .clamp(0.0, (cell_side - 1) as f64) as usize;
            field_counts[cy * cell_side + cx] += 1;
        }
        complete_diameters.sort_by(f64::total_cmp);
        let quantile = |fraction: f64| {
            complete_diameters
                .get(((complete_diameters.len().saturating_sub(1)) as f64 * fraction) as usize)
                .copied()
        };
        let mean = lakes.len() as f64 / field_counts.len() as f64;
        let variance = field_counts
            .iter()
            .map(|&n| (n as f64 - mean).powi(2))
            .sum::<f64>()
            / field_counts.len() as f64;
        let path = format!("{output_prefix}-{extent_m:.0}m-{segments}.csv");
        let mut csv = BufWriter::new(File::create(&path)?);
        writeln!(
            csv,
            "x_m,y_m,height_m,water_m,micro_lake,wetland,body_id,tier"
        )?;
        for y in 0..side {
            for x in 0..side {
                let s = &grid[y * side + x];
                writeln!(
                    csv,
                    "{},{},{},{},{},{},{},{}",
                    (x as f64 / segments as f64 - 0.5) * extent_m,
                    (y as f64 / segments as f64 - 0.5) * extent_m,
                    s.height_m,
                    s.water_m,
                    usize::from(micro(s)),
                    s.biome_mix[Biome::Wetland as usize],
                    s.water_body_id
                        .map_or_else(String::new, |id| id.to_string()),
                    if micro(s) {
                        ((s.water_body_id.unwrap() >> 38) & 3).to_string()
                    } else {
                        String::new()
                    }
                )?;
            }
        }
        csv.flush()?;
        results.push(json!({"extent_m":extent_m,"segments":segments,"spacing_m":spacing_m,"sample_count":grid.len(),
            "micro_lake_points":wet,"micro_lake_fraction":wet as f64 / grid.len() as f64,"wetland_points":wetland,
            "dry_wetland_margin_points":wetland_margin,"lake_id_count":lakes.len(),"lake_ids_by_tier":tier_counts,
            "complete_lake_count":complete_diameters.len(),"sampled_equivalent_diameter_bins_m":["<25","25–50","50–100","100–250","250–500","500–1000",">=1000"],
            "sampled_equivalent_diameter_bin_counts":diameter_counts,"sampled_equivalent_diameter_p10_m":quantile(0.1),
            "sampled_equivalent_diameter_p50_m":quantile(0.5),"sampled_equivalent_diameter_p90_m":quantile(0.9),
            "lake_centroid_cells_per_side":cell_side,"lake_centroid_cell_counts":field_counts,
            "lake_centroid_cell_coefficient_of_variation":if mean>0.0 {Some(variance.sqrt()/mean)} else {None},
            "max_depth_m":max_depth,"shared_sample_count":shared_count,"shared_sample_changes":shared_changes,
            "reverse_replay_count":replay_count,"reverse_replay_changes":replay_changes,"reverse_replay_ms":replay_ms,
            "sampling_ms":sampling_ms,"csv":path}));
        previous = Some((segments, grid));
    }
    Ok(results)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let positional: Vec<&str> = args
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .map(String::as_str)
        .collect();
    let requested_center = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--center="))
        .map(|value| -> Result<[f64; 3]> {
            let coordinates = value
                .split(',')
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let direction: [f64; 3] = coordinates
                .try_into()
                .map_err(|_| anyhow::anyhow!("--center requires three coordinates"))?;
            anyhow::ensure!(
                direction.iter().all(|v| v.is_finite()) && direction.iter().any(|&v| v != 0.0),
                "invalid center"
            );
            Ok(normalize(direction))
        })
        .transpose()?;
    let province_limit = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--provinces="))
        .map(str::parse::<usize>)
        .transpose()?
        .unwrap_or(32);
    let extents: Vec<f64> = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--extents="))
        .unwrap_or("10000,1000")
        .split(',')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    let segments: Vec<usize> = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--segments="))
        .unwrap_or("128,256,512")
        .split(',')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    anyhow::ensure!(
        extents.iter().all(|v| v.is_finite() && *v > 0.0),
        "extents must be positive metres"
    );
    anyhow::ensure!(
        segments.iter().all(|&v| v > 0)
            && segments.windows(2).all(|v| v[1] > v[0] && v[1] % v[0] == 0),
        "segments must be positive nested grids"
    );
    let saved_path = positional.first().filter(|path| path.ends_with(".json"));
    let prepare_source = if saved_path.is_some() {
        "load_saved_atlas"
    } else {
        "generate_atlas"
    };
    let prepare = Instant::now();
    let mut world: World = if let Some(path) = saved_path {
        serde_json::from_reader(BufReader::new(File::open(path)?)).context("load world")?
    } else {
        World::generate(WorldConfig {
            seed: 2_386_084_681,
            resolution: positional
                .first()
                .map(|s| s.parse())
                .transpose()?
                .unwrap_or(128),
            erosion_steps: 25,
            ..Default::default()
        })?
    };
    let override_value = |prefix: &str| -> Result<Option<f64>> {
        Ok(args
            .iter()
            .find_map(|arg| arg.strip_prefix(prefix))
            .map(str::parse)
            .transpose()?)
    };
    let lake_density = override_value("--lake-density=")?;
    let island_density = override_value("--island-density=")?;
    let fjord_strength = override_value("--fjord-strength=")?;
    if let Some(value) = lake_density {
        world.config.regional_lake_density = value;
    }
    if let Some(value) = island_density {
        world.config.archipelago_density = value;
    }
    if let Some(value) = fjord_strength {
        world.config.fjord_strength = value;
    }
    world.config.validate()?;
    if lake_density.is_some() || island_density.is_some() || fjord_strength.is_some() {
        world.invalidate_geometry();
    }
    let config_overrides = json!({"lake_density":lake_density,"island_density":island_density,"fjord_strength":fjord_strength});
    let prepare_ms = prepare.elapsed().as_secs_f64() * 1000.0;
    let hydraulic = Instant::now();
    let _ = world.river_reach(0);
    let hydraulic_prepare_ms = hydraulic.elapsed().as_secs_f64() * 1000.0;
    let hydraulic_local_water_stats = world_core::local_water::cache_stats(&world);
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
        surveyed.push(json!({"province_index":index,"center":province.direction,"coarse_micro_lake_points":wet,"coarse_wetland_points":wetland,"sample_count":81}));
        if selected.is_none() || score > best_score {
            best_score = score;
            selected = Some((index, province.direction));
        }
    }
    let survey_ms = started.elapsed().as_secs_f64() * 1000.0;
    let center = requested_center.or_else(|| selected.map(|(_, center)| center));
    let Some(center) = center else {
        anyhow::bail!("no lake province found; use --center=x,y,z for an explicit window");
    };
    let output_prefix = positional.get(1).copied().unwrap_or("/tmp/wb-local-water");
    let mut grids = Vec::new();
    let mut all_levels = HashMap::new();
    for extent_m in &extents {
        grids.extend(measure_window(
            &world,
            center,
            *extent_m,
            &segments,
            output_prefix,
            &mut all_levels,
        )?);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"world_id":world.id,"seed":world.config.seed,"resolution":world.config.resolution,
        "config":world.config,"config_overrides":config_overrides,"prepare_ms":prepare_ms,"prepare_source":prepare_source,"hydraulic_prepare_ms":hydraulic_prepare_ms,
        "province_count":provinces.len(),"survey_ms":survey_ms,"surveyed":surveyed,
        "hydraulic_local_water_stats":hydraulic_local_water_stats,"final_local_water_stats":world_core::local_water::cache_stats(&world),
        "selected_province_index":selected.map(|(index,_)|index),"requested_center":requested_center,"center":center,"extents_m":extents,"grids":grids,
        "interpretation":"Nested samples and reverse replay compare the entire same frozen sample. Finer spacing resolves existing objects, never creates them. Diameter histograms estimate wet sampled area, omit edge-clipped objects and are biased below a few grid cells. These basins do not add a validated global water budget or establish renderer quality."})
        )?
    );
    Ok(())
}
