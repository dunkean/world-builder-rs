//! Continuous local reconstruction of the frozen physical atlas. Water bodies
//! provide their own hydraulic datum; interpolated wet fractions never flood
//! dry terrain. Detail is addressed by global position and protected by channels.

use crate::{geometry, noise, Biome, Sample, Tile, TileKey, World};
use anyhow::{bail, Result};
use geometry::{direction, dot, normalize, FACE_BASES};
use rayon::prelude::*;

const RIVER_ID_BASE: u64 = 1_u64 << 52;

struct Patch {
    attributes: [(usize, f64); 12],
    attribute_count: usize,
    nearby: [usize; 48],
    nearby_count: usize,
    height: f64,
    spacing_m: f64,
}

#[derive(Clone, Copy)]
struct WaterDatum {
    level: f64,
    id: u64,
    distance2: f64,
}

#[derive(Clone, Copy)]
struct Channel {
    distance_m: f64,
    half_width_m: f64,
    valley_width_m: f64,
    water_level_m: f64,
    depth_m: f64,
    discharge: f64,
    id: u64,
}

impl World {
    pub fn sample(&self, position: [f64; 3]) -> Sample {
        let p = normalize(position);
        let patch = reconstruction(self, p);
        let mut temperature = 0.0;
        let mut moisture = 0.0;
        let mut discharge = 0.0;
        let mut erodability = 0.0;
        let mut biome_mix = [0.0_f32; 10];
        let mut selected = patch.attributes[0].0;
        let mut dominant = -1.0;
        for &(index, weight) in &patch.attributes[..patch.attribute_count] {
            let cell = &self.cells[index];
            temperature += weight * f64::from(cell.temperature_c);
            moisture += weight * f64::from(cell.moisture);
            discharge += weight * f64::from(cell.discharge);
            erodability += weight * f64::from(cell.erodability);
            biome_mix[cell.biome as usize] += weight as f32;
            if weight > dominant {
                dominant = weight;
                selected = index;
            }
        }

        let shoreline_datum = water_datum(self, p, &patch);
        let mut datum = shoreline_datum;
        let mut height = patch.height;
        let mut valley_influence = 0.0_f64;
        let mut bed_owner: Option<(Channel, f64)> = None;
        for &index in &patch.nearby[..patch.nearby_count] {
            let Some(candidate) = channel_at(self, index, p, patch.spacing_m) else {
                continue;
            };
            let influence =
                smooth((1.0 - candidate.distance_m / candidate.valley_width_m).clamp(0.0, 1.0));
            if influence == 0.0 {
                continue;
            }
            // A spill outlet drains away from its lake. Its reconstructed
            // valley can lie below the lake plane while still being outside
            // the reservoir: a nearby wet stencil cell must not flood this
            // downstream reach back up to the lake's level.
            if !self.cells[selected].lake
                && datum.is_some_and(|water| {
                    water.id != 0 && candidate.water_level_m < water.level - 0.002
                })
            {
                datum = None;
            }
            valley_influence = valley_influence.max(influence);
            // A hydraulic bed and a smooth cross-section connect to the frozen
            // drainage graph. The broad valley blend avoids imposing a remote
            // atlas bed with an abrupt step at a pixel-wide river boundary.
            let bank = candidate.depth_m * (candidate.distance_m / candidate.half_width_m).powi(2);
            let bank = bank_profile(
                bank,
                candidate.depth_m,
                (patch.height - candidate.water_level_m).abs().max(40.0),
            );
            let target = candidate.water_level_m - candidate.depth_m + bank;
            let carved = patch.height * (1.0 - influence) + target * influence;
            height = height.min(carved);
            // Among actual hydraulic sections, pair the water profile with
            // the reach that shapes the lowest bed. Broad dry valleys keep
            // their terrain influence but cannot steal a real river's datum.
            // A wet section also takes precedence over dry shoreline support.
            if candidate.distance_m < candidate.half_width_m * 1.1
                && bed_owner.is_none_or(|(old, old_bed)| {
                    let wet = candidate.distance_m < candidate.half_width_m;
                    let old_wet = old.distance_m < old.half_width_m;
                    (wet && !old_wet)
                        || (wet == old_wet
                            && (carved < old_bed || (carved == old_bed && candidate.id < old.id)))
                })
            {
                bed_owner = Some((candidate, carved));
            }
        }
        // A small dry support lets the renderer clip a shoreline. It must not
        // turn the much broader valley, or a clamped reach endpoint, into water.
        let channel = bed_owner
            .map(|(owner, _)| owner)
            .filter(|owner| owner.distance_m < owner.half_width_m * 1.1);

        let mut amplitude = self.config.detail_m * (1.3 - 0.4 * erodability).clamp(0.3, 1.4);
        if let Some(water) = shoreline_datum.filter(|d| d.id != 0) {
            // Keep lake basin structure, with small shoreline bands rather
            // than unconstrained hills replacing the bottom of a shallow lake.
            amplitude = amplitude.min(3.0 + 0.12 * (patch.height - water.level).abs());
        }
        amplitude *= (1.0 - valley_influence).powi(2);
        if amplitude > 0.0 {
            height += fractal_detail(self, p, erodability) * amplitude;
        }

        let mut ocean = false;
        let mut lake = false;
        let mut river = 0.0;
        let mut water_level = datum.map(|d| d.level);
        let mut water_id = datum.map(|d| d.id);
        if let Some(water) = datum {
            if height < water.level {
                ocean = water.id == 0;
                lake = water.id != 0;
            }
        }
        if !ocean && !lake {
            if let Some(channel) = channel {
                discharge = discharge.max(channel.discharge);
                water_level = Some(channel.water_level_m);
                water_id = Some(channel.id);
                river = smooth((1.0 - channel.distance_m / channel.half_width_m).clamp(0.0, 1.0));
            }
        }
        if height < 0.0 && water_id.is_none_or(|id| id == 0) {
            if let Some(owner) = basin_owner(self, &patch) {
                // The canonical reservoir owns its dry submerged fringes too.
                // A nearby sea stencil must not fill an endorheic basin up to
                // zero, and a coastal fallback must not erase its water budget.
                ocean = false;
                lake = height < f64::from(owner.level_m);
                water_level = Some(f64::from(owner.level_m));
                water_id = Some(owner.id);
            } else {
                // The unresolved marine coast uses a global zero level set.
                // Keep the frozen fine height and existing lake/channel owners;
                // the finite 4x4 terrain stencil is not an ocean shoreline.
                ocean = true;
                water_level = Some(0.0);
                water_id = Some(0);
            }
        }
        let water = water_level
            .filter(|&level| level > height && (ocean || lake || river > 0.0))
            .unwrap_or(height);
        let mut biome = self.cells[selected].biome;
        if ocean || lake {
            biome = if ocean { Biome::Ocean } else { Biome::Lake };
            biome_mix = [0.0; 10];
            biome_mix[biome as usize] = 1.0;
        } else {
            // Water color is not allowed to bleed onto emerging land merely
            // because a neighboring atlas cell belongs to a lake or the sea.
            biome_mix[Biome::Ocean as usize] = 0.0;
            biome_mix[Biome::Lake as usize] = 0.0;
            if matches!(biome, Biome::Ocean | Biome::Lake) {
                biome = land_biome(temperature, moisture);
            }
            if river > 0.15 && matches!(biome, Biome::Desert | Biome::Grassland) {
                biome = Biome::Wetland;
                biome_mix[Biome::Wetland as usize] += 0.25;
            }
            let sum: f32 = biome_mix.iter().sum();
            if sum > 0.0 {
                for fraction in &mut biome_mix {
                    *fraction /= sum;
                }
            } else {
                biome_mix[biome as usize] = 1.0;
            }
        }
        Sample {
            direction: p,
            height_m: height as f32,
            water_m: water.max(height) as f32,
            water_level_m: water_level.map(|h| h as f32),
            water_body_id: water_id,
            temperature_c: temperature as f32,
            moisture: moisture as f32,
            discharge: discharge as f32,
            biome,
            biome_mix,
            material: self.cells[selected].material,
            erodability: erodability as f32,
            ocean,
            lake,
            river: river as f32,
        }
    }

    pub fn tile(&self, key: TileKey, segments: u32) -> Result<Tile> {
        if key.face >= 6 || key.level > 24 {
            bail!("invalid tile face or level");
        }
        let count = 1_u32 << key.level;
        if key.x >= count || key.y >= count || !(2..=256).contains(&segments) {
            bail!("invalid tile coordinates or segments (2..=256)");
        }
        let side = (segments + 1) as usize;
        let sample_at = |index: usize| {
            let x = (index % side) as u32;
            let y = (index / side) as u32;
            let denominator = f64::from(count) * f64::from(segments);
            let u =
                2.0 * (f64::from(key.x) * f64::from(segments) + f64::from(x)) / denominator - 1.0;
            let v =
                2.0 * (f64::from(key.y) * f64::from(segments) + f64::from(y)) / denominator - 1.0;
            self.sample(direction(key.face, u, v))
        };
        // Indexed collection preserves the exact row-major order and each
        // point's arithmetic. Small tiles avoid thread scheduling overhead.
        let vertices = if segments >= 64 {
            (0..side * side).into_par_iter().map(sample_at).collect()
        } else {
            (0..side * side).map(sample_at).collect()
        };
        let mut indices = Vec::with_capacity((segments * segments * 6) as usize);
        for y in 0..segments {
            for x in 0..segments {
                let a = y * (segments + 1) + x;
                indices.extend_from_slice(&[
                    a,
                    a + 1,
                    a + segments + 2,
                    a,
                    a + segments + 2,
                    a + segments + 1,
                ]);
            }
        }
        Ok(Tile {
            key,
            segments,
            vertices,
            indices,
        })
    }
}

fn atlas_index(face: u8, x: i32, y: i32, n: u32) -> usize {
    if x >= 0 && y >= 0 && x < n as i32 && y < n as i32 {
        geometry::cell_index(face, x as u32, y as u32, n)
    } else {
        geometry::direction_index(
            direction(
                face,
                2.0 * (f64::from(x) + 0.5) / f64::from(n) - 1.0,
                2.0 * (f64::from(y) + 0.5) / f64::from(n) - 1.0,
            ),
            n,
        )
    }
}

fn reconstruction(world: &World, p: [f64; 3]) -> Patch {
    let n = world.config.resolution;
    let mut result = Patch {
        attributes: [(0, 0.0); 12],
        attribute_count: 0,
        nearby: [0; 48],
        nearby_count: 0,
        height: 0.0,
        spacing_m: 0.0,
    };
    let mut total_weight = 0.0;
    for (face, [normal, bu, bv]) in FACE_BASES.iter().enumerate() {
        let plane = dot(p, *normal);
        if plane <= 0.0 {
            continue;
        }
        let u = dot(p, *bu) / plane;
        let v = dot(p, *bv) / plane;
        let face_weight = (1.0 + 1.0 / f64::from(n) - u.abs().max(v.abs())).max(0.0);
        if face_weight == 0.0 {
            continue;
        }
        let fx = (u + 1.0) * 0.5 * f64::from(n) - 0.5;
        let fy = (v + 1.0) * 0.5 * f64::from(n) - 0.5;
        let ix = fx.floor() as i32;
        let iy = fy.floor() as i32;
        let tx = fx - f64::from(ix);
        let ty = fy - f64::from(iy);
        let mut stencil = [[0.0; 4]; 4];
        for (dy, row) in stencil.iter_mut().enumerate() {
            for (dx, height) in row.iter_mut().enumerate() {
                let index = atlas_index(face as u8, ix + dx as i32 - 1, iy + dy as i32 - 1, n);
                *height = f64::from(world.cells[index].height_m);
                result.nearby[result.nearby_count] = index;
                result.nearby_count += 1;
            }
        }
        // Interpolating monotone cubic reconstruction preserves atlas centers
        // and forbids overshoots beyond the neighborhood's physical heights.
        // Symmetric row/column evaluation reduces directional interpolation bias.
        let rows = std::array::from_fn(|y| cubic(stencil[y], tx));
        let columns = std::array::from_fn(|x| cubic(std::array::from_fn(|y| stencil[y][x]), ty));
        result.height += 0.5 * (cubic(rows, ty) + cubic(columns, tx)) * face_weight;
        result.spacing_m +=
            2.0 * world.config.radius_m / f64::from(n) / (1.0 + u * u + v * v).powf(0.75)
                * face_weight;
        for (dx, dy, weight) in [
            (0, 0, (1.0 - tx) * (1.0 - ty)),
            (1, 0, tx * (1.0 - ty)),
            (0, 1, (1.0 - tx) * ty),
            (1, 1, tx * ty),
        ] {
            result.attributes[result.attribute_count] = (
                atlas_index(face as u8, ix + dx, iy + dy, n),
                weight * face_weight,
            );
            result.attribute_count += 1;
        }
        total_weight += face_weight;
    }
    result.height /= total_weight;
    result.spacing_m /= total_weight;
    for (_, weight) in &mut result.attributes[..result.attribute_count] {
        *weight /= total_weight;
    }
    result
}

fn cubic(v: [f64; 4], t: f64) -> f64 {
    let slope = |a: f64, b: f64| {
        if a * b <= 0.0 {
            0.0
        } else {
            2.0 * a * b / (a + b)
        }
    };
    let delta = v[2] - v[1];
    let m0 = slope(v[1] - v[0], delta);
    let m1 = slope(delta, v[3] - v[2]);
    let t2 = t * t;
    let t3 = t2 * t;
    (2.0 * t3 - 3.0 * t2 + 1.0) * v[1]
        + (t3 - 2.0 * t2 + t) * m0
        + (-2.0 * t3 + 3.0 * t2) * v[2]
        + (t3 - t2) * m1
}

fn water_datum(world: &World, p: [f64; 3], patch: &Patch) -> Option<WaterDatum> {
    let mut result: Option<WaterDatum> = None;
    for &index in &patch.nearby[..patch.nearby_count] {
        let cell = &world.cells[index];
        if !cell.ocean && !cell.lake {
            continue;
        }
        if cell.lake
            && !patch.attributes[..patch.attribute_count]
                .iter()
                .any(|&(corner, weight)| corner == index && weight > 1e-10)
        {
            // The 4x4 stencil supports cubic terrain derivatives, not water
            // connectivity. A remote reservoir beyond the current quad must
            // not lend its plane to another depression behind a dry ridge.
            // Keep the sea's existing global datum and lake shoreline support
            // on both wet and dry sides of a quad containing real lake water.
            continue;
        }
        let distance2 = (p[0] - cell.position[0]).powi(2)
            + (p[1] - cell.position[1]).powi(2)
            + (p[2] - cell.position[2]).powi(2);
        let id = if cell.ocean {
            0
        } else {
            cell.lake_id
                .unwrap_or(1 + u64::from(cell.water_m.to_bits()))
        };
        let datum = WaterDatum {
            level: if cell.ocean {
                0.0
            } else {
                f64::from(cell.water_m)
            },
            id,
            distance2,
        };
        if result.is_none_or(|old| {
            distance2 < old.distance2 || (distance2 == old.distance2 && id < old.id)
        }) {
            result = Some(datum);
        }
    }
    result
}

fn basin_owner<'a>(world: &'a World, patch: &Patch) -> Option<&'a crate::hydrology::Lake> {
    // Most marine/coastal quads contain no depression at all. Do not scan the
    // small reservoir lists on their hot path. No cache is kept: World is public
    // and synthetic worlds can change their cells and reservoirs between calls.
    if !patch.attributes[..patch.attribute_count]
        .iter()
        .any(|&(index, weight)| {
            let cell = &world.cells[index];
            weight > 1e-10 && !cell.ocean && cell.routing_height_m - cell.height_m >= 0.0005
        })
    {
        return None;
    }
    let mut result: Option<(&crate::hydrology::Lake, f64)> = None;
    for &(index, weight) in &patch.attributes[..patch.attribute_count] {
        if weight <= 1e-10 {
            continue;
        }
        for lake in &world.lakes {
            if lake.cells.contains(&index)
                && result.is_none_or(|(old, old_weight)| {
                    weight > old_weight || (weight == old_weight && lake.id < old.id)
                })
                && lake.cells.iter().any(|&i| {
                    let cell = &world.cells[i];
                    cell.lake
                        && cell.lake_id == Some(lake.id)
                        && cell.water_m.to_bits() == lake.level_m.to_bits()
                })
            {
                result = Some((lake, weight));
            }
        }
    }
    result.map(|(owner, _)| owner)
}

fn smooth(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

fn bank_profile(bank: f64, depth: f64, cap: f64) -> f64 {
    // Preserve the wet parabolic section exactly. Outside its shoreline,
    // approach the valley shoulder without the hard cap's derivative break.
    if bank <= depth {
        bank
    } else {
        let scale = (cap - depth).max(1e-6);
        depth - scale * (-(bank - depth) / scale).exp_m1()
    }
}

fn fractal_detail(world: &World, p: [f64; 3], erodability: f64) -> f64 {
    let frequency = f64::from(world.config.resolution) * 0.75;
    let warp = std::array::from_fn(|i| {
        p[i] + 0.32 / frequency
            * noise(
                world.config.seed ^ (0x99121 + i as u64 * 107),
                p,
                frequency * 0.37,
            )
    });
    let warp = normalize(warp);
    let mut value = 0.0;
    let mut normalization = 0.0;
    let mut amplitude = 1.0;
    let mut frequency = frequency;
    let ridge_fraction = (0.58 - 0.24 * erodability).clamp(0.1, 0.5);
    for band in 0..17 {
        if world.config.radius_m / frequency < 0.5 {
            break;
        }
        let n = noise(
            world.config.seed.wrapping_add(0x44281 + band * 97),
            warp,
            frequency,
        );
        let ridge = (0.6 - 2.0 * n.abs()).clamp(-1.0, 1.0);
        value += amplitude * (n * (1.0 - ridge_fraction) + ridge * ridge_fraction);
        normalization += amplitude;
        amplitude *= 2.0_f64.powf(-0.85);
        frequency *= 2.0;
    }
    value / normalization
}

fn tangent(world: &World, index: usize) -> [f64; 3] {
    let cell = &world.cells[index];
    let mut incoming = None;
    for neighbor in crate::hydrology::neighbours8(index, world.config.resolution) {
        if world.cells[neighbor].receiver == Some(index)
            && incoming.is_none_or(|old: usize| {
                world.cells[neighbor].discharge > world.cells[old].discharge
            })
        {
            incoming = Some(neighbor);
        }
    }
    let before = incoming.map_or(cell.position, |i| world.cells[i].position);
    let after = cell
        .receiver
        .map_or(cell.position, |i| world.cells[i].position);
    let mut result = std::array::from_fn(|i| after[i] - before[i]);
    let radial = dot(result, cell.position);
    for (i, r) in result.iter_mut().enumerate() {
        *r -= radial * cell.position[i];
    }
    normalize(result)
}

fn curve(world: &World, from: usize, to: usize, t: f64, ta: [f64; 3], tb: [f64; 3]) -> [f64; 3] {
    let a = world.cells[from].position;
    let b = world.cells[to].position;
    let length = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    let t2 = t * t;
    let t3 = t2 * t;
    normalize(std::array::from_fn(|i| {
        (2.0 * t3 - 3.0 * t2 + 1.0) * a[i]
            + (t3 - 2.0 * t2 + t) * length * ta[i]
            + (-2.0 * t3 + 3.0 * t2) * b[i]
            + (t3 - t2) * length * tb[i]
    }))
}

fn channel_at(world: &World, index: usize, p: [f64; 3], spacing: f64) -> Option<Channel> {
    let cell = &world.cells[index];
    if cell.ocean || cell.lake || cell.discharge < 120.0 {
        return None;
    }
    let to = cell.receiver?;
    let end = &world.cells[to];
    let end_level = if end.ocean {
        0.0
    } else if end.lake {
        f64::from(end.water_m)
    } else {
        f64::from(end.height_m)
    };
    let start_level = f64::from(cell.height_m);
    // Potential spill routing is not a physical river through a dry depression.
    if end_level > start_level + 0.002 {
        return None;
    }
    let half_width = world.channel_width(cell.discharge) * 0.5;
    let spacing = cell.area_m2.sqrt().max(spacing * 0.1);
    let valley_width = (half_width * 24.0).max(spacing * 0.28).min(spacing * 0.8);
    let delta = std::array::from_fn::<_, 3, _>(|j| end.position[j] - cell.position[j]);
    let segment_length = dot(delta, delta).sqrt() * world.config.radius_m;
    let projection = (dot(std::array::from_fn(|j| p[j] - cell.position[j]), delta)
        / dot(delta, delta))
    .clamp(0.0, 1.0);
    let near = normalize(std::array::from_fn(|j| {
        cell.position[j] + projection * delta[j]
    }));
    let straight_distance =
        ((p[0] - near[0]).powi(2) + (p[1] - near[1]).powi(2) + (p[2] - near[2]).powi(2)).sqrt()
            * world.config.radius_m;
    if straight_distance > valley_width + segment_length * 0.4 {
        return None;
    }
    let ta = tangent(world, index);
    let tb = tangent(world, to);
    let mut best_distance = f64::INFINITY;
    let mut best_t = 0.0;
    let mut a = cell.position;
    for i in 0..8 {
        let b = curve(world, index, to, f64::from(i + 1) / 8.0, ta, tb);
        let delta = std::array::from_fn::<_, 3, _>(|j| b[j] - a[j]);
        let local =
            (dot(std::array::from_fn(|j| p[j] - a[j]), delta) / dot(delta, delta)).clamp(0.0, 1.0);
        let closest = normalize(std::array::from_fn(|j| a[j] + local * delta[j]));
        let distance = ((p[0] - closest[0]).powi(2)
            + (p[1] - closest[1]).powi(2)
            + (p[2] - closest[2]).powi(2))
        .sqrt()
            * world.config.radius_m;
        if distance < best_distance {
            best_distance = distance;
            best_t = (f64::from(i) + local) / 8.0;
        }
        a = b;
    }
    if best_distance >= valley_width {
        return None;
    }
    let level = start_level * (1.0 - best_t) + end_level.min(start_level) * best_t;
    let depth = (0.35 + f64::from(cell.discharge).powf(0.3) * 0.15)
        * (3.0 / world.config.gravity_m_s2).sqrt();
    Some(Channel {
        distance_m: best_distance,
        half_width_m: half_width,
        valley_width_m: valley_width,
        water_level_m: level,
        depth_m: depth.clamp(0.5, 18.0),
        discharge: f64::from(cell.discharge),
        id: RIVER_ID_BASE + index as u64 + 1,
    })
}

fn land_biome(temperature: f64, moisture: f64) -> Biome {
    if temperature < -8.0 {
        Biome::Ice
    } else if temperature < 3.0 {
        Biome::Tundra
    } else if moisture < 0.22 {
        Biome::Desert
    } else if temperature > 19.0 && moisture > 0.70 {
        Biome::Rainforest
    } else if moisture > 0.46 {
        Biome::Forest
    } else {
        Biome::Grassland
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Material, WorldConfig};

    fn synthetic() -> World {
        let mut world = World::generate(WorldConfig {
            resolution: 8,
            detail_m: 0.0,
            ..Default::default()
        })
        .unwrap();
        for c in &mut world.cells {
            c.height_m = 160.0;
            c.water_m = 160.0;
            c.ocean = false;
            c.lake = false;
            c.lake_id = None;
            c.receiver = None;
            c.discharge = 0.0;
            c.temperature_c = 20.0;
            c.moisture = 0.5;
            c.material = Material::Sandstone;
            c.biome = Biome::Grassland;
        }
        world
    }

    #[test]
    fn lake_surface_is_horizontal_and_dry_neighbors_are_not_flooded() {
        let mut world = synthetic();
        for (x, y) in [(3, 3), (4, 3), (3, 4), (4, 4)] {
            let c = &mut world.cells[geometry::cell_index(4, x, y, 8)];
            c.height_m = 60.0;
            c.water_m = 100.0;
            c.lake = true;
            c.lake_id = Some(17);
            c.biome = Biome::Lake;
        }
        let mut wet = 0;
        for y in 0..41 {
            for x in 0..41 {
                let p = direction(4, -0.4 + f64::from(x) * 0.02, -0.4 + f64::from(y) * 0.02);
                let s = world.sample(p);
                if s.lake {
                    wet += 1;
                    assert_eq!(s.water_m, 100.0);
                    assert_eq!(s.water_level_m, Some(100.0));
                    assert_eq!(s.water_body_id, Some(17));
                    assert!(s.height_m < s.water_m);
                } else {
                    assert_eq!(s.water_m, s.height_m);
                }
            }
        }
        assert!(wet > 20);
        let dry = world.sample(world.cells[geometry::cell_index(4, 2, 3, 8)].position);
        assert!(!dry.lake);
        assert_eq!(dry.water_m, dry.height_m);
        assert_eq!(dry.biome_mix[Biome::Lake as usize], 0.0);
    }

    #[test]
    fn lake_plane_does_not_cross_a_ridge_into_an_independent_low_valley() {
        let mut world = synthetic();
        let lake = geometry::cell_index(4, 3, 3, 8);
        let dry_valley = geometry::cell_index(4, 5, 3, 8);
        world.cells[lake].height_m = 60.0;
        world.cells[lake].water_m = 100.0;
        world.cells[lake].lake = true;
        world.cells[lake].lake_id = Some(17);
        world.cells[lake].biome = Biome::Lake;
        world.cells[dry_valley].height_m = 60.0;
        world.cells[dry_valley].water_m = 60.0;
        world.cells[dry_valley].routing_height_m = 60.0;

        let (face, u, v) = geometry::face_uv(world.cells[dry_valley].position);
        for y in -4..=4 {
            for x in -4..=4 {
                let p = direction(face, u + f64::from(x) / 400.0, v + f64::from(y) / 400.0);
                let sample = world.sample(p);
                assert!(!sample.lake, "remote lake flooded x={x}, y={y}");
                assert_eq!(sample.water_level_m, None);
                assert_eq!(sample.water_body_id, None);
                assert_eq!(sample.water_m, sample.height_m);
            }
        }
        let center = world.sample(world.cells[dry_valley].position);
        assert_eq!(center.height_m, 60.0);
        let wet = world.sample(world.cells[lake].position);
        assert!(wet.lake);
        assert_eq!(wet.water_m, 100.0);
    }

    #[test]
    fn marine_surface_does_not_stop_at_the_terrain_stencil() {
        let mut world = synthetic();
        world.lakes.clear();
        for cell in &mut world.cells {
            cell.height_m = -5.0;
            cell.water_m = -5.0;
            cell.routing_height_m = 0.0;
        }
        let ocean = geometry::cell_index(4, 0, 3, 8);
        world.cells[ocean].ocean = true;
        world.cells[ocean].water_m = 0.0;
        let (_, u, v) = geometry::face_uv(world.cells[ocean].position);
        // Cross the exact old four-cell support boundary; both sides of each
        // resulting triangle are under sea level and must share the same datum.
        for i in -16..=16 {
            let p = direction(4, u + 0.5 + f64::from(i) * 0.00001, v);
            let patch = reconstruction(&world, p);
            let sample = world.sample(p);
            assert_eq!(sample.height_m.to_bits(), (patch.height as f32).to_bits());
            assert!(sample.ocean);
            assert_eq!(sample.water_level_m, Some(0.0));
            assert_eq!(sample.water_body_id, Some(0));
            assert_eq!(sample.water_m, 0.0);
        }
        let land = geometry::cell_index(4, 5, 3, 8);
        world.cells[land].height_m = 160.0;
        let dry = world.sample(world.cells[land].position);
        assert!(!dry.ocean);
        assert_eq!(dry.water_level_m, None);
    }

    #[test]
    fn closed_subsea_basin_keeps_its_dry_fringe_and_lake_level() {
        let mut world = synthetic();
        world.lakes.clear();
        let pit = geometry::cell_index(4, 2, 3, 8);
        let fringe = geometry::cell_index(4, 3, 3, 8);
        let dry = geometry::cell_index(4, 4, 3, 8);
        let sea = geometry::cell_index(4, 6, 3, 8);
        for (index, height) in [(pit, -120.0), (fringe, -60.0), (dry, -20.0)] {
            world.cells[index].height_m = height;
            world.cells[index].water_m = height;
            world.cells[index].routing_height_m = 100.0;
        }
        world.cells[pit].lake = true;
        world.cells[pit].lake_id = Some(17);
        world.cells[pit].water_m = -80.0;
        world.cells[sea].height_m = -30.0;
        world.cells[sea].ocean = true;
        world.cells[sea].water_m = 0.0;
        world.cells[sea].routing_height_m = 0.0;
        world.lakes.push(crate::hydrology::Lake {
            id: 17,
            level_m: -80.0,
            spill_level_m: 100.0,
            cells: vec![pit, fringe, dry],
            ..Default::default()
        });
        for index in [pit, fringe, dry] {
            let sample = world.sample(world.cells[index].position);
            assert_eq!(sample.height_m, world.cells[index].height_m);
            assert!(!sample.ocean);
            assert_eq!(sample.water_level_m, Some(-80.0));
            assert_eq!(sample.water_body_id, Some(17));
            assert_eq!(sample.lake, index == pit);
            assert_eq!(sample.water_m, world.cells[index].height_m.max(-80.0));
        }
        // Reservoir ownership follows the current World, including mutations;
        // it cannot come from an ID-keyed static spatial cache.
        world.lakes.clear();
        assert!(world.sample(world.cells[dry].position).ocean);
    }

    #[test]
    fn monotone_cubic_preserves_centers_and_avoids_spurious_islands() {
        for input in [
            [-10.0, -10.0, 100.0, 100.0],
            [300.0, 0.0, 0.0, 300.0],
            [-100.0, -20.0, -10.0, 1000.0],
        ] {
            assert_eq!(cubic(input, 0.0), input[1]);
            assert_eq!(cubic(input, 1.0), input[2]);
            for i in 0..101 {
                let h = cubic(input, f64::from(i) / 100.0);
                assert!(h >= input[1].min(input[2]) - 1e-9 && h <= input[1].max(input[2]) + 1e-9);
            }
        }
    }

    #[test]
    fn valley_shoulders_are_c1_and_keep_the_wet_section() {
        for (depth, cap) in [(3.0, 40.0), (18.0, 6000.0)] {
            for i in 0..=20 {
                let bank = depth * f64::from(i) / 20.0;
                assert_eq!(bank_profile(bank, depth, cap), bank);
            }
            // Test both the wet/dry join and the former hard-cap terrace.
            for join in [depth, cap] {
                let epsilon = 0.001;
                let center = bank_profile(join, depth, cap);
                let left = (center - bank_profile(join - epsilon, depth, cap)) / epsilon;
                let right = (bank_profile(join + epsilon, depth, cap) - center) / epsilon;
                assert!((left - right).abs() < 0.0001);
                assert!(left > 0.0 && right > 0.0);
            }
            let mut previous = 0.0;
            for i in 0..=100 {
                let height = bank_profile(cap * f64::from(i) / 10.0, depth, cap);
                assert!(height >= previous && height <= cap);
                previous = height;
            }
        }
    }

    #[test]
    fn fractal_coast_is_stable_across_lod_and_preserves_continental_envelope() {
        let mut world = synthetic();
        world.config.detail_m = 40.0;
        for c in &mut world.cells {
            c.height_m = 1200.0 * c.position[0] as f32;
            c.ocean = c.height_m <= 0.0;
            c.water_m = if c.ocean { 0.0 } else { c.height_m };
        }
        let a = world
            .tile(
                TileKey {
                    face: 4,
                    level: 1,
                    x: 0,
                    y: 0,
                },
                8,
            )
            .unwrap();
        let b = world
            .tile(
                TileKey {
                    face: 4,
                    level: 2,
                    x: 0,
                    y: 0,
                },
                8,
            )
            .unwrap();
        for y in 0..=8 {
            for x in 0..=8 {
                if x % 2 == 0 && y % 2 == 0 {
                    let parent = &a.vertices[(y / 2) * 9 + x / 2];
                    let fine = &b.vertices[y * 9 + x];
                    assert_eq!(parent.height_m.to_bits(), fine.height_m.to_bits());
                    assert_eq!(parent.water_body_id, fine.water_body_id);
                }
            }
        }
        for c in &world.cells {
            if c.height_m.abs() > 80.0 {
                let s = world.sample(c.position);
                assert_eq!(s.height_m.is_sign_positive(), c.height_m.is_sign_positive());
            }
        }
    }

    #[test]
    fn coastal_tiles_preserve_samples_across_threads_levels_and_request_order() {
        let mut world = synthetic();
        world.config.detail_m = 140.0;
        let coast = direction(5, -19.0 / 128.0, 45.0 / 128.0);
        for cell in &mut world.cells {
            cell.height_m = (1200.0 * (cell.position[0] - coast[0])) as f32;
            cell.ocean = cell.height_m <= 0.0;
            cell.water_m = if cell.ocean { 0.0 } else { cell.height_m };
            cell.biome = if cell.ocean {
                Biome::Ocean
            } else {
                Biome::Grassland
            };
        }
        let parent_key = TileKey {
            face: 5,
            level: 8,
            x: 109,
            y: 173,
        };
        let child_key = TileKey {
            face: 5,
            level: 10,
            x: 436,
            y: 692,
        };
        let fine_key = TileKey {
            face: 5,
            level: 14,
            x: 6978,
            y: 11074,
        };
        let serial_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let parallel_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let parent = serial_pool.install(|| world.tile(parent_key, 64).unwrap());
        let (fine, child) = parallel_pool.install(|| {
            rayon::join(
                || world.tile(fine_key, 64).unwrap(),
                || world.tile(child_key, 64).unwrap(),
            )
        });
        let encode = |sample: &Sample| serde_json::to_vec(sample).unwrap();
        assert_eq!(encode(&parent.vertices[0]), encode(&world.sample(coast)));
        assert_eq!(encode(&parent.vertices[0]), encode(&child.vertices[0]));
        assert_eq!(
            encode(&parent.vertices[2 * 65 + 2]),
            encode(&child.vertices[8 * 65 + 8])
        );
        assert_eq!(
            encode(&parent.vertices[2 * 65 + 2]),
            encode(&fine.vertices[0])
        );
        let again = parallel_pool.install(|| world.tile(parent_key, 64).unwrap());
        assert_eq!(
            serde_json::to_vec(&parent).unwrap(),
            serde_json::to_vec(&again).unwrap()
        );
    }

    #[test]
    fn river_reaches_share_levels_at_connections_and_survive_sampling() {
        let mut world = synthetic();
        let cells: [usize; 3] = [
            geometry::cell_index(4, 2, 3, 8),
            geometry::cell_index(4, 3, 3, 8),
            geometry::cell_index(4, 4, 3, 8),
        ];
        for (i, &index) in cells.iter().enumerate() {
            world.cells[index].height_m = 100.0 - i as f32 * 20.0;
            world.cells[index].water_m = world.cells[index].height_m;
            world.cells[index].discharge = 1000.0;
            if i < 2 {
                world.cells[index].receiver = Some(cells[i + 1]);
            }
        }
        let center = world.sample(world.cells[cells[1]].position);
        assert!(center.river > 0.99);
        assert!(center.height_m < center.water_m);
        assert_eq!(center.water_level_m, Some(80.0));
        assert!(center.water_body_id.unwrap() >= RIVER_ID_BASE);
        let mut previous = 1000.0;
        for i in 0..41 {
            let t = f64::from(i) / 40.0;
            let p = curve(
                &world,
                cells[0],
                cells[1],
                t,
                tangent(&world, cells[0]),
                tangent(&world, cells[1]),
            );
            let s = world.sample(p);
            let level = s.water_level_m.unwrap();
            assert!(level <= previous + 0.05);
            assert!(s.height_m < s.water_m);
            previous = level;
        }
    }

    #[test]
    fn lake_outlet_valley_uses_downstream_river_level() {
        let mut world = synthetic();
        let lake = geometry::cell_index(4, 2, 3, 8);
        let outlet = geometry::cell_index(4, 3, 3, 8);
        let downstream = geometry::cell_index(4, 4, 3, 8);
        world.cells[lake].height_m = 60.0;
        world.cells[lake].water_m = 100.0;
        world.cells[lake].lake = true;
        world.cells[lake].lake_id = Some(17);
        world.cells[lake].biome = Biome::Lake;
        world.cells[lake].receiver = Some(outlet);
        world.cells[outlet].height_m = 100.0;
        world.cells[outlet].water_m = 100.0;
        world.cells[outlet].receiver = Some(downstream);
        world.cells[outlet].discharge = 1000.0;
        world.cells[downstream].height_m = 60.0;
        world.cells[downstream].water_m = 60.0;
        world.cells[downstream].discharge = 1000.0;

        // The lake stays visible in the cubic stencil along the entire reach.
        // Its plane must never replace the river's falling hydraulic profile.
        for i in 1..=40 {
            let t = f64::from(i) / 40.0;
            let p = curve(
                &world,
                outlet,
                downstream,
                t,
                tangent(&world, outlet),
                tangent(&world, downstream),
            );
            let sample = world.sample(p);
            assert!(!sample.lake, "lake flooded river at t={t}");
            assert!(sample.river > 0.99);
            assert!(sample.water_body_id.unwrap() >= RIVER_ID_BASE);
            assert!((f64::from(sample.water_m) - (100.0 - 40.0 * t)).abs() < 0.1);
        }
    }

    #[test]
    fn river_node_does_not_flood_a_shallow_transverse_valley() {
        let mut world = synthetic();
        let previous_radius = world.config.radius_m;
        world.config.radius_m = 60_000.0;
        let area_scale = (world.config.radius_m / previous_radius).powi(2);
        let incoming = geometry::cell_index(4, 2, 3, 8);
        let node = geometry::cell_index(4, 3, 3, 8);
        let outgoing = geometry::cell_index(4, 4, 3, 8);
        let center = world.cells[node].position;
        let along = normalize(std::array::from_fn(|i| {
            world.cells[outgoing].position[i] - world.cells[incoming].position[i]
        }));
        let transverse = normalize(geometry::cross(center, along));
        for cell in &mut world.cells {
            // A shallow transverse slope places the cubic terrain a few
            // centimetres below the node's hydraulic level on one bank.
            let offset = dot(
                std::array::from_fn(|i| cell.position[i] - center[i]),
                transverse,
            ) * world.config.radius_m;
            cell.height_m = (100.0 - 0.0001 * offset) as f32;
            cell.water_m = cell.height_m;
            cell.area_m2 *= area_scale;
        }
        for (index, height, receiver) in [
            (incoming, 102.0, Some(node)),
            (node, 100.0, Some(outgoing)),
            (outgoing, 50.0, None),
        ] {
            world.cells[index].height_m = height;
            world.cells[index].water_m = height;
            world.cells[index].receiver = receiver;
            world.cells[index].discharge = 11_495.0;
        }
        let half_width = world.channel_width(11_495.0) * 0.5;
        let at_distance = |distance: f64| {
            normalize(std::array::from_fn(|i| {
                center[i] + distance / world.config.radius_m * transverse[i]
            }))
        };
        let water = world.sample(center);
        assert!(water.river > 0.99);
        assert!(water.water_m > water.height_m);
        let phantom_arm = world.sample(at_distance(half_width * 2.0));
        assert!(phantom_arm.height_m < 100.0);
        assert_eq!(phantom_arm.river, 0.0);
        assert_eq!(phantom_arm.water_m, phantom_arm.height_m);
        assert_eq!(phantom_arm.water_body_id, None);
        let dry_support = world.sample(at_distance(half_width * 1.05));
        assert_eq!(dry_support.river, 0.0);
        assert_eq!(dry_support.water_m, dry_support.height_m);
        assert!(dry_support.water_body_id.is_some());

        let patch = crate::RegionPatch {
            key: TileKey {
                face: 4,
                level: 0,
                x: 0,
                y: 0,
            },
            segments: 16,
            halo_cells: 0,
            heights_m: vec![0.0; 17 * 17],
            residual_m: vec![0.0; 17 * 17],
            source_world_id: world.id.clone(),
            algorithm_version: "test-dry-river-support".into(),
        };
        let published = world.sample_with_patches(at_distance(half_width * 1.05), &[patch.clone()]);
        assert_eq!(published.river, 0.0);
        assert_eq!(published.water_m, published.height_m);
        let tile = world
            .tile_with_patches(
                TileKey {
                    face: 4,
                    level: 6,
                    x: 27,
                    y: 28,
                },
                128,
                &[patch],
            )
            .unwrap();
        let mut supported_banks = 0;
        for sample in &tile.vertices {
            if sample.river == 0.0 && sample.water_body_id.is_some_and(|id| id >= RIVER_ID_BASE) {
                supported_banks += 1;
                assert_eq!(sample.water_m, sample.height_m);
            }
        }
        assert!(supported_banks > 0);
    }
}
