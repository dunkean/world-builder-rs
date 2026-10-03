//! Numerical quality probes. These measure the spatial field, not GPU rendering.
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{collections::VecDeque, fs::File, io::BufReader, time::Instant};
use world_core::{
    geometry::{cross, direction, face_uv, neighbours, normalize},
    Sample, TileKey, World, WorldConfig,
};

fn tangent_point(center: [f64; 3], radius: f64, x: f64, y: f64) -> [f64; 3] {
    let axis = if center[1].abs() < 0.9 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let east = normalize(cross(axis, center));
    let north = cross(center, east);
    normalize(std::array::from_fn(|i| {
        center[i] + (x * east[i] + y * north[i]) / radius
    }))
}

fn components(mask: &[bool], side: usize, value: bool) -> (usize, usize) {
    let mut visited = vec![false; mask.len()];
    let mut total = 0;
    let mut interior = 0;
    for start in 0..mask.len() {
        if visited[start] || mask[start] != value {
            continue;
        }
        total += 1;
        let mut border = false;
        let mut queue = VecDeque::from([start]);
        visited[start] = true;
        while let Some(i) = queue.pop_front() {
            let x = i % side;
            let y = i / side;
            border |= x == 0 || y == 0 || x + 1 == side || y + 1 == side;
            for (nx, ny) in [
                (x.wrapping_sub(1), y),
                (x + 1, y),
                (x, y.wrapping_sub(1)),
                (x, y + 1),
            ] {
                if nx >= side || ny >= side {
                    continue;
                }
                let j = ny * side + nx;
                if !visited[j] && mask[j] == value {
                    visited[j] = true;
                    queue.push_back(j);
                }
            }
        }
        interior += usize::from(!border);
    }
    (total, interior)
}

fn coast_grids(world: &World, center: [f64; 3]) -> Value {
    let extent = 10_000.0;
    let mut previous: Option<Vec<bool>> = None;
    let mut result = Vec::new();
    let mut saved_grids = Vec::new();
    for segments in [8usize, 16, 32, 64, 128, 256, 512] {
        let side = segments + 1;
        let mut mask = Vec::with_capacity(side * side);
        let mut heights = Vec::with_capacity(side * side);
        for y in 0..side {
            for x in 0..side {
                let p = tangent_point(
                    center,
                    world.config.radius_m,
                    extent * (x as f64 / segments as f64 - 0.5),
                    extent * (y as f64 / segments as f64 - 0.5),
                );
                let sample = world.sample(p);
                mask.push(sample.ocean);
                heights.push(sample.height_m);
            }
        }
        let mut shared_changes = 0;
        if let Some(old) = previous.as_ref() {
            let old_side = (segments / 2) + 1;
            for y in 0..old_side {
                for x in 0..old_side {
                    shared_changes +=
                        usize::from(old[y * old_side + x] != mask[(2 * y) * side + 2 * x]);
                }
            }
        }
        let sea = components(&mask, side, true);
        let land = components(&mask, side, false);
        let min = heights.iter().copied().fold(f32::INFINITY, f32::min);
        let max = heights.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        result.push(
            json!({"segments":segments,"spacing_m":extent/segments as f64,
            "ocean_components_4_connected":sea.0,"interior_ocean_components":sea.1,
            "land_components_4_connected":land.0,"interior_land_components":land.1,
            "changed_shared_ocean_samples":shared_changes,"min_height_m":min,"max_height_m":max}),
        );
        previous = Some(mask.clone());
        saved_grids.push((segments, mask, heights));
    }
    let (fine_segments, fine_mask, _) = saved_grids.last().unwrap();
    let fine_side = fine_segments + 1;
    for (record, (segments, _, heights)) in result.iter_mut().zip(&saved_grids) {
        let stride = fine_segments / segments;
        let side = segments + 1;
        let mut different = 0usize;
        for iy in 0..fine_side {
            for ix in 0..fine_side {
                let x = (ix / stride).min(segments - 1);
                let y = (iy / stride).min(segments - 1);
                let tx = ix as f64 / stride as f64 - x as f64;
                let ty = iy as f64 / stride as f64 - y as f64;
                let a = heights[y * side + x] as f64;
                let b = heights[y * side + x + 1] as f64;
                let c = heights[(y + 1) * side + x + 1] as f64;
                let d = heights[(y + 1) * side + x] as f64;
                // Same fixed diagonal as World::tile; this is an affine height
                // estimate, not a replay of sphere projection or the GPU shader.
                let interpolated = if tx >= ty {
                    a * (1.0 - tx) + b * (tx - ty) + c * ty
                } else {
                    a * (1.0 - ty) + c * tx + d * (ty - tx)
                };
                different += usize::from((interpolated < 0.0) != fine_mask[iy * fine_side + ix]);
            }
        }
        record["triangle_ocean_disagreement_vs_512_fraction"] =
            json!(different as f64 / fine_mask.len() as f64);
    }
    json!({"center":center,"extent_m":extent,"grids":result,
        "interpretation":"Component counts describe raster connectivity. Zero shared changes proves the same sampled field, not invariant polygon topology under undersampling. Triangle disagreement estimates unresolved shore area against the 512 grid, without reproducing GPU projection."})
}

fn parent_child(world: &World, center: [f64; 3]) -> Result<Value> {
    let (face, u, v) = face_uv(center);
    let mut mismatches = 0;
    let mut compared = 0;
    let mut worst_height = 0.0_f32;
    for level in 0..24u8 {
        let count = 1u32 << level;
        let x = (((u + 1.0) * 0.5 * count as f64).floor() as u32).min(count - 1);
        let y = (((v + 1.0) * 0.5 * count as f64).floor() as u32).min(count - 1);
        let a = world.tile(TileKey { face, level, x, y }, 8)?;
        let b = world.tile(
            TileKey {
                face,
                level: level + 1,
                x: x * 2,
                y: y * 2,
            },
            8,
        )?;
        for iy in 0..=4 {
            for ix in 0..=4 {
                let parent = &a.vertices[iy * 9 + ix];
                let child = &b.vertices[(iy * 2) * 9 + ix * 2];
                mismatches +=
                    usize::from(serde_json::to_vec(parent)? != serde_json::to_vec(child)?);
                compared += 1;
                worst_height = worst_height.max((parent.height_m - child.height_m).abs());
            }
        }
    }
    Ok(
        json!({"level_pairs":24,"shared_points":compared,"full_sample_mismatches":mismatches,"max_height_difference_m":worst_height}),
    )
}

#[derive(Default)]
struct WaterEdges {
    pairs: usize,
    both_wet: usize,
    changed_owner: usize,
    jumps_over_10cm: usize,
    maximum_jump_m: f64,
    worst: Option<Value>,
    large_jump_cases: Vec<Value>,
}

impl WaterEdges {
    fn observe(&mut self, a: &Sample, b: &Sample, point: Value) {
        self.pairs += 1;
        let wet =
            |s: &Sample| s.water_m > s.height_m + 0.0001 && (s.ocean || s.lake || s.river > 0.0);
        if !wet(a) || !wet(b) {
            return;
        }
        self.both_wet += 1;
        if a.water_body_id == b.water_body_id {
            return;
        }
        self.changed_owner += 1;
        let jump = (a.water_m - b.water_m).abs() as f64;
        self.jumps_over_10cm += usize::from(jump > 0.1);
        if jump > 0.1 && self.large_jump_cases.len() < 64 {
            self.large_jump_cases.push(json!({"point":point,"water_before_m":a.water_m,"water_after_m":b.water_m,"body_before":a.water_body_id,"body_after":b.water_body_id,"height_before_m":a.height_m,"height_after_m":b.height_m,"before_lake":a.lake,"after_lake":b.lake,"before_ocean":a.ocean,"after_ocean":b.ocean,"river_before":a.river,"river_after":b.river}));
        }
        if jump > self.maximum_jump_m {
            self.maximum_jump_m = jump;
            self.worst = Some(
                json!({"point":point,"water_before_m":a.water_m,"water_after_m":b.water_m,
                "body_before":a.water_body_id,"body_after":b.water_body_id,
                "height_before_m":a.height_m,"height_after_m":b.height_m,
                "before_ocean":a.ocean,"after_ocean":b.ocean,"before_lake":a.lake,"after_lake":b.lake,
                "river_before":a.river,"river_after":b.river}),
            );
        }
    }
    fn report(&self) -> Value {
        json!({"sample_pairs":self.pairs,"both_wet_pairs":self.both_wet,
            "changed_wet_owner_pairs":self.changed_owner,"owner_switch_jumps_over_10cm":self.jumps_over_10cm,
            "max_water_surface_jump_m":self.maximum_jump_m,"worst_case":self.worst,"large_jump_cases":self.large_jump_cases})
    }
}

fn warped_water_edges(world: &World) -> Value {
    // Locate changes in the real warped field rather than duplicating the
    // private remapping algorithm in this probe. A coarse search brackets each
    // owner change, then bisection compares positions about a millimetre apart.
    let n = world.config.resolution as usize;
    let mut edges = WaterEdges::default();
    let mut scan_samples = 0;
    for (i, _) in world.cells.iter().enumerate().filter(|(_, c)| c.lake) {
        let face = (i / (n * n)) as u8;
        let uc = 2.0 * ((i % n) as f64 + 0.5) / n as f64 - 1.0;
        let vc = 2.0 * (((i / n) % n) as f64 + 0.5) / n as f64 - 1.0;
        for axis in 0..2 {
            for fraction in [-0.4, 0.0, 0.4] {
                let at = |offset: f64| {
                    let u = uc
                        + if axis == 0 {
                            offset
                        } else {
                            fraction * 2.0 / n as f64
                        };
                    let v = vc
                        + if axis == 1 {
                            offset
                        } else {
                            fraction * 2.0 / n as f64
                        };
                    direction(face, u, v)
                };
                let mut lo = -3.0 / n as f64;
                let mut previous = world.sample(at(lo));
                scan_samples += 1;
                for step in 1..=24 {
                    let hi = (-3.0 + step as f64 / 4.0) / n as f64;
                    let current = world.sample(at(hi));
                    scan_samples += 1;
                    if current.water_body_id != previous.water_body_id {
                        let mut left = lo;
                        let mut right = hi;
                        let owner = previous.water_body_id;
                        for _ in 0..32 {
                            if (right - left) * world.config.radius_m < 0.001 {
                                break;
                            }
                            let middle = 0.5 * (left + right);
                            if world.sample(at(middle)).water_body_id == owner {
                                left = middle;
                            } else {
                                right = middle;
                            }
                        }
                        let a = world.sample(at(left));
                        let b = world.sample(at(right));
                        edges.observe(&a, &b, json!({"lake_cell":i,"face":face,"axis":axis,"direction":at(0.5*(left+right)),"before_direction":at(left),"after_direction":at(right),"pair_spacing_m":(right-left)*world.config.radius_m}));
                    }
                    previous = current;
                    lo = hi;
                }
            }
        }
    }
    json!({"scan_samples":scan_samples,"owner_boundaries":edges.report(),
        "interpretation":"Owner boundaries located by scanning lake neighbourhoods in the actual warped field and bisecting to a millimetre. This does not exhaust every possible boundary on the planet."})
}

fn lake_edges(world: &World) -> Value {
    // Disable coastal warp so these known atlas support borders are also actual
    // attribute-stencil borders. Geometry and water classifications stay frozen.
    let mut unwarped = world.clone();
    unwarped.config.coast_detail_ratio = 0.0;
    unwarped.invalidate_geometry();
    let n = world.config.resolution as usize;
    let epsilon = 1e-8;
    let mut jumps = Vec::new();
    let mut worst = json!(null);
    let mut maximum = 0.0;
    let mut water_edges = WaterEdges::default();
    for (i, cell) in world.cells.iter().enumerate().filter(|(_, c)| c.lake) {
        let face = (i / (n * n)) as u8;
        let x = i % n;
        let y = (i / n) % n;
        let uc = 2.0 * (x as f64 + 0.5) / n as f64 - 1.0;
        let vc = 2.0 * (y as f64 + 0.5) / n as f64 - 1.0;
        for axis in 0..2 {
            for sign in [-1.0, 1.0] {
                for fraction in [-0.4, 0.0, 0.4] {
                    let mut u = uc;
                    let mut v = vc;
                    if axis == 0 {
                        u += sign * 2.0 / n as f64;
                        v += fraction * 2.0 / n as f64;
                    } else {
                        v += sign * 2.0 / n as f64;
                        u += fraction * 2.0 / n as f64;
                    }
                    if u.abs() >= 0.999 || v.abs() >= 0.999 {
                        continue;
                    }
                    let a = unwarped.sample(direction(
                        face,
                        u - if axis == 0 { epsilon } else { 0.0 },
                        v - if axis == 1 { epsilon } else { 0.0 },
                    ));
                    let b = unwarped.sample(direction(
                        face,
                        u + if axis == 0 { epsilon } else { 0.0 },
                        v + if axis == 1 { epsilon } else { 0.0 },
                    ));
                    let jump = (a.height_m - b.height_m).abs() as f64;
                    water_edges.observe(&a, &b, json!({"lake_cell":i,"face":face,"u":u,"v":v,"direction":direction(face,u,v)}));
                    jumps.push(jump);
                    if jump > maximum {
                        maximum = jump;
                        worst = json!({"lake_cell":i,"lake_id":cell.lake_id,"face":face,"u":u,"v":v,"direction":direction(face,u,v),"height_before_m":a.height_m,"height_after_m":b.height_m});
                    }
                }
            }
        }
    }
    jumps.sort_by(f64::total_cmp);
    let percentile = |q: f64| {
        jumps
            .get(((jumps.len().saturating_sub(1)) as f64 * q).round() as usize)
            .copied()
    };
    json!({"sample_pairs":jumps.len(),"epsilon_face_coordinate":epsilon,"nominal_pair_spacing_m":world.config.radius_m*2.0*epsilon,
        "p50_height_difference_m":percentile(0.5),"p95_height_difference_m":percentile(0.95),"max_height_difference_m":maximum,"worst_case":worst,"water_surfaces":water_edges.report(),
        "interpretation":"Finite differences across lake attribute support borders with coastline warp disabled. Includes physical terrain gradient; no claim about GPU squares."})
}

fn river_profiles(world: &World) -> Value {
    let mut natural = world.clone();
    for c in &mut natural.cells {
        c.receiver = None;
    }
    natural.invalidate_geometry();
    let mut reaches: Vec<_> = (0..world.cells.len())
        .filter_map(|i| world.river_reach(i))
        .collect();
    let total = reaches.len();
    reaches.sort_by(|a, b| b.discharge_m3_s.total_cmp(&a.discharge_m3_s));
    reaches.truncate(512);
    let mut elevated = 0;
    let mut mismatches = 0;
    let mut samples = 0;
    let mut max_lift = 0.0_f64;
    let mut max_depth = 0.0_f64;
    let mut deepest = json!(null);
    let mut max_river_depth = 0.0_f64;
    let mut deepest_river = json!(null);
    let mut worst = json!(null);
    let mut mismatch_case = json!(null);
    let mut mismatch_cases = Vec::new();
    for reach in &reaches {
        for step in 1..32 {
            let t = step as f64 / 32.0;
            let p = reach.position(t);
            let original = natural.sample(p);
            let sample = world.sample(p);
            let water = reach.level(t);
            let lift = water - original.height_m as f64;
            let depth = sample.water_m as f64 - sample.height_m as f64;
            if depth > max_depth {
                max_depth = depth;
                deepest = json!({"from":reach.from,"to":reach.to,"t":t,"direction":p,"water_level_m":water,"natural_height_m":original.height_m,"sample_height_m":sample.height_m,"sample_water_m":sample.water_m,"sample_body_id":sample.water_body_id,"river":sample.river,"ocean":sample.ocean,"lake":sample.lake,"intended_depth_m":reach.depth(t),"width_m":reach.width(t),"discharge_m3_s":reach.discharge_m3_s});
            }
            if !sample.ocean && !sample.lake && sample.river > 0.0 && depth > max_river_depth {
                max_river_depth = depth;
                deepest_river = json!({"from":reach.from,"to":reach.to,"t":t,"direction":p,"water_level_m":water,"natural_height_m":original.height_m,"sample_height_m":sample.height_m,"sample_water_m":sample.water_m,"sample_body_id":sample.water_body_id,"river":sample.river,"intended_depth_m":reach.depth(t),"width_m":reach.width(t),"discharge_m3_s":reach.discharge_m3_s});
            }
            elevated += usize::from(lift > 10.0);
            samples += 1;
            if lift > max_lift {
                max_lift = lift;
                worst = json!({"from":reach.from,"to":reach.to,"t":t,"direction":p,"water_level_m":water,"natural_height_m":original.height_m,"sample_height_m":sample.height_m,"sample_water_m":sample.water_m,"intended_depth_m":reach.depth(t),"width_m":reach.width(t),"discharge_m3_s":reach.discharge_m3_s});
            }
            if !sample.ocean && !sample.lake && (sample.water_m as f64 - water).abs() > 0.02 {
                mismatches += 1;
                let selected = sample.water_body_id.and_then(|id|id.checked_sub((1u64<<52)+1)).and_then(|from|world.river_reach(from as usize)).map(|owner| {
                    let (distance,parameter)=owner.closest(p);
                    json!({"from":owner.from,"to":owner.to,"raw_receiver":world.cells[owner.from].receiver,"closest_t":parameter,"distance_m":distance,"level_m":owner.level(parameter),"lake_inlet":owner.lake_inlet,"lake_outlet":owner.lake_outlet,"marine_mouth":owner.marine_mouth})
                });
                let case = json!({"from":reach.from,"to":reach.to,"t":t,"direction":p,"canonical_level_m":water,"sample_level_m":sample.water_m,"sample_body_id":sample.water_body_id,"river":sample.river,"selected_reach":selected});
                if mismatch_case.is_null() {
                    mismatch_case = case.clone();
                }
                if mismatch_cases.len() < 64 {
                    mismatch_cases.push(case);
                }
            }
        }
    }
    json!({"all_reaches":total,"major_reaches_checked":reaches.len(),"samples":samples,
        "water_more_than_10m_above_natural_terrain":elevated,"max_water_lift_above_natural_m":max_lift,
        "max_sampled_water_depth_m":max_depth,"non_lake_non_ocean_profile_mismatches":mismatches,
        "max_river_water_depth_m":max_river_depth,"deepest_river_case":deepest_river,
        "worst_lift_case":worst,"deepest_sample_case":deepest,"first_mismatch_case":mismatch_case,"mismatch_cases":mismatch_cases,
        "interpretation":"Natural field omits all river carving. Elevated water can create deep flooded channels when linear endpoint levels cross lower interior terrain. Overlapping canonical reaches can select another water owner."})
}

fn lake_reconstruction_lift(world: &World) -> Value {
    // Isolate reservoir reconstruction from fine detail and all river incision.
    // The reference retains the same frozen DEM, coast map and climate.
    let mut protected = world.clone();
    protected.config.detail_m = 0.0;
    for cell in &mut protected.cells {
        cell.receiver = None;
    }
    protected.invalidate_geometry();
    let mut reference = protected.clone();
    reference.lakes.clear();
    for cell in &mut reference.cells {
        cell.lake = false;
        cell.lake_id = None;
    }
    reference.invalidate_geometry();
    let mut samples = 0;
    let mut modified = 0;
    let mut lifted = Vec::new();
    let mut worst = json!(null);
    let mut maximum = 0.0;
    let n = world.config.resolution as f64;
    for (index, cell) in world
        .cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c.lake && !c.ocean)
    {
        let (face, u, v) = face_uv(cell.position);
        for distance in [0.6, 0.9, 1.0, 1.1, 1.4] {
            for azimuth in 0..8 {
                let phi = azimuth as f64 * std::f64::consts::TAU / 8.0;
                let p = direction(
                    face,
                    u + distance * 2.0 / n * phi.cos(),
                    v + distance * 2.0 / n * phi.sin(),
                );
                let bare = reference.sample(p).height_m;
                let retained = protected.sample(p).height_m;
                let lift = f64::from(retained - bare);
                samples += 1;
                if lift > 0.001 {
                    modified += 1;
                    lifted.push(lift);
                }
                if lift > maximum {
                    maximum = lift;
                    worst = json!({"lake_cell":index,"direction":p,"reference_height_m":bare,"retained_height_m":retained});
                }
            }
        }
    }
    lifted.sort_by(f64::total_cmp);
    let percentile = |q: f64| {
        lifted
            .get(((lifted.len().saturating_sub(1)) as f64 * q).round() as usize)
            .copied()
    };
    let mut corner_modified = 0;
    let mut corner_maximum = 0.0_f64;
    for x in [-1.0, 1.0] {
        for y in [-1.0, 1.0] {
            for z in [-1.0, 1.0] {
                let p = normalize([x, y, z]);
                let lift = f64::from(protected.sample(p).height_m - reference.sample(p).height_m);
                corner_modified += usize::from(lift > 0.001);
                corner_maximum = corner_maximum.max(lift);
            }
        }
    }
    json!({"samples_near_wet_atlas_cells":samples,"samples_lifted_over_1mm":modified,"modified_fraction":modified as f64/samples.max(1) as f64,
      "p50_positive_lift_m":percentile(0.5),"p95_positive_lift_m":percentile(0.95),"max_lift_m":maximum,"worst_case":worst,
      "cube_corners_checked":8,"cube_corners_lifted_over_1mm":corner_modified,"max_cube_corner_lift_m":corner_maximum,
      "interpretation":"Reservoir collar/divide reconstruction against the same frozen DEM with lakes removed. Both variants disable vertical detail and river carving. Positive lifts quantify added retaining terrain, not a hydraulic erosion or visual naturalness proof."})
}

fn river_overlap_boundaries(world: &World, profiles: &Value) -> Value {
    let cases = profiles["mismatch_cases"].as_array().unwrap();
    let mut total_samples = 0;
    let mut water_edges = WaterEdges::default();
    let mut maximum_height_jump = 0.0_f64;
    let mut height_jumps_over_10cm = 0;
    let mut height_cases = Vec::new();
    let mut worst_height = json!(null);
    let mut per_case = Vec::new();
    let mut max_river_depth = 0.0_f64;
    let mut max_depth_excess = 0.0_f64;
    let mut worst_depth = json!(null);
    let state = |sample: &Sample| {
        (
            sample.water_body_id,
            sample.water_m > sample.height_m + 0.0001,
        )
    };
    let mut observe_depth = |sample: &Sample, point: [f64; 3]| {
        if sample.ocean || sample.lake || sample.river <= 0.0 {
            return;
        }
        let depth = f64::from(sample.water_m - sample.height_m);
        let nominal = sample
            .water_body_id
            .and_then(|id| id.checked_sub((1u64 << 52) + 1))
            .and_then(|from| world.river_reach(from as usize))
            .map(|owner| owner.depth(owner.closest(point).1))
            .unwrap_or(0.0);
        max_depth_excess = max_depth_excess.max(depth - nominal);
        if depth > max_river_depth {
            max_river_depth = depth;
            worst_depth = json!({"direction":point,"height_m":sample.height_m,"water_m":sample.water_m,"owner":sample.water_body_id,"nominal_depth_m":nominal,"river":sample.river});
        }
    };
    for (case_index, case) in cases.iter().enumerate() {
        let from = case["from"].as_u64().unwrap() as usize;
        let t = case["t"].as_f64().unwrap();
        let reach = world.river_reach(from).unwrap();
        let center = reach.position(t);
        let before = reach.position((t - 0.001).max(0.0));
        let after = reach.position((t + 0.001).min(1.0));
        let along = normalize(std::array::from_fn(|axis| after[axis] - before[axis]));
        let across = normalize(cross(center, along));
        let extent = (reach.width(t) * 4.0).max(200.0).min(2000.0);
        let initial_pairs = water_edges.pairs;
        let initial_jumps = water_edges.jumps_over_10cm;
        let mut local_max_height = 0.0_f64;
        for axis in 0..2 {
            for fraction in [-0.8, -0.6, -0.4, -0.2, 0.0, 0.2, 0.4, 0.6, 0.8] {
                let at = |offset: f64| {
                    let (x, y) = if axis == 0 {
                        (offset, fraction * extent)
                    } else {
                        (fraction * extent, offset)
                    };
                    normalize(std::array::from_fn(|i| {
                        center[i] + (along[i] * x + across[i] * y) / world.config.radius_m
                    }))
                };
                let mut lo = -extent;
                let mut previous = world.sample(at(lo));
                observe_depth(&previous, at(lo));
                total_samples += 1;
                for step in 1..=160 {
                    let hi = extent * (-1.0 + 2.0 * step as f64 / 160.0);
                    let current = world.sample(at(hi));
                    observe_depth(&current, at(hi));
                    total_samples += 1;
                    if state(&previous) != state(&current) {
                        let mut left = lo;
                        let mut right = hi;
                        let old_state = state(&previous);
                        for _ in 0..32 {
                            if right - left < 0.001 {
                                break;
                            }
                            let middle = 0.5 * (left + right);
                            if state(&world.sample(at(middle))) == old_state {
                                left = middle;
                            } else {
                                right = middle;
                            }
                        }
                        let a = world.sample(at(left));
                        let b = world.sample(at(right));
                        observe_depth(&a, at(left));
                        observe_depth(&b, at(right));
                        let point = json!({"case_index":case_index,"from":from,"t":t,"axis":axis,"cross_offset_fraction":fraction,"direction":at(0.5*(left+right)),"before_direction":at(left),"after_direction":at(right),"pair_spacing_m":right-left});
                        water_edges.observe(&a, &b, point.clone());
                        let jump = f64::from((a.height_m - b.height_m).abs());
                        local_max_height = local_max_height.max(jump);
                        let details = json!({"point":point,"body_before":a.water_body_id,"body_after":b.water_body_id,"height_before_m":a.height_m,"height_after_m":b.height_m,"water_before_m":a.water_m,"water_after_m":b.water_m,"river_before":a.river,"river_after":b.river,"wet_before":state(&a).1,"wet_after":state(&b).1});
                        if jump > 0.1 {
                            height_jumps_over_10cm += 1;
                            if height_cases.len() < 64 {
                                height_cases.push(details.clone());
                            }
                        }
                        if jump > maximum_height_jump {
                            maximum_height_jump = jump;
                            worst_height = details;
                        }
                    }
                    lo = hi;
                    previous = current;
                }
            }
        }
        per_case.push(json!({"case_index":case_index,"from":from,"t":t,"extent_half_width_m":extent,"owner_or_wetness_boundaries":water_edges.pairs-initial_pairs,"wet_owner_jumps_over_10cm":water_edges.jumps_over_10cm-initial_jumps,"max_height_difference_m":local_max_height}));
    }
    json!({"mismatch_positions_checked":cases.len(),"scan_samples":total_samples,"water_owner_boundaries":water_edges.report(),"height_jumps_over_10cm":height_jumps_over_10cm,"max_height_difference_m":maximum_height_jump,"worst_height_case":worst_height,"large_height_cases":height_cases,"per_case":per_case,"max_river_depth_m":max_river_depth,"max_depth_above_owner_nominal_m":max_depth_excess,"worst_depth_case":worst_depth,
      "interpretation":"Local scans along/across the ten mismatched major-reach sample positions, bisecting owner or wetness changes to under1mm. Repeated neighbourhoods are not independent; no claim about the rest of the planet."})
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let started = Instant::now();
    let world = if let Some(path) = args.first().filter(|s| s.ends_with(".json")) {
        serde_json::from_reader(BufReader::new(File::open(path)?)).context("load world atlas")?
    } else {
        World::generate(WorldConfig {
            seed: 2_386_084_681,
            resolution: args.first().map(|s| s.parse()).transpose()?.unwrap_or(128),
            erosion_steps: 25,
            ..Default::default()
        })?
    };
    let prepare_ms = started.elapsed().as_secs_f64() * 1000.0;
    let catalogue_started = Instant::now();
    let _ = world.river_reach(0);
    let catalogue_ms = catalogue_started.elapsed().as_secs_f64() * 1000.0;
    // Keep the original failure neighbourhoods observable after a fix removes
    // their profile mismatches. This optional input only affects diagnostics.
    let overlap_reference: Option<Value> = args
        .iter()
        .skip(1)
        .find_map(|arg| arg.strip_prefix("--overlap-reference="))
        .map(|path| -> Result<Value> {
            Ok(serde_json::from_reader(BufReader::new(File::open(path)?))?)
        })
        .transpose()?;
    let coast = world
        .cells
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            !c.ocean
                && !c.lake
                && neighbours(*i, world.config.resolution)
                    .iter()
                    .any(|&j| world.cells[j].ocean)
        })
        .min_by(|(_, a), (_, b)| a.height_m.abs().total_cmp(&b.height_m.abs()))
        .map(|(_, c)| c.position)
        .unwrap_or([1.0, 0.0, 0.0]);
    let analysis_started = Instant::now();
    let rivers = river_profiles(&world);
    let river_overlaps = river_overlap_boundaries(
        &world,
        overlap_reference
            .as_ref()
            .map(|reference| &reference["rivers"])
            .unwrap_or(&rivers),
    );
    eprintln!(
        "river profile analysis complete, outside lakes/ocean: {}",
        rivers["deepest_river_case"]
    );
    let edges = lake_edges(&world);
    let actual_edges = warped_water_edges(&world);
    eprintln!(
        "warped water owner analysis complete: {}",
        actual_edges["owner_boundaries"]
    );
    let lake_lift = lake_reconstruction_lift(&world);
    let nested = parent_child(&world, coast)?;
    let initial_center = parent_child(
        &world,
        [0.568495595734618, 0.34289780745545134, 0.7478193974968728],
    )?;
    let grids = coast_grids(&world, coast);
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"world_id":world.id,"seed":world.config.seed,"resolution":world.config.resolution,"prepare_ms":prepare_ms,"catalogue_compile_ms":catalogue_ms,"analysis_ms":analysis_started.elapsed().as_secs_f64()*1000.0,"stats":world.stats,"rivers":rivers,"river_overlap_boundaries":river_overlaps,"lake_support_edges":edges,"warped_water_boundaries":actual_edges,"lake_reconstruction_lift":lake_lift,"parent_child":nested,"initial_center_parent_child":initial_center,"coast":grids})
        )?
    );
    Ok(())
}
