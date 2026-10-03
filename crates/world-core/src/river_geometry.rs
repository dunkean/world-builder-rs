//! Shared hydraulic nodes and curved reaches for both terrain carving and display.
//! Marine reaches terminate at a shoreline, never at the submarine atlas bed.
use crate::{
    geometry::{dot, normalize},
    hydrology::neighbours8,
    World,
};
#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;
use std::collections::{HashMap, VecDeque};

pub const MIN_RIVER_DISCHARGE_M3_S: f64 = 120.0;

#[derive(Clone, Debug)]
pub struct RiverReach {
    pub from: usize,
    pub to: usize,
    pub start: [f64; 3],
    pub end: [f64; 3],
    pub start_level_m: f64,
    pub end_level_m: f64,
    pub discharge_m3_s: f64,
    pub width_start_m: f64,
    pub width_end_m: f64,
    pub depth_start_m: f64,
    pub depth_end_m: f64,
    pub marine_mouth: bool,
    pub lake_inlet: bool,
    pub lake_outlet: bool,
    start_controlled: bool,
    end_controlled: bool,
    /// Smooth tidal widening near a low-gradient marine mouth; 1 for other reaches.
    pub estuary_factor: f64,
    pub estuary_length_m: f64,
    raw_start: [f64; 3],
    raw_end: [f64; 3],
    coefficients: [[f64; 3]; 3],
    start_parameter: f64,
    end_parameter: f64,
    // Fixed spatial samples, shared by terrain carving and the display ribbon.
    level_profile: Option<Box<[f64]>>,
    radius_m: f64,
}

fn hydraulic_level(world: &World, index: usize) -> f64 {
    let cell = &world.cells[index];
    if cell.ocean {
        0.0
    } else if cell.lake {
        f64::from(cell.water_m)
    } else {
        f64::from(cell.height_m)
    }
}

pub(crate) fn width(world: &World, discharge: f64) -> f64 {
    (discharge.sqrt() * 1.8 * (9.81 / world.config.gravity_m_s2).powf(0.25)).clamp(8.0, 2500.0)
}

fn depth(world: &World, discharge: f64) -> f64 {
    ((0.35 + discharge.powf(0.3) * 0.15) * (3.0 / world.config.gravity_m_s2).sqrt())
        .clamp(0.5, 18.0)
}

fn tangent(world: &World, index: usize, position: &mut impl FnMut(usize) -> [f64; 3]) -> [f64; 3] {
    let cell = &world.cells[index];
    let incoming = neighbours8(index, world.config.resolution)
        .into_iter()
        .filter(|&i| world.cells[i].receiver == Some(index))
        .max_by(|&a, &b| {
            world.cells[a]
                .discharge
                .total_cmp(&world.cells[b].discharge)
                .then(b.cmp(&a))
        });
    let center = position(index);
    let before = incoming.map_or(center, &mut *position);
    let after = cell.receiver.map_or(center, &mut *position);
    let mut t = std::array::from_fn(|i| after[i] - before[i]);
    let radial = dot(t, center);
    for i in 0..3 {
        t[i] -= radial * center[i];
    }
    if dot(t, t) < 1e-24 {
        return [0.0; 3];
    }
    normalize(t)
}

/// A lake contributes an outgoing reach only at its actual open hydraulic control.
/// Uphill potential spill paths are not invented physical rivers.
pub fn reach(world: &World, index: usize) -> Option<RiverReach> {
    let cell = world.cells.get(index)?;
    if cell.ocean {
        return None;
    }
    let to = cell.receiver?;
    let outlet = world
        .lakes
        .iter()
        .find(|lake| lake.control_cell == index && lake.outlet == Some(to));
    let controlled = world
        .lakes
        .iter()
        .any(|lake| lake.outlet == Some(index) && lake.outflow_m3_s > 0.0);
    reach_with_outlet(world, index, outlet, controlled, &mut |i| {
        crate::coast::inverse(&world.config, world.cells[i].position)
    })
}

fn reach_with_outlet(
    world: &World,
    index: usize,
    outlet: Option<&crate::hydrology::Lake>,
    controlled: bool,
    position: &mut impl FnMut(usize) -> [f64; 3],
) -> Option<RiverReach> {
    let cell = world.cells.get(index)?;
    if cell.ocean {
        return None;
    }
    let to = cell.receiver?;
    let next = world.cells.get(to)?;
    if cell.lake && outlet.is_none() {
        return None;
    }
    let discharge = outlet.map_or(f64::from(cell.discharge), |lake| lake.outflow_m3_s);
    if discharge <= 0.0 || (discharge < MIN_RIVER_DISCHARGE_M3_S && outlet.is_none() && !controlled)
    {
        return None;
    }
    let start_level = outlet.map_or_else(
        || hydraulic_level(world, index),
        |lake| f64::from(lake.level_m),
    );
    let end_level = hydraulic_level(world, to);
    if end_level > start_level + 0.002 {
        return None;
    }
    let downstream = if next.ocean || next.lake {
        discharge
    } else {
        f64::from(next.discharge).max(discharge)
    };
    let end_parameter = if next.ocean || next.lake {
        ((start_level - end_level).max(0.0) / (start_level - f64::from(next.height_m)).max(1e-9))
            .clamp(1e-6, 1.0)
    } else {
        1.0
    };
    let start = position(index);
    let end = position(to);
    let ta = tangent(world, index, position);
    let tb = tangent(world, to, position);
    let chord = std::array::from_fn(|i| end[i] - start[i]);
    let length = dot(chord, chord).sqrt();
    let coefficients = [
        std::array::from_fn(|i| length * ta[i]),
        std::array::from_fn(|i| 3.0 * chord[i] - length * (2.0 * ta[i] + tb[i])),
        std::array::from_fn(|i| -2.0 * chord[i] + length * (ta[i] + tb[i])),
    ];
    let mut result = RiverReach {
        from: index,
        to,
        start,
        end,
        start_level_m: start_level,
        end_level_m: end_level.min(start_level),
        discharge_m3_s: discharge,
        width_start_m: width(world, discharge),
        width_end_m: width(world, downstream),
        depth_start_m: depth(world, discharge),
        depth_end_m: depth(world, downstream),
        marine_mouth: next.ocean,
        lake_inlet: next.lake,
        lake_outlet: outlet.is_some(),
        start_controlled: outlet.is_some(),
        end_controlled: next.ocean || next.lake,
        estuary_factor: 1.0,
        estuary_length_m: 0.0,
        raw_start: start,
        raw_end: end,
        coefficients,
        start_parameter: 0.0,
        end_parameter,
        level_profile: None,
        radius_m: world.config.radius_m,
    };
    result.end = result.position(1.0);
    result.update_estuary();
    Some(result)
}

/// Compile the immutable catalogue once, before tile sampling. The callback
/// samples the same natural terrain used by the renderer, without river carving.
pub fn compile(world: &World) -> Vec<Option<Box<RiverReach>>> {
    let outlets: HashMap<_, _> = world
        .lakes
        .iter()
        .filter(|lake| lake.outlet.is_some())
        .map(|lake| (lake.control_cell, lake))
        .collect();
    let mut spill_controls: HashMap<usize, f64> = HashMap::new();
    for lake in &world.lakes {
        let Some(index) = lake.outlet else {
            continue;
        };
        let Some(cell) = world.cells.get(index) else {
            continue;
        };
        if lake.outflow_m3_s > 0.0 && !cell.ocean && !cell.lake {
            spill_controls
                .entry(index)
                .and_modify(|level| *level = level.max(f64::from(lake.level_m)))
                .or_insert(f64::from(lake.level_m));
        }
    }
    let natural = |p| crate::sampler::natural_height(world, p);
    // The continuous terrain is evaluated at T(p). Its discrete drainage nodes
    // therefore occupy T^-1(atlas_position) in the displayed sphere as well.
    // Cache only nodes touched by real reaches and their tangent neighbours.
    let mut mapped_positions = HashMap::new();
    let mut position = |index: usize| {
        *mapped_positions
            .entry(index)
            .or_insert_with(|| crate::coast::inverse(&world.config, world.cells[index].position))
    };
    let mut channels: Vec<_> = (0..world.cells.len())
        .map(|index| {
            let cell = &world.cells[index];
            let outlet = outlets
                .get(&index)
                .copied()
                .filter(|lake| lake.outlet == cell.receiver);
            if cell.ocean
                || (outlet.is_none()
                    && !spill_controls.contains_key(&index)
                    && f64::from(cell.discharge) < MIN_RIVER_DISCHARGE_M3_S)
                || outlet.is_some_and(|lake| lake.outflow_m3_s <= 0.0)
            {
                return None;
            }
            let mut channel = reach_with_outlet(
                world,
                index,
                outlet,
                spill_controls.contains_key(&index),
                &mut position,
            )?;
            if channel.marine_mouth || channel.lake_inlet {
                channel.clip_mouth(natural);
            }
            if channel.lake_outlet {
                channel.clip_outlet(natural);
            }
            Some(Box::new(channel))
        })
        .collect();
    fit_profiles_with_controls(&mut channels, natural, &spill_controls);
    fit_neighbouring_valleys(world, &mut channels);
    channels
}

/// Tributaries can share an end node yet overlap upstream at different heights.
/// Fit their free surfaces to the same already-incised neighbourhood, instead
/// of letting a hydraulic owner's switch replace the continuous terrain field.
/// Read a complete immutable pass before publishing the next one; native and
/// WASM therefore use the same arithmetic regardless of thread scheduling.
fn fit_neighbouring_valleys(world: &World, channels: &mut [Option<Box<RiverReach>>]) {
    let active: Vec<_> = channels
        .iter()
        .enumerate()
        .filter_map(|(index, reach)| reach.as_ref().map(|_| index))
        .collect();
    for _ in 0..3 {
        let fitted = |&index: &usize| {
            let reach = channels[index].as_ref().unwrap();
            let old = reach.level_profile.as_deref().unwrap();
            let mut profile = old.to_vec();
            let segments = profile.len() - 1;
            for (j, value) in profile.iter_mut().enumerate().take(segments).skip(1) {
                let t = j as f64 / segments as f64;
                let (center, along, _) = reach.derivatives(t);
                let across = normalize(crate::geometry::cross(center, along));
                let half_width = reach.width(t) * 0.5 / world.config.radius_m;
                for side in [-1.0, 0.0, 1.0] {
                    let point = normalize(std::array::from_fn(|axis| {
                        center[axis] + side * half_width * across[axis]
                    }));
                    let (nearby, count, spacing) =
                        crate::sampler::river_neighbourhood(world, point);
                    let mut natural = None;
                    for &other_index in &nearby[..count] {
                        if other_index == index {
                            continue;
                        }
                        let Some(other) = channels[other_index].as_deref() else {
                            continue;
                        };
                        let natural = *natural
                            .get_or_insert_with(|| crate::sampler::natural_height(world, point));
                        if let Some(cap) = crate::sampler::other_river_surface(
                            world, other, point, natural, spacing,
                        ) {
                            // The downstream endpoint is also the base level
                            // for this reach; fixed marine/lake planes and all
                            // shared hydraulic nodes must remain unchanged.
                            *value = value.min(cap.max(reach.end_level_m));
                        }
                    }
                }
            }
            let mut ceiling = reach.start_level_m;
            for value in profile.iter_mut().take(segments).skip(1) {
                ceiling = ceiling.min(*value);
                *value = ceiling.max(reach.end_level_m);
            }
            profile[0] = reach.start_level_m;
            profile[segments] = reach.end_level_m;
            (index, profile.into_boxed_slice())
        };
        #[cfg(not(target_arch = "wasm32"))]
        let fitted: Vec<_> = active.par_iter().map(fitted).collect();
        #[cfg(target_arch = "wasm32")]
        let fitted: Vec<_> = active.iter().map(fitted).collect();
        for (index, profile) in fitted {
            channels[index].as_mut().unwrap().level_profile = Some(profile);
        }
    }
}

/// A fixed-resolution spatial profile, independent of the camera or tile level.
/// Fit under the uncarved terrain plus a fraction of the hydraulic depth, then
/// propagate its low points downstream. This prevents linear atlas water levels
/// from floating above reconstructed valleys; no terrain is raised to hide it.
#[cfg(test)]
fn fit_profiles(channels: &mut [Option<Box<RiverReach>>], height: impl Fn([f64; 3]) -> f64 + Sync) {
    fit_profiles_with_controls(channels, height, &HashMap::new());
}

fn fit_profiles_with_controls(
    channels: &mut [Option<Box<RiverReach>>],
    height: impl Fn([f64; 3]) -> f64 + Sync,
    spill_controls: &HashMap<usize, f64>,
) {
    // A lake's external spill receiver is a hydraulic control too. An upstream
    // tributary cannot lower that crest merely because its interpolated course
    // crosses a local hollow: it meets the same reservoir/backwater plane.
    for reach in channels.iter_mut().flatten() {
        if let Some(&level) = spill_controls.get(&reach.from) {
            reach.start_level_m = level;
            reach.start_controlled = true;
        }
        if let Some(&level) = spill_controls.get(&reach.to) {
            reach.end_level_m = level;
            reach.end_controlled = true;
            if !reach.lake_outlet {
                reach.lake_inlet = true;
                reach.clip_mouth(&height);
            }
        }
    }
    const SEGMENTS: usize = 128;
    let mut indegree = vec![0u32; channels.len()];
    let mut levels = vec![f64::INFINITY; channels.len()];
    let sample_profile = |reach: &mut RiverReach| {
        reach.level_profile = Some(
            (0..=SEGMENTS)
                .map(|i| {
                    let t = i as f64 / SEGMENTS as f64;
                    height(reach.position(t)) + 0.35 * reach.depth(t)
                })
                .collect(),
        );
    };
    #[cfg(not(target_arch = "wasm32"))]
    channels
        .par_iter_mut()
        .filter_map(Option::as_mut)
        .for_each(|reach| sample_profile(reach.as_mut()));
    #[cfg(target_arch = "wasm32")]
    channels
        .iter_mut()
        .flatten()
        .for_each(|reach| sample_profile(reach.as_mut()));
    for reach in channels.iter().flatten() {
        let profile = reach.level_profile.as_deref().unwrap();
        levels[reach.from] = levels[reach.from].min(profile[0].min(reach.start_level_m));
        if !(reach.marine_mouth || reach.lake_inlet) {
            levels[reach.to] = levels[reach.to].min(profile[SEGMENTS].min(reach.end_level_m));
            indegree[reach.to] += 1;
        }
    }
    // Lake controls are fixed reservoir planes, not fitted terrain samples.
    for reach in channels.iter().flatten().filter(|r| r.lake_outlet) {
        levels[reach.from] = reach.start_level_m;
    }
    for (&index, &level) in spill_controls {
        levels[index] = level;
    }
    let mut queue: VecDeque<_> = channels
        .iter()
        .enumerate()
        .filter_map(|(i, r)| (r.is_some() && indegree[i] == 0).then_some(i))
        .collect();
    let mut order = Vec::new();
    while let Some(i) = queue.pop_front() {
        order.push(i);
        let reach = channels[i].as_ref().unwrap();
        if !(reach.marine_mouth || reach.lake_inlet) {
            indegree[reach.to] -= 1;
            if indegree[reach.to] == 0 && channels[reach.to].is_some() {
                queue.push_back(reach.to);
            }
        }
    }
    // Drainage is acyclic. A corrupt saved graph must not expose an unfitted
    // cyclic water profile or loop forever while building the catalogue.
    for (i, incoming) in indegree.iter().enumerate() {
        if *incoming != 0 && channels[i].is_some() {
            channels[i] = None;
        }
    }
    let mut floors = vec![f64::NEG_INFINITY; channels.len()];
    let mut marine_sink = vec![false; channels.len()];
    for &i in order.iter().rev() {
        let reach = channels[i].as_ref().unwrap();
        floors[i] = if reach.marine_mouth || reach.lake_inlet {
            reach.end_level_m
        } else if let Some(&level) = spill_controls.get(&reach.to) {
            level
        } else {
            floors[reach.to]
        };
        marine_sink[i] = if reach.marine_mouth || reach.lake_inlet {
            reach.marine_mouth
        } else {
            marine_sink[reach.to]
        };
    }
    // A reservoir plane is a backwater boundary, not permission to draw water
    // tens of metres above an unrelated reconstructed river bed. Find the first
    // real shoreline even when the discrete receiver is still an atlas land cell.
    // A reach wholly inside that water plane has no separate visible channel.
    for &i in &order {
        let floor = floors[i];
        if !floor.is_finite() {
            continue;
        }
        let reach = channels[i].as_mut().unwrap();
        let reservoir_neck = reach.lake_outlet && spill_controls.contains_key(&reach.to);
        if !reservoir_neck && height(reach.start) < floor - 0.002 {
            channels[i] = None;
            continue;
        }
        let intersects = reach
            .level_profile
            .as_deref()
            .unwrap()
            .iter()
            .enumerate()
            .any(|(j, h)| *h - 0.35 * reach.depth(j as f64 / SEGMENTS as f64) < floor - 1e-6);
        if intersects && !(reach.marine_mouth || reach.lake_inlet || reservoir_neck) {
            reach.marine_mouth = marine_sink[i];
            reach.lake_inlet = !marine_sink[i];
            reach.end_level_m = floor;
            reach.clip_mouth(&height);
            sample_profile(reach);
        }
    }
    order.retain(|&i| channels[i].is_some());
    levels.fill(f64::INFINITY);
    for reach in channels.iter().flatten() {
        let profile = reach.level_profile.as_deref().unwrap();
        levels[reach.from] = levels[reach.from].min(profile[0].min(reach.start_level_m));
        if !(reach.marine_mouth || reach.lake_inlet) {
            levels[reach.to] = levels[reach.to].min(profile[SEGMENTS].min(reach.end_level_m));
        }
    }
    for reach in channels.iter().flatten().filter(|r| r.lake_outlet) {
        levels[reach.from] = reach.start_level_m;
    }
    for (&index, &level) in spill_controls {
        levels[index] = level;
    }
    for &i in &order {
        let reach = channels[i].as_ref().unwrap();
        let start = levels[i].max(floors[i]);
        levels[i] = start;
        let ceiling = reach
            .level_profile
            .as_deref()
            .unwrap()
            .iter()
            .copied()
            .fold(start, f64::min)
            .max(floors[i]);
        if !(reach.marine_mouth || reach.lake_inlet) && !spill_controls.contains_key(&reach.to) {
            levels[reach.to] = levels[reach.to].min(ceiling);
        }
    }
    for &i in &order {
        let reach = channels[i].as_mut().unwrap();
        reach.start_level_m = levels[i];
        reach.end_level_m = if reach.marine_mouth || reach.lake_inlet {
            reach.end_level_m
        } else {
            levels[reach.to]
        };
        let mut ceiling = reach.start_level_m;
        let chord = std::array::from_fn(|axis| reach.end[axis] - reach.start[axis]);
        let length = dot(chord, chord).sqrt() * reach.radius_m;
        // The whole wet section must join its receiving plane. A centreline-only
        // crossing leaves upstream ribbon flanks above the adjacent lake water.
        let join = if reach.lake_inlet || reach.marine_mouth {
            let grade = (reach.start_level_m - reach.end_level_m).max(0.0) / length.max(1.0);
            // On nearly level ground the lake contacts the channel's flanks
            // long before its centreline reaches the geometric mouth. A depth /
            // grade backwater scale covers that shallow hydraulic transition.
            let backwater =
                (3.0 * reach.width_end_m).max(2.0 * reach.depth_end_m / grade.max(1e-6));
            1.0 - (backwater / length.max(1.0)).min(0.8)
        } else {
            1.0
        };
        let controlled = reach.start_controlled || reach.end_controlled;
        let profile = reach.level_profile.as_deref_mut().unwrap();
        for (j, value) in profile.iter_mut().enumerate() {
            ceiling = ceiling.min((*value).max(floors[i]));
            let t = j as f64 / SEGMENTS as f64;
            let u = (t / join).clamp(0.0, 1.0);
            let u = if controlled { smooth(u) } else { u };
            let endpoint_ramp = reach.start_level_m * (1.0 - u) + reach.end_level_m * u;
            *value = ceiling.min(endpoint_ramp);
        }
        profile[0] = reach.start_level_m;
        profile[SEGMENTS] = reach.end_level_m;
        reach.update_estuary();
    }
}

impl RiverReach {
    fn update_estuary(&mut self) {
        let base = self.width_end_m / self.estuary_factor;
        let chord = std::array::from_fn(|i| self.end[i] - self.start[i]);
        let length = dot(chord, chord).sqrt() * self.radius_m;
        let slope = (self.start_level_m - self.end_level_m).max(0.0) / length.max(1.0);
        let flat = smooth(((0.01 - slope) / 0.008).clamp(0.0, 1.0));
        self.estuary_factor = if self.marine_mouth {
            1.0 + 0.55 * flat
        } else {
            1.0
        };
        self.width_end_m = base * self.estuary_factor;
        self.estuary_length_m = if self.estuary_factor > 1.0 {
            (700.0 * (base / 30.0).sqrt()).min(length * 0.4)
        } else {
            0.0
        };
    }
    fn raw_position(&self, t: f64) -> [f64; 3] {
        if t <= 0.0 {
            return self.raw_start;
        }
        if t >= 1.0 {
            return self.raw_end;
        }
        normalize(std::array::from_fn(|i| {
            self.raw_start[i]
                + t * (self.coefficients[0][i]
                    + t * (self.coefficients[1][i] + t * self.coefficients[2][i]))
        }))
    }
    fn derivatives(&self, t: f64) -> ([f64; 3], [f64; 3], [f64; 3]) {
        let span = self.end_parameter - self.start_parameter;
        let t = self.start_parameter + t * span;
        let r = std::array::from_fn(|i| {
            self.raw_start[i]
                + t * (self.coefficients[0][i]
                    + t * (self.coefficients[1][i] + t * self.coefficients[2][i]))
        });
        let dr = std::array::from_fn(|i| {
            (self.coefficients[0][i]
                + 2.0 * t * self.coefficients[1][i]
                + 3.0 * t * t * self.coefficients[2][i])
                * span
        });
        let ddr = std::array::from_fn(|i| {
            (2.0 * self.coefficients[1][i] + 6.0 * t * self.coefficients[2][i]) * span * span
        });
        let inv = dot(r, r).sqrt().recip();
        let inv2 = inv * inv;
        let radial = dot(r, dr) * inv2;
        let curvature = 3.0 * radial * radial - (dot(dr, dr) + dot(r, ddr)) * inv2;
        (
            r.map(|v| v * inv),
            std::array::from_fn(|i| (dr[i] - r[i] * radial) * inv),
            std::array::from_fn(|i| (ddr[i] - 2.0 * dr[i] * radial + r[i] * curvature) * inv),
        )
    }
    pub fn position(&self, t: f64) -> [f64; 3] {
        self.raw_position(
            self.start_parameter + t.clamp(0.0, 1.0) * (self.end_parameter - self.start_parameter),
        )
    }
    pub fn level(&self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        let Some(profile) = self.level_profile.as_deref() else {
            return self.start_level_m * (1.0 - t) + self.end_level_m * t;
        };
        if t == 1.0 {
            return self.end_level_m;
        }
        let x = t * (profile.len() - 1) as f64;
        let i = x.floor() as usize;
        let u = x - i as f64;
        let delta = profile[i + 1] - profile[i];
        let slope = |a: f64, b: f64| {
            if a * b <= 0.0 {
                0.0
            } else {
                2.0 * a * b / (a + b)
            }
        };
        let left = if i == 0 {
            if self.start_controlled {
                0.0
            } else {
                delta
            }
        } else {
            slope(profile[i] - profile[i - 1], delta)
        };
        let right = if i + 2 == profile.len() {
            if self.end_controlled {
                0.0
            } else {
                delta
            }
        } else {
            slope(delta, profile[i + 2] - profile[i + 1])
        };
        let u2 = u * u;
        let u3 = u2 * u;
        (2.0 * u3 - 3.0 * u2 + 1.0) * profile[i]
            + (u3 - 2.0 * u2 + u) * left
            + (-2.0 * u3 + 3.0 * u2) * profile[i + 1]
            + (u3 - u2) * right
    }
    pub fn width(&self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        if t == 0.0 {
            return self.width_start_m;
        }
        if t == 1.0 {
            return self.width_end_m;
        }
        let baseline = self.width_start_m
            + ((self.width_end_m / self.estuary_factor).max(self.width_start_m)
                - self.width_start_m)
                * t;
        if self.estuary_length_m <= 0.0 {
            return baseline;
        }
        let chord = std::array::from_fn(|i| self.end[i] - self.start[i]);
        let distance_to_mouth = dot(chord, chord).sqrt() * self.radius_m * (1.0 - t);
        baseline
            * (1.0
                + (self.estuary_factor - 1.0)
                    * smooth(1.0 - distance_to_mouth / self.estuary_length_m))
    }
    pub fn depth(&self, t: f64) -> f64 {
        self.depth_start_m * (1.0 - t.clamp(0.0, 1.0)) + self.depth_end_m * t.clamp(0.0, 1.0)
    }

    /// Refine the endpoint using the frozen natural terrain field, without carving.
    /// Compile once per world/reach; doing this on every terrain sample is expensive.
    pub fn clip_mouth(&mut self, height: impl Fn([f64; 3]) -> f64) {
        if !(self.marine_mouth || self.lake_inlet) || height(self.start) <= self.end_level_m {
            return;
        }
        let point =
            |t: f64| self.raw_position(self.start_parameter + t * (1.0 - self.start_parameter));
        // First descending crossing keeps an inland river from crossing an island.
        let mut lo = 0.0;
        let mut hi = None;
        for step in 1..=128 {
            let t = f64::from(step) / 128.0;
            if height(point(t)) <= self.end_level_m {
                hi = Some(t);
                break;
            }
            lo = t;
        }
        let Some(mut hi) = hi else {
            return;
        };
        for _ in 0..32 {
            let t = 0.5 * (lo + hi);
            if height(point(t)) > self.end_level_m {
                lo = t;
            } else {
                hi = t;
            }
        }
        self.end_parameter = self.start_parameter + 0.5 * (lo + hi) * (1.0 - self.start_parameter);
        self.end = self.position(1.0);
        self.update_estuary();
    }

    /// Start an open reservoir's outlet at the last ascending shoreline before
    /// its hydraulic control. Earlier islands do not drain the reservoir: using
    /// their first shoreline can leave a flat high-water neck over a later hollow.
    fn clip_outlet(&mut self, height: impl Fn([f64; 3]) -> f64) {
        if !self.lake_outlet {
            return;
        }
        let point = |t: f64| self.raw_position(t * self.end_parameter);
        let mut previous_t = 0.0;
        let mut previous_height = height(point(0.0));
        let mut crossing = None;
        for step in 1..=128 {
            let t = step as f64 / 128.0;
            let current_height = height(point(t));
            if previous_height < self.start_level_m && current_height >= self.start_level_m {
                crossing = Some((previous_t, t));
            }
            previous_t = t;
            previous_height = current_height;
        }
        let Some((mut lo, mut hi)) = crossing else {
            return;
        };
        for _ in 0..32 {
            let t = 0.5 * (lo + hi);
            if height(point(t)) < self.start_level_m {
                lo = t;
            } else {
                hi = t;
            }
        }
        self.start_parameter = 0.5 * (lo + hi) * self.end_parameter;
        self.start = self.position(0.0);
        self.update_estuary();
    }

    /// Smooth closest point rather than exposing the eight straight Hermite chords.
    pub fn closest(&self, p: [f64; 3]) -> (f64, f64) {
        let distance2 = |t: f64| {
            let q = self.position(t);
            dot(
                std::array::from_fn(|i| p[i] - q[i]),
                std::array::from_fn(|i| p[i] - q[i]),
            )
        };
        let mut best = 0;
        let mut minimum = f64::INFINITY;
        for i in 0..=8 {
            let d = distance2(f64::from(i) / 8.0);
            if d < minimum {
                minimum = d;
                best = i;
            }
        }
        let mut lo = f64::from((best - 1).max(0)) / 8.0;
        let mut hi = f64::from((best + 1).min(8)) / 8.0;
        // Safeguarded Newton projection uses the analytic normalised Hermite
        // derivatives: no visible polyline chords and no 48 extra curve evaluations.
        let mut t = f64::from(best) / 8.0;
        for _ in 0..12 {
            let (q, dq, ddq) = self.derivatives(t);
            let error = std::array::from_fn(|i| q[i] - p[i]);
            let gradient = dot(error, dq);
            let hessian = dot(dq, dq) + dot(error, ddq);
            if gradient < 0.0 {
                lo = t;
            } else {
                hi = t;
            }
            let candidate = t - gradient / hessian;
            let next =
                if hessian > 0.0 && candidate.is_finite() && candidate >= lo && candidate <= hi {
                    candidate
                } else {
                    0.5 * (lo + hi)
                };
            if (next - t).abs() < 1e-13 {
                break;
            }
            t = next;
        }
        let mut d = distance2(t);
        for endpoint in [0.0, 1.0] {
            let candidate = distance2(endpoint);
            if candidate < d {
                d = candidate;
                t = endpoint;
            }
        }
        (d.sqrt() * self.radius_m, t)
    }
}

fn smooth(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// C1 channel, banks, floodplain and valley sides. The wet support is exactly
/// half the width; every exterior point is at or above the hydraulic datum.
/// Combine with a smoothly bounded positive incision, rather than raising terrain.
pub fn bank_height(
    distance_m: f64,
    width_m: f64,
    depth_m: f64,
    water_level_m: f64,
    wall_slope: f64,
) -> f64 {
    let d = distance_m.max(0.0);
    let width = width_m.max(1.0);
    let half = 0.5 * width;
    if d <= half {
        let q = d / half;
        return water_level_m - depth_m.max(0.0) * (1.0 - q * q).powi(2);
    }
    let bank = 0.6 * width + 1.5;
    if d <= half + bank {
        return water_level_m + 0.35 * smooth((d - half) / bank);
    }
    let floodplain = 1.7 * width + 7.0;
    let easing = bank * 0.25;
    let rise = |x: f64| 0.012 * x * x / (x + easing);
    let edge = (floodplain - half - bank).max(0.0);
    let x = d - half - bank;
    if x <= edge {
        return water_level_m + 0.35 + rise(x);
    }
    let outside = x - edge;
    let derivative = 0.012 * edge * (edge + 2.0 * easing) / (edge + easing).powi(2);
    water_level_m
        + 0.35
        + rise(edge)
        + derivative * outside
        + wall_slope.max(0.0) * outside * outside / (outside + 30.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{geometry::cell_index, WorldConfig};
    fn synthetic() -> World {
        let mut world = World::generate(WorldConfig {
            resolution: 8,
            erosion_steps: 0,
            coast_detail_ratio: 0.0,
            ..Default::default()
        })
        .unwrap();
        world.lakes.clear();
        for cell in &mut world.cells {
            cell.ocean = false;
            cell.lake = false;
            cell.lake_id = None;
            cell.receiver = None;
            cell.discharge = 0.0;
            cell.height_m = 300.0;
            cell.water_m = 300.0;
        }
        world
    }
    #[test]
    fn coast_transport_keeps_hydraulic_nodes_and_confluences_in_the_same_frame() {
        let mut world = synthetic();
        world.config.coast_detail_ratio = 0.16;
        world.config.coast_reference_spacing_m = 1.0;
        let a = cell_index(4, 2, 2, 8);
        let b = cell_index(4, 3, 3, 8);
        let c = cell_index(4, 4, 3, 8);
        world.cells[a].height_m = 100.0;
        world.cells[a].discharge = 500.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].height_m = 70.0;
        world.cells[b].discharge = 2000.0;
        world.cells[b].receiver = Some(c);
        world.cells[c].height_m = 20.0;
        let atlas_a = world.cells[a].position;
        let atlas_b = world.cells[b].position;
        let upstream = reach(&world, a).unwrap();
        let downstream = reach(&world, b).unwrap();
        assert_eq!(upstream.end, downstream.start);
        for (position, atlas) in [(upstream.start, atlas_a), (upstream.end, atlas_b)] {
            let q = crate::coast::remap(&world.config, position);
            assert!(q.iter().zip(atlas).all(|(a, b)| (a - b).abs() < 1e-12));
        }
        let shift = std::array::from_fn(|i| upstream.start[i] - atlas_a[i]);
        assert!(dot(shift, shift).sqrt() * world.config.radius_m > 1.0);
        assert_eq!(
            world.cells[a].position, atlas_a,
            "physical atlas is immutable"
        );
        assert_eq!(world.cells[b].position, atlas_b);
        let (_, derivative, _) = upstream.derivatives(0.0);
        assert!(dot(upstream.start, derivative).abs() < 1e-14);
        assert_eq!(upstream.width(1.0), downstream.width(0.0));
        assert_eq!(upstream.level(1.0), downstream.level(0.0));
    }

    #[test]
    fn confluences_share_hydraulic_level_and_downstream_width() {
        let mut world = synthetic();
        let a = cell_index(4, 2, 2, 8);
        let b = cell_index(4, 3, 3, 8);
        let c = cell_index(4, 4, 3, 8);
        world.cells[a].height_m = 100.0;
        world.cells[a].discharge = 500.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].height_m = 70.0;
        world.cells[b].discharge = 2000.0;
        world.cells[b].receiver = Some(c);
        world.cells[c].height_m = 20.0;
        let upstream = reach(&world, a).unwrap();
        let downstream = reach(&world, b).unwrap();
        assert_eq!(upstream.end_level_m, downstream.start_level_m);
        assert_eq!(upstream.width_end_m, downstream.width_start_m);
        assert_eq!(upstream.depth_end_m, downstream.depth_start_m);
        assert_eq!(upstream.end, downstream.start);
        for i in 0..100 {
            assert!(
                upstream.level(f64::from(i + 1) / 100.0) <= upstream.level(f64::from(i) / 100.0)
            );
        }
    }
    #[test]
    fn marine_endpoint_uses_zero_level_and_stops_before_the_seabed() {
        let mut world = synthetic();
        let a = cell_index(4, 3, 3, 8);
        let b = cell_index(4, 4, 3, 8);
        world.cells[a].height_m = 100.0;
        world.cells[a].discharge = 1000.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].ocean = true;
        world.cells[b].height_m = -300.0;
        world.cells[b].water_m = 0.0;
        let mut channel = reach(&world, a).unwrap();
        assert_eq!(channel.end_level_m, 0.0);
        assert_ne!(channel.end, world.cells[b].position);
        let origin = world.cells[a].position;
        let end = world.cells[b].position;
        let delta = std::array::from_fn(|i| end[i] - origin[i]);
        let height = |p: [f64; 3]| {
            100.0
                - 400.0 * dot(std::array::from_fn(|i| p[i] - origin[i]), delta) / dot(delta, delta)
        };
        channel.clip_mouth(height);
        assert!(height(channel.end).abs() < 1e-6);
        assert_eq!(channel.position(0.0), origin);
        assert_eq!(channel.level(1.0), 0.0);
    }

    #[test]
    fn lake_inlet_ends_at_the_reservoir_shore_instead_of_its_deep_centre() {
        let mut world = synthetic();
        let a = cell_index(4, 3, 3, 8);
        let b = cell_index(4, 4, 3, 8);
        world.cells[a].height_m = 150.0;
        world.cells[a].discharge = 1000.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].lake = true;
        world.cells[b].lake_id = Some(17);
        world.cells[b].height_m = 50.0;
        world.cells[b].water_m = 100.0;
        let mut channel = reach(&world, a).unwrap();
        let origin = world.cells[a].position;
        let delta = std::array::from_fn(|i| world.cells[b].position[i] - origin[i]);
        let height = |p: [f64; 3]| {
            150.0
                - 100.0 * dot(std::array::from_fn(|i| p[i] - origin[i]), delta) / dot(delta, delta)
        };
        channel.clip_mouth(height);
        assert!(channel.lake_inlet);
        assert!(!channel.marine_mouth);
        assert!((height(channel.end) - 100.0).abs() < 1e-6);
        assert_eq!(channel.level(1.0), 100.0);
        assert_eq!(channel.estuary_factor, 1.0);
        assert_ne!(channel.end, world.cells[b].position);
    }
    #[test]
    fn smooth_curve_projection_is_precise_at_bank_scale() {
        let mut world = synthetic();
        let a = cell_index(4, 2, 2, 8);
        let b = cell_index(4, 3, 3, 8);
        world.cells[a].height_m = 100.0;
        world.cells[a].discharge = 1000.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].height_m = 20.0;
        let channel = reach(&world, a).unwrap();
        for i in 0..=20 {
            let t = f64::from(i) / 20.0;
            let (distance, recovered) = channel.closest(channel.position(t));
            assert!(distance < 1.0, "distance={distance}");
            assert!((t - recovered).abs() < 1e-4);
        }
    }

    #[test]
    fn curved_confluence_banks_project_without_polyline_corners() {
        let mut world = synthetic();
        let a = cell_index(4, 2, 2, 8);
        let b = cell_index(4, 3, 3, 8);
        let c = cell_index(4, 4, 3, 8);
        world.cells[a].height_m = 100.0;
        world.cells[a].discharge = 1000.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].height_m = 70.0;
        world.cells[b].discharge = 2000.0;
        world.cells[b].receiver = Some(c);
        let channel = reach(&world, a).unwrap();
        for step in 1..40 {
            let t = f64::from(step) / 40.0;
            let (q, dq, _) = channel.derivatives(t);
            let normal = normalize(crate::geometry::cross(q, dq));
            let angle = 50.0 / world.config.radius_m;
            let bank = normalize(std::array::from_fn(|i| {
                q[i] * angle.cos() + normal[i] * angle.sin()
            }));
            let (distance, recovered) = channel.closest(bank);
            assert!(
                (distance - 50.0).abs() < 0.01,
                "distance={distance} at t={t}"
            );
            assert!(
                (recovered - t).abs() < 1e-6,
                "projection changed param {t}→{recovered}"
            );
        }
    }

    #[test]
    fn river_cross_section_has_exact_shore_and_continuous_bank_slopes() {
        for width in [8.0, 50.0, 300.0] {
            let level = 120.0;
            let depth = 4.0;
            let bank = 0.6 * width + 1.5;
            assert_eq!(bank_height(0.0, width, depth, level, 0.24), level - depth);
            assert_eq!(bank_height(width * 0.5, width, depth, level, 0.24), level);
            for join in [width * 0.5, width * 0.5 + bank, 1.7 * width + 7.0] {
                let e = 1e-4;
                let h = bank_height(join, width, depth, level, 0.24);
                let left = (h - bank_height(join - e, width, depth, level, 0.24)) / e;
                let right = (bank_height(join + e, width, depth, level, 0.24) - h) / e;
                assert!((left - right).abs() < 1e-3, "slope discontinuity at {join}");
            }
            for step in 1..100 {
                let d = width * 0.5 + f64::from(step) * width / 10.0;
                assert!(bank_height(d, width, depth, level, 0.24) >= level);
            }
        }
    }

    #[test]
    fn fitted_water_follows_reconstructed_valley_and_keeps_confluences_shared() {
        let mut world = synthetic();
        let a = cell_index(4, 2, 2, 8);
        let b = cell_index(4, 3, 3, 8);
        let c = cell_index(4, 4, 3, 8);
        world.cells[a].height_m = 220.0;
        world.cells[a].discharge = 500.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].height_m = 180.0;
        world.cells[b].discharge = 2000.0;
        world.cells[b].receiver = Some(c);
        world.cells[c].height_m = 140.0;
        let mut channels = vec![None; world.cells.len()];
        channels[a] = reach(&world, a).map(Box::new);
        channels[b] = reach(&world, b).map(Box::new);
        let valley = channels[a].as_ref().unwrap().position(0.5);
        let height = |p: [f64; 3]| {
            let d = dot(
                std::array::from_fn(|i| p[i] - valley[i]),
                std::array::from_fn(|i| p[i] - valley[i]),
            );
            210.0 - 100.0 * (-d / 0.002).exp()
        };
        assert!(channels[a].as_ref().unwrap().level(0.5) - height(valley) > 50.0);
        fit_profiles(&mut channels, height);
        let upstream = channels[a].as_ref().unwrap();
        let downstream = channels[b].as_ref().unwrap();
        assert_eq!(upstream.level(1.0), downstream.level(0.0));
        assert_eq!(upstream.width(1.0), downstream.width(0.0));
        assert_eq!(upstream.depth(1.0), downstream.depth(0.0));
        for reach in [upstream, downstream] {
            for step in 0..=128 {
                let t = step as f64 / 128.0;
                assert!(reach.level(t) <= height(reach.position(t)) + reach.depth(t) * 0.35 + 1e-9);
            }
            for step in 0..1024 {
                assert!(
                    reach.level((step + 1) as f64 / 1024.0)
                        <= reach.level(step as f64 / 1024.0) + 1e-9
                );
            }
            // Uniform-knot monotone Hermite interpolation shares derivatives.
            let t = 0.5;
            let e = 1e-7;
            let left = (reach.level(t) - reach.level(t - e)) / e;
            let right = (reach.level(t + e) - reach.level(t)) / e;
            assert!((left - right).abs() < 1e-2);
        }
    }

    #[test]
    fn tributary_hollow_cannot_lower_a_lakes_external_spill_control() {
        let mut world = synthetic();
        let a = cell_index(4, 2, 3, 8);
        let b = cell_index(4, 3, 3, 8);
        let c = cell_index(4, 4, 3, 8);
        let tributary = cell_index(4, 3, 2, 8);
        let plane = 1219.0;
        world.cells[a].lake = true;
        world.cells[a].lake_id = Some(7);
        world.cells[a].height_m = 1191.0;
        world.cells[a].water_m = plane as f32;
        world.cells[a].discharge = 500.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].height_m = plane as f32;
        world.cells[b].discharge = 1500.0;
        world.cells[b].receiver = Some(c);
        world.cells[c].height_m = 1170.0;
        world.cells[tributary].height_m = 1230.0;
        world.cells[tributary].discharge = 1000.0;
        world.cells[tributary].receiver = Some(b);
        world.lakes.push(crate::hydrology::Lake {
            id: 7,
            level_m: plane as f32,
            spill_level_m: plane as f32,
            control_cell: a,
            outlet: Some(b),
            outflow_m3_s: 500.0,
            cells: vec![a],
            ..Default::default()
        });
        let mut channels = vec![None; world.cells.len()];
        for i in [a, b, tributary] {
            channels[i] = reach(&world, i).map(Box::new);
        }
        let hollow = channels[tributary].as_ref().unwrap().position(0.5);
        let height = |p: [f64; 3]| {
            let delta = std::array::from_fn(|i| p[i] - hollow[i]);
            1220.0 - 49.0 * (-dot(delta, delta) / 0.0005).exp()
        };
        let mut unprotected = channels.clone();
        fit_profiles(&mut unprotected, height);
        assert!(unprotected[b].as_ref().unwrap().level(0.0) < plane - 40.0);
        fit_profiles_with_controls(&mut channels, height, &HashMap::from([(b, plane)]));
        let outlet = channels[a].as_ref().unwrap();
        let outgoing = channels[b].as_ref().unwrap();
        assert_eq!(outlet.level(0.0), plane);
        assert_eq!(outlet.level(1.0), plane);
        assert_eq!(outgoing.level(0.0), plane);
        let inlet = channels[tributary].as_ref().unwrap();
        assert!(inlet.lake_inlet);
        assert_eq!(inlet.level(1.0), plane);
        assert!((height(inlet.end) - plane).abs() < 1e-6);
        for reach in [outlet, outgoing, inlet] {
            for step in 0..1024 {
                let t = step as f64 / 1024.0;
                assert!(reach.level(t + 1.0 / 1024.0) <= reach.level(t) + 1e-9);
                assert!(reach.level(t) <= height(reach.position(t)) + reach.depth(t) + 1e-3);
            }
        }
        assert!(
            (outgoing.level(1e-7) - plane).abs() < 1e-9,
            "water leaves the spill control with a horizontal tangent"
        );
    }

    #[test]
    fn inlet_water_surface_joins_the_receiver_plane_over_the_wet_section() {
        let mut world = synthetic();
        world.config.radius_m = 10_000.0;
        let a = cell_index(4, 3, 3, 8);
        let b = cell_index(4, 4, 3, 8);
        world.cells[a].height_m = 150.0;
        world.cells[a].discharge = 1000.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].lake = true;
        world.cells[b].height_m = 50.0;
        world.cells[b].water_m = 100.0;
        let mut channel = reach(&world, a).unwrap();
        let origin = channel.raw_start;
        let delta = std::array::from_fn(|i| channel.raw_end[i] - origin[i]);
        let height = |p: [f64; 3]| {
            150.0
                - 100.0 * dot(std::array::from_fn(|i| p[i] - origin[i]), delta) / dot(delta, delta)
        };
        channel.clip_mouth(height);
        let mut channels = vec![None; world.cells.len()];
        channels[a] = Some(Box::new(channel));
        fit_profiles(&mut channels, height);
        let channel = channels[a].as_ref().unwrap();
        for t in [0.9, 0.95, 1.0] {
            assert_eq!(channel.level(t), 100.0);
        }
        let delta = std::array::from_fn(|i| channel.end[i] - channel.start[i]);
        let length = dot(delta, delta).sqrt() * channel.radius_m;
        assert_eq!(channel.level(1.0 - channel.width_end_m / length), 100.0);
    }

    #[test]
    fn shallow_inlet_backwater_reaches_early_lateral_lake_contacts() {
        for (source_level, discharge) in [(101.0, 1000.0), (104.5, 8700.0)] {
            let mut world = synthetic();
            world.config.radius_m = 10_000.0;
            let a = cell_index(4, 3, 3, 8);
            let b = cell_index(4, 4, 3, 8);
            world.cells[a].height_m = source_level as f32;
            world.cells[a].discharge = discharge as f32;
            world.cells[a].receiver = Some(b);
            world.cells[b].lake = true;
            world.cells[b].height_m = 99.9;
            world.cells[b].water_m = 100.0;
            let mut channel = reach(&world, a).unwrap();
            let origin = channel.raw_start;
            let delta = std::array::from_fn(|i| channel.raw_end[i] - origin[i]);
            let height = |p: [f64; 3]| {
                source_level
                    - (source_level - 99.9) * dot(std::array::from_fn(|i| p[i] - origin[i]), delta)
                        / dot(delta, delta)
            };
            channel.clip_mouth(height);
            let mut channels = vec![None; world.cells.len()];
            channels[a] = Some(Box::new(channel));
            fit_profiles(&mut channels, height);
            let channel = channels[a].as_ref().unwrap();
            assert_eq!(
                channel.level(0.0),
                source_level,
                "shared upstream node stays fixed"
            );
            for t in [0.28, 0.35, 0.425, 0.8, 1.0] {
                assert_eq!(channel.level(t), 100.0);
            }
            for step in 0..1024 {
                let t = step as f64 / 1024.0;
                assert!(channel.level(t + 1.0 / 1024.0) <= channel.level(t) + 1e-9);
                assert!(channel.level(t) <= height(channel.position(t)) + channel.depth(t));
            }
        }
    }

    #[test]
    fn positive_lake_outflows_below_the_main_river_threshold_remain_visible() {
        let mut world = synthetic();
        let a = cell_index(4, 3, 3, 8);
        let b = cell_index(4, 4, 3, 8);
        world.cells[a].lake = true;
        world.cells[a].height_m = 90.0;
        world.cells[a].water_m = 100.0;
        world.cells[a].discharge = 0.25;
        world.cells[a].receiver = Some(b);
        world.cells[b].height_m = 100.0;
        world.cells[b].discharge = 0.25;
        world.cells[b].receiver = Some(cell_index(4, 5, 3, 8));
        world.cells[cell_index(4, 5, 3, 8)].height_m = 90.0;
        world.lakes.push(crate::hydrology::Lake {
            id: 7,
            level_m: 100.0,
            spill_level_m: 100.0,
            control_cell: a,
            outlet: Some(b),
            outflow_m3_s: 0.25,
            cells: vec![a],
            ..Default::default()
        });
        let outlet = reach(&world, a).unwrap();
        assert!(outlet.lake_outlet);
        assert_eq!(outlet.discharge_m3_s, 0.25);
        assert!(
            reach(&world, b).is_some(),
            "the weak outlet has a real outgoing crest section"
        );
        world.lakes[0].outflow_m3_s = 0.0;
        assert!(reach(&world, a).is_none());
    }

    #[test]
    fn fitted_profile_preserves_fixed_marine_and_reservoir_controls() {
        let mut world = synthetic();
        let a = cell_index(4, 3, 3, 8);
        let b = cell_index(4, 4, 3, 8);
        world.cells[a].height_m = 100.0;
        world.cells[a].discharge = 1000.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].ocean = true;
        world.cells[b].height_m = -300.0;
        let mut channel = reach(&world, a).unwrap();
        channel.lake_outlet = true;
        let length = std::array::from_fn(|i| channel.end[i] - channel.start[i]);
        let origin = channel.start;
        let height = |p: [f64; 3]| {
            let t = dot(std::array::from_fn(|i| p[i] - origin[i]), length) / dot(length, length);
            100.0 * (1.0 - t)
        };
        let mut channels = vec![None; world.cells.len()];
        channels[a] = Some(Box::new(channel));
        fit_profiles(&mut channels, height);
        let channel = channels[a].as_ref().unwrap();
        assert_eq!(channel.level(0.0), 100.0);
        assert_eq!(channel.level(1.0), 0.0);
        for step in 0..1024 {
            assert!(
                channel.level((step + 1) as f64 / 1024.0)
                    <= channel.level(step as f64 / 1024.0) + 1e-9
            );
            assert!(channel.level(step as f64 / 1024.0) >= 0.0);
        }
    }

    #[test]
    fn backwater_clips_before_a_land_receiver_below_the_lake_plane() {
        let mut world = synthetic();
        let a = cell_index(4, 2, 3, 8);
        let b = cell_index(4, 3, 3, 8);
        let c = cell_index(4, 4, 3, 8);
        world.cells[a].height_m = 300.0;
        world.cells[a].discharge = 1000.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].height_m = 200.0;
        world.cells[b].discharge = 2000.0;
        world.cells[b].receiver = Some(c);
        world.cells[c].lake = true;
        world.cells[c].height_m = 50.0;
        world.cells[c].water_m = 100.0;
        let origin = world.cells[a].position;
        let delta = std::array::from_fn(|i| world.cells[b].position[i] - origin[i]);
        let height = |p: [f64; 3]| {
            let t = dot(std::array::from_fn(|i| p[i] - origin[i]), delta) / dot(delta, delta);
            300.0 - 210.0 * t
        };
        let mut channels = vec![None; world.cells.len()];
        channels[a] = reach(&world, a).map(Box::new);
        channels[b] = reach(&world, b).map(Box::new);
        fit_profiles(&mut channels, height);
        assert!(
            channels[b].is_none(),
            "submerged land receiver has no separate visible river"
        );
        let channel = channels[a].as_ref().unwrap();
        assert!(channel.lake_inlet);
        assert_eq!(channel.level(1.0), 100.0);
        assert!((height(channel.end) - 100.0).abs() < 1e-6);
        assert_ne!(channel.end, world.cells[b].position);
        for step in 0..=128 {
            let t = step as f64 / 128.0;
            assert!(channel.level(t) <= height(channel.position(t)) + channel.depth(t) + 1e-6);
        }
    }

    #[test]
    fn clipped_lake_outlet_keeps_its_curve_transform_and_shared_plane() {
        let mut world = synthetic();
        let a = cell_index(4, 3, 3, 8);
        let b = cell_index(4, 4, 3, 8);
        world.cells[a].height_m = 100.0;
        world.cells[a].discharge = 1000.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].ocean = true;
        world.cells[b].height_m = -100.0;
        let mut channel = reach(&world, a).unwrap();
        channel.lake_outlet = true;
        let origin = channel.raw_start;
        let delta = std::array::from_fn(|i| channel.raw_end[i] - origin[i]);
        let height = |p: [f64; 3]| {
            let t = dot(std::array::from_fn(|i| p[i] - origin[i]), delta) / dot(delta, delta);
            50.0 + 200.0 * (std::f64::consts::PI * t).sin() - 150.0 * t
        };
        channel.clip_mouth(height);
        channel.clip_outlet(height);
        assert!((height(channel.start) - 100.0).abs() < 1e-6);
        assert!(height(channel.end).abs() < 1e-6);
        assert_eq!(channel.position(0.0), channel.start);
        assert_eq!(channel.position(1.0), channel.end);
        for i in 0..=20 {
            let t = i as f64 / 20.0;
            let (distance, recovered) = channel.closest(channel.position(t));
            assert!(distance < 1.0);
            assert!((t - recovered).abs() < 1e-4);
        }
        let mut channels = vec![None; world.cells.len()];
        channels[a] = Some(Box::new(channel));
        fit_profiles(&mut channels, height);
        assert_eq!(channels[a].as_ref().unwrap().level(0.0), 100.0);
        assert_eq!(channels[a].as_ref().unwrap().level(1.0), 0.0);
    }

    #[test]
    fn lake_outlet_does_not_restart_on_an_island_beyond_its_marine_mouth() {
        let mut world = synthetic();
        let a = cell_index(4, 3, 3, 8);
        let b = cell_index(4, 4, 3, 8);
        world.cells[a].height_m = 100.0;
        world.cells[a].discharge = 1000.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].ocean = true;
        world.cells[b].height_m = -100.0;
        let mut channel = reach(&world, a).unwrap();
        channel.lake_outlet = true;
        let origin = channel.raw_start;
        let delta = std::array::from_fn(|i| channel.raw_end[i] - origin[i]);
        let height = |p: [f64; 3]| {
            let t = dot(std::array::from_fn(|i| p[i] - origin[i]), delta) / dot(delta, delta);
            if t < 0.25 {
                20.0 - 200.0 * t
            } else {
                -30.0 + 500.0 * (t - 0.25)
            }
        };
        channel.clip_mouth(height);
        channel.clip_outlet(height);
        assert_eq!(channel.start, origin);
        assert!(channel.start_parameter < channel.end_parameter);
        assert!(height(channel.end).abs() < 1e-6);
    }

    #[test]
    fn lake_outlet_starts_after_the_last_submerged_hollow_before_its_control() {
        let mut world = synthetic();
        let a = cell_index(4, 3, 3, 8);
        let b = cell_index(4, 4, 3, 8);
        world.cells[a].height_m = 100.0;
        world.cells[a].discharge = 1000.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].height_m = 100.0;
        let mut channel = reach(&world, a).unwrap();
        channel.lake_outlet = true;
        let origin = channel.raw_start;
        let delta = std::array::from_fn(|i| channel.raw_end[i] - origin[i]);
        let parameter =
            |p: [f64; 3]| dot(std::array::from_fn(|i| p[i] - origin[i]), delta) / dot(delta, delta);
        let height = |p| {
            let t = parameter(p);
            if t < 0.2 {
                50.0 + 350.0 * t
            } else if t < 0.5 {
                120.0 - (70.0 / 0.3) * (t - 0.2)
            } else if t < 0.9 {
                50.0 + 175.0 * (t - 0.5)
            } else {
                120.0
            }
        };
        channel.clip_outlet(height);
        assert!(parameter(channel.start) > 0.7);
        assert!((height(channel.start) - 100.0).abs() < 1e-6);
        for i in 0..=128 {
            assert!(height(channel.position(i as f64 / 128.0)) >= 100.0 - 1e-6);
        }
    }

    #[test]
    fn cyclic_saved_receivers_do_not_produce_an_unfitted_profile() {
        let mut world = synthetic();
        let a = cell_index(4, 3, 3, 8);
        let b = cell_index(4, 4, 3, 8);
        for i in [a, b] {
            world.cells[i].height_m = 100.0;
            world.cells[i].discharge = 1000.0;
        }
        world.cells[a].receiver = Some(b);
        world.cells[b].receiver = Some(a);
        let mut channels = vec![None; world.cells.len()];
        channels[a] = reach(&world, a).map(Box::new);
        channels[b] = reach(&world, b).map(Box::new);
        fit_profiles(&mut channels, |_| 100.0);
        assert!(channels[a].is_none());
        assert!(channels[b].is_none());
    }

    #[test]
    fn tidal_mouth_widens_smoothly_but_steep_outfalls_do_not() {
        let mut world = synthetic();
        let a = cell_index(4, 3, 3, 8);
        let b = cell_index(4, 4, 3, 8);
        world.cells[a].height_m = 10.0;
        world.cells[a].discharge = 1000.0;
        world.cells[a].receiver = Some(b);
        world.cells[b].ocean = true;
        world.cells[b].height_m = -10.0;
        let mut channel = reach(&world, a).unwrap();
        assert!(channel.estuary_factor > 1.0);
        assert_eq!(channel.width(0.0), channel.width_start_m);
        assert_eq!(channel.width(1.0), channel.width_end_m);
        for i in 0..100 {
            assert!(channel.width(f64::from(i + 1) / 100.0) >= channel.width(f64::from(i) / 100.0));
        }
        channel.start_level_m = 50_000.0;
        channel.update_estuary();
        assert_eq!(channel.estuary_factor, 1.0);
        assert_eq!(channel.estuary_length_m, 0.0);
    }
}
