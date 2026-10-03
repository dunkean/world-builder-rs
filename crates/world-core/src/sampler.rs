//! Continuous local reconstruction of the frozen physical atlas. Water bodies
//! provide their own hydraulic datum; interpolated wet fractions never flood
//! dry terrain. Detail is addressed by global position and protected by channels.

use crate::{geometry, noise, Biome, Sample, Tile, TileKey, World};
use anyhow::{bail, Result};
use geometry::{direction, dot, normalize, FACE_BASES};
use rayon::prelude::*;

const RIVER_ID_BASE: u64 = 1_u64 << 52;

struct Patch {
    atlas_direction: [f64; 3],
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
        let patch = coastal_reconstruction(self, p);
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

        let shoreline_datum = water_datum(self, &patch);
        let datum = shoreline_datum;
        let amplitude = detail_amplitude(self, &patch, erodability, patch.height);
        let natural = patch.height
            + if amplitude > 0.0 {
                fractal_detail(self, p, erodability) * amplitude
            } else {
                0.0
            };
        let natural = lake_divide_height(self, &patch, natural, amplitude);
        let mut height = natural;
        let mut channel: Option<(Channel, f64)> = None;
        let mut dry_channel: Option<(Channel, f64)> = None;
        let mut dry_weight = 1.0;
        let mut dry_wet_mask = 1.0;
        let mut channel_weight = 0.0;
        let mut channel_beds = 0.0;
        let mut channel_depths = 0.0;
        for &index in &patch.nearby[..patch.nearby_count] {
            let Some(candidate) = channel_at(self, index, p, patch.spacing_m) else {
                continue;
            };
            let carved = carve(candidate, natural);
            height = height.min(carved);
            let radial = candidate.distance_m / candidate.half_width_m;
            let wet_shape = (1.0 - radial * radial).max(0.0).powi(2);
            let bank_width = candidate.half_width_m * 0.65 + 1.0;
            let weight = 1.0
                - smooth(
                    ((candidate.distance_m - candidate.half_width_m) / bank_width).clamp(0.0, 1.0),
                );
            if weight > 0.0 {
                // Reconstruct bed and hydraulic depth with the same partition.
                // A single reach retains its exact quartic wet section. Dry
                // banks contribute a signed potential, so equal river planes
                // remain equal instead of gaining a bump where sections overlap.
                dry_weight *= 1.0 - weight;
                dry_wet_mask *= 1.0 - wet_shape;
                channel_weight += weight;
                let bed = natural.min(carved);
                let signed_depth = if wet_shape > 0.0 {
                    candidate.depth_m * wet_shape
                } else {
                    (candidate.water_level_m - bed).min(0.0)
                };
                channel_beds += weight * bed;
                channel_depths += weight * signed_depth;
                if wet_shape > 0.0 {
                    discharge = discharge.max(candidate.discharge);
                    if channel.is_none_or(|(old, old_weight)| {
                        wet_shape > old_weight || (wet_shape == old_weight && candidate.id < old.id)
                    }) {
                        channel = Some((candidate, wet_shape));
                    }
                } else if radial < 1.1
                    && dry_channel.is_none_or(|(old, old_radius)| {
                        radial < old_radius || (radial == old_radius && candidate.id < old.id)
                    })
                {
                    dry_channel = Some((candidate, radial));
                }
            }
        }
        let coverage = 1.0 - dry_weight;
        let depth = if channel_weight > 0.0 {
            let scale = coverage / channel_weight;
            height = dry_weight * height + scale * channel_beds;
            (scale * channel_depths).max(0.0)
        } else {
            0.0
        };

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
            if let Some((channel, _)) = channel.filter(|_| depth > 0.0) {
                water_level = Some(height + depth);
                water_id = Some(channel.id);
                river = 1.0 - dry_wet_mask;
            } else if let Some((channel, _)) = dry_channel.filter(|_| height >= 0.0) {
                // Metadata only: dry support cannot create water or override a
                // marine fallback below zero. Both geometric fields stay dry.
                water_level = Some(height);
                water_id = Some(channel.id);
            }
        }
        if height < 0.0 && water_id.is_none_or(|id| id == 0) {
            if let Some(owner) = lake_datum(self, &patch) {
                // The canonical reservoir owns its dry submerged fringes too.
                // A nearby sea stencil must not fill an endorheic basin up to
                // zero, and a coastal fallback must not erase its water budget.
                ocean = false;
                lake = height < owner.level;
                water_level = Some(owner.level);
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
        atlas_direction: p,
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
                if !result.nearby[..result.nearby_count].contains(&index) {
                    result.nearby[result.nearby_count] = index;
                    result.nearby_count += 1;
                }
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

fn water_datum(world: &World, patch: &Patch) -> Option<WaterDatum> {
    // One metric owns all reservoir decisions, including dry subsea fringes.
    // A local marine stencil cannot override a canonical closed basin.
    if let Some(lake) = lake_datum(world, patch) {
        return Some(lake);
    }
    let p = patch.atlas_direction;
    let mut nearest = f64::INFINITY;
    let mut result = None;
    for &index in &patch.nearby[..patch.nearby_count] {
        let cell = &world.cells[index];
        if !cell.ocean {
            continue;
        }
        let distance2 = (p[0] - cell.position[0]).powi(2)
            + (p[1] - cell.position[1]).powi(2)
            + (p[2] - cell.position[2]).powi(2);
        if distance2 < nearest {
            nearest = distance2;
            result = Some(WaterDatum { level: 0.0, id: 0 });
        }
    }
    result
}

fn lake_datum(world: &World, patch: &Patch) -> Option<WaterDatum> {
    let (supports, count) = crate::lake_geometry::supports(
        world,
        &patch.nearby[..patch.nearby_count],
        patch.atlas_direction,
    );
    supports[..count]
        .iter()
        .flatten()
        .filter(|s| s.radius < 1.0)
        .min_by(|a, b| a.radius.total_cmp(&b.radius).then(a.id.cmp(&b.id)))
        .map(|support| WaterDatum {
            level: support.level,
            id: support.id,
        })
}

fn smooth(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

// The same invertible spherical map is evaluated at every LOD. A coastline is
// a level set of the frozen atlas in this domain, never a camera-local noise mask.
fn coastal_reconstruction(world: &World, p: [f64; 3]) -> Patch {
    reconstruction(world, crate::coast::remap(&world.config, p))
}

fn detail_amplitude(world: &World, patch: &Patch, erodability: f64, height: f64) -> f64 {
    let nominal = world.config.detail_m * (1.3 - 0.4 * erodability).clamp(0.3, 1.4);
    let mut lake_weight = 0.0;
    let mut protected = 0.0;
    for &(index, weight) in &patch.attributes[..patch.attribute_count] {
        let cell = &world.cells[index];
        let plane = if cell.lake && !cell.ocean {
            Some(f64::from(cell.water_m))
        } else {
            crate::lake_geometry::control_level(world, index)
        };
        if let Some(plane) = plane {
            lake_weight += weight;
            let depth = (patch.height - plane).abs();
            protected += weight * nominal.min(3.0 + 0.12 * depth).min(0.8 * depth);
        }
    }
    // Protection fades continuously at the support boundary. Choosing a single
    // lake datum and abruptly changing amplitude there produced square cliffs.
    let amplitude = if lake_weight > 1e-12 {
        let influence = smooth((lake_weight / 0.25).clamp(0.0, 1.0));
        nominal * (1.0 - influence) + (protected / lake_weight) * influence
    } else {
        nominal
    };
    // Every normalized noise band is bounded by one. A strict fraction of the
    // natural height leaves its sign unchanged, including tiny coastal ridges.
    amplitude.min(height.abs() * 0.8)
}

/// Separate neighbouring reservoirs whose unresolved divide was lost by the
/// coarse cubic reconstruction. Their hydraulic planes stay horizontal; a
/// compact, continuous terrain ridge makes the ownership boundary dry.
fn lake_divide_height(world: &World, patch: &Patch, natural: f64, detail_bound: f64) -> f64 {
    let (supports, count) = crate::lake_geometry::supports(
        world,
        &patch.nearby[..patch.nearby_count],
        patch.atlas_direction,
    );
    let mut height = natural;
    let width = 0.18;
    let nearest_radius = supports[..count]
        .iter()
        .flatten()
        .map(|s| s.radius)
        .fold(f64::INFINITY, f64::min);
    for support in supports[..count].iter().flatten() {
        let distance = (support.radius - 1.0).abs();
        if distance < 0.14 {
            // A closed reservoir below sea level also has to retain the global
            // marine fallback outside its domain, not just its own lower plane.
            let crest = support.level.max(0.0) + 0.25;
            let ownership =
                1.0 - smooth(((support.radius - nearest_radius) / 0.14).clamp(0.0, 1.0));
            // Lift the reconstructed base rather than flattening individual
            // noise troughs. The known detail bound keeps the exact divide dry
            // while preserving its spatial variation.
            let lift = (crest + detail_bound - patch.height).max(0.0);
            height = height.max(natural + lift * (1.0 - smooth(distance / 0.14)) * ownership);
        }
    }
    for a in 0..count {
        for b in a + 1..count {
            let a = supports[a].unwrap();
            let b = supports[b].unwrap();
            if (a.level - b.level).abs() < 0.02 {
                continue;
            }
            let gap = (a.radius - b.radius).abs();
            if gap >= width {
                continue;
            }
            let crest = a.level.max(b.level) + 0.25;
            // Continue the support past the quad border: adding a wet corner
            // with tiny interpolation weight must not remove a retaining ridge.
            let support = (1.0 - smooth(((a.radius - 1.0) / 0.3).clamp(0.0, 1.0)))
                * (1.0 - smooth(((b.radius - 1.0) / 0.3).clamp(0.0, 1.0)));
            let ownership =
                1.0 - smooth(((a.radius.max(b.radius) - nearest_radius) / 0.14).clamp(0.0, 1.0));
            let influence = (1.0 - smooth(gap / width)) * support * ownership;
            height =
                height.max(natural + (crest + detail_bound - patch.height).max(0.0) * influence);
        }
    }
    height
}

pub(crate) fn natural_height(world: &World, p: [f64; 3]) -> f64 {
    let p = normalize(p);
    let patch = coastal_reconstruction(world, p);
    let erodability = patch.attributes[..patch.attribute_count]
        .iter()
        .map(|&(index, weight)| weight * f64::from(world.cells[index].erodability))
        .sum::<f64>();
    let amplitude = detail_amplitude(world, &patch, erodability, patch.height);
    let natural = patch.height + fractal_detail(world, p, erodability) * amplitude;
    lake_divide_height(world, &patch, natural, amplitude)
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

fn channel_at(world: &World, index: usize, p: [f64; 3], spacing: f64) -> Option<Channel> {
    let reach = world.river_reach(index)?;
    channel_from_reach(world, reach, p, spacing)
}

fn channel_from_reach(
    world: &World,
    reach: &crate::river_geometry::RiverReach,
    p: [f64; 3],
    spacing: f64,
) -> Option<Channel> {
    Some(channel_from_projection(
        reach,
        river_projection(world, reach, p, spacing)?,
    ))
}

/// Geometry stays fixed during longitudinal profile fitting. Keep its expensive
/// closest-point projection separate from the changing hydraulic stage.
#[derive(Clone, Copy)]
pub(crate) struct RiverProjection {
    distance_m: f64,
    t: f64,
    valley_width_m: f64,
}

pub(crate) fn river_projection(
    world: &World,
    reach: &crate::river_geometry::RiverReach,
    p: [f64; 3],
    spacing: f64,
) -> Option<RiverProjection> {
    let cell = &world.cells[reach.from];
    let spacing = cell.area_m2.sqrt().max(spacing * 0.1);
    let widest = reach.width_start_m.max(reach.width_end_m) * 0.5;
    // Coarse atlas spacing must not turn every routed edge into a kilometre-wide
    // canyon. The support follows the actual hydraulic width while preserving
    // a complete channel, banks and floodplain even for unusually broad rivers.
    let valley_width = (widest * 6.0 + 30.0)
        .max(80.0)
        .min(spacing * 0.35)
        .max(widest * 5.0 + 20.0);
    let delta = std::array::from_fn::<_, 3, _>(|j| reach.end[j] - reach.start[j]);
    let length2 = dot(delta, delta);
    if length2 <= 1e-24 {
        return None;
    }
    let t = (dot(std::array::from_fn(|j| p[j] - reach.start[j]), delta) / length2).clamp(0.0, 1.0);
    let near = normalize(std::array::from_fn(|j| reach.start[j] + t * delta[j]));
    let distance = dot(
        std::array::from_fn(|j| p[j] - near[j]),
        std::array::from_fn(|j| p[j] - near[j]),
    )
    .sqrt()
        * world.config.radius_m;
    if distance > valley_width + length2.sqrt() * world.config.radius_m * 0.4 {
        return None;
    }
    let (distance_m, t) = reach.closest(p);
    if distance_m >= valley_width {
        return None;
    }
    Some(RiverProjection {
        distance_m,
        t,
        valley_width_m: valley_width,
    })
}

fn channel_from_projection(
    reach: &crate::river_geometry::RiverReach,
    projection: RiverProjection,
) -> Channel {
    let RiverProjection {
        distance_m,
        t,
        valley_width_m,
    } = projection;
    Channel {
        distance_m,
        half_width_m: reach.width(t) * 0.5,
        valley_width_m,
        water_level_m: reach.level(t),
        depth_m: reach.depth(t),
        discharge: reach.discharge_m3_s,
        id: RIVER_ID_BASE + reach.from as u64 + 1,
    }
}

fn carve(candidate: Channel, natural: f64) -> f64 {
    // Preserve the complete hydraulic section and floodplain. Only the outer
    // valley blends back into the natural terrain, with the same C1 kernel
    // during catalogue fitting and actual terrain reconstruction.
    let floodplain = (3.4 * candidate.half_width_m + 7.0).min(candidate.valley_width_m * 0.65);
    // A shallow alluvial valley retains its full floodplain. Where a coarse
    // river crosses a much higher reconstructed ridge, flattening that whole
    // strip creates an artificial canyon and a steep recovery at its border.
    // Keep the wet section exact, but restore the natural sides over more of
    // the available dry support instead of carving a deep flat terrace.
    let dry_span = (candidate.valley_width_m - candidate.half_width_m).max(1.0);
    let incision = (natural - candidate.water_level_m).max(0.0);
    let flatness = 1.0 - smooth((incision / (dry_span * 0.6)).clamp(0.0, 1.0));
    let floodplain = candidate.half_width_m + (floodplain - candidate.half_width_m) * flatness;
    let influence = smooth(
        ((candidate.valley_width_m - candidate.distance_m)
            / (candidate.valley_width_m - floodplain))
            .clamp(0.0, 1.0),
    );
    let wall_slope = ((natural - candidate.water_level_m).max(0.0)
        / (candidate.valley_width_m * 0.7))
        .clamp(0.015, 1.2);
    let target = crate::river_geometry::bank_height(
        candidate.distance_m,
        candidate.half_width_m * 2.0,
        candidate.depth_m,
        candidate.water_level_m,
        wall_slope,
    );
    natural * (1.0 - influence) + target * influence
}

/// Bounded stencil in the same atlas domain as sample(), without river-cache
/// access. Catalogue fitting can inspect neighbours while its cache is built.
pub(crate) fn river_neighbourhood(world: &World, p: [f64; 3]) -> ([usize; 48], usize, f64) {
    let patch = coastal_reconstruction(world, normalize(p));
    (patch.nearby, patch.nearby_count, patch.spacing_m)
}

/// Receiver/tributary surface for profile fitting. Inside another wet section
/// cap against its free surface, never its bed: a mutual bed cap would lower
/// both reaches by another depth on every iteration. No own reach is included
/// by the caller, and no compiled river-cache is consulted here.
pub(crate) fn other_river_surface_from_projection(
    reach: &crate::river_geometry::RiverReach,
    projection: RiverProjection,
    natural: f64,
) -> f64 {
    let candidate = channel_from_projection(reach, projection);
    let surface = carve(candidate, natural);
    if candidate.distance_m < candidate.half_width_m {
        surface.max(candidate.water_level_m)
    } else {
        surface
    }
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
            coast_detail_ratio: 0.0,
            ..Default::default()
        })
        .unwrap();
        world.lakes.clear();
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
        world.invalidate_geometry();
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
    fn lake_surface_does_not_jump_at_a_dominant_cell_switch_near_a_dry_valley() {
        let mut world = synthetic();
        let previous_radius = world.config.radius_m;
        world.config.radius_m = 10_000.0;
        for cell in &mut world.cells {
            cell.area_m2 *= (world.config.radius_m / previous_radius).powi(2);
        }
        let lake = geometry::cell_index(4, 3, 3, 8);
        let river = geometry::cell_index(4, 3, 4, 8);
        let downstream = geometry::cell_index(4, 4, 4, 8);
        world.cells[lake].height_m = 60.0;
        world.cells[lake].water_m = 100.0;
        world.cells[lake].lake = true;
        world.cells[lake].lake_id = Some(17);
        world.cells[river].height_m = 60.0;
        world.cells[river].water_m = 60.0;
        world.cells[river].discharge = 30_000.0;
        world.cells[river].receiver = Some(downstream);
        world.cells[downstream].height_m = 40.0;
        world.cells[downstream].water_m = 40.0;
        world.cells[downstream].discharge = 30_000.0;
        world.invalidate_geometry();
        let a = world.sample(direction(4, -0.125, -1e-8));
        let b = world.sample(direction(4, -0.125, 1e-8));
        assert!(a.lake && b.lake);
        assert_eq!(a.water_body_id, Some(17));
        assert_eq!(b.water_body_id, Some(17));
        assert_eq!(a.water_m, 100.0);
        assert_eq!(b.water_m, 100.0);
        assert!((a.height_m - b.height_m).abs() < 0.001);
    }

    #[test]
    fn different_lake_planes_have_a_continuous_dry_divide() {
        let mut world = synthetic();
        let a = geometry::cell_index(4, 3, 3, 8);
        let b = geometry::cell_index(4, 4, 3, 8);
        for (index, level, id) in [(a, 100.0, 17), (b, 110.0, 18)] {
            world.cells[index].height_m = 60.0;
            world.cells[index].water_m = level;
            world.cells[index].lake = true;
            world.cells[index].lake_id = Some(id);
        }
        world.invalidate_geometry();
        for (index, level) in [(a, 100.0), (b, 110.0)] {
            let sample = world.sample(world.cells[index].position);
            assert!(sample.lake);
            assert_eq!(sample.height_m, 60.0);
            assert_eq!(sample.water_m, level);
        }
        let before = world.sample(direction(4, -1e-8, -0.125));
        let after = world.sample(direction(4, 1e-8, -0.125));
        assert!(!before.lake && !after.lake);
        assert!(before.height_m > 110.0 && after.height_m > 110.0);
        assert!((before.height_m - after.height_m).abs() < 0.001);
        for i in 0..=1000 {
            let sample = world.sample(direction(4, -0.125 + i as f64 * 0.00025, -0.125));
            if sample.lake {
                assert_eq!(
                    sample.water_m,
                    if sample.water_body_id == Some(17) {
                        100.0
                    } else {
                        110.0
                    }
                );
            }
        }
    }

    #[test]
    fn unequal_cell_areas_use_the_same_metric_for_water_and_divides() {
        let mut world = synthetic();
        for cell in &mut world.cells {
            cell.height_m = 60.0;
            cell.water_m = 60.0;
        }
        for (x, level, id, area_scale) in [(3, 100.0, 17, 0.49), (4, 110.0, 18, 1.44)] {
            let cell = &mut world.cells[geometry::cell_index(4, x, 3, 8)];
            cell.water_m = level;
            cell.lake = true;
            cell.lake_id = Some(id);
            cell.area_m2 *= area_scale;
        }
        world.invalidate_geometry();
        let mut previous: Option<Sample> = None;
        let mut owners = [false; 2];
        for i in 0..=2000 {
            let sample = world.sample(direction(4, -0.125 + i as f64 * 0.000125, -0.125));
            if sample.lake {
                owners[usize::from(sample.water_body_id == Some(18))] = true;
            }
            if let Some(old) = previous {
                assert!(
                    !(old.lake && sample.lake && old.water_body_id != sample.water_body_id),
                    "two horizontal planes touched at unequal-area ownership boundary, i={i}"
                );
            }
            previous = Some(sample);
        }
        assert_eq!(owners, [true, true]);
    }

    #[test]
    fn a_non_owner_lake_pair_cannot_raise_a_fin_inside_a_third_lake() {
        let mut world = synthetic();
        for cell in &mut world.cells {
            cell.height_m = 60.0;
            cell.water_m = 60.0;
        }
        for (x, y, level, id) in [(3, 3, 100.0, 17), (4, 3, 110.0, 18), (3, 4, 120.0, 19)] {
            let cell = &mut world.cells[geometry::cell_index(4, x, y, 8)];
            cell.water_m = level;
            cell.lake = true;
            cell.lake_id = Some(id);
        }
        world.invalidate_geometry();
        for delta in [0.025, 0.05] {
            let sample = world.sample(direction(4, -0.125 + delta, -0.125 + delta));
            assert!(sample.lake);
            assert_eq!(sample.water_body_id, Some(17));
            assert_eq!(sample.height_m, 60.0);
            assert_eq!(sample.water_m, 100.0);
        }
    }

    #[test]
    fn subsea_reservoir_support_has_no_quad_fallback_outside_its_metric_domain() {
        let mut world = synthetic();
        for cell in &mut world.cells {
            cell.height_m = -120.0;
            cell.water_m = -120.0;
        }
        let index = geometry::cell_index(4, 3, 3, 8);
        world.cells[index].water_m = -80.0;
        world.cells[index].lake = true;
        world.cells[index].lake_id = Some(17);
        world.lakes.push(crate::hydrology::Lake {
            id: 17,
            level_m: -80.0,
            cells: vec![index],
            ..Default::default()
        });
        world.invalidate_geometry();
        let center = world.cells[index].position;
        let east = normalize(geometry::cross([0.0, 1.0, 0.0], center));
        let north = geometry::cross(center, east);
        let angular_radius = 0.85 * world.cells[index].area_m2.sqrt() / world.config.radius_m;
        for i in 0..32 {
            let phi = i as f64 * std::f64::consts::TAU / 32.0;
            let angle = angular_radius * 1.12;
            let p = normalize(std::array::from_fn(|axis| {
                center[axis] * angle.cos()
                    + (east[axis] * phi.cos() + north[axis] * phi.sin()) * angle.sin()
            }));
            let patch = coastal_reconstruction(&world, p);
            assert!(
                lake_datum(&world, &patch).is_none(),
                "quad fallback at phi={phi}"
            );
            assert!(!world.sample(p).lake);
        }
    }

    #[test]
    fn bounded_metric_stencil_matches_full_reservoir_search_at_cube_seams() {
        let mut world = synthetic();
        for cell in &mut world.cells {
            cell.height_m = 60.0;
            cell.water_m = 100.0;
            cell.lake = true;
            cell.lake_id = Some(17);
        }
        world.invalidate_geometry();
        let all: Vec<_> = (0..world.cells.len()).collect();
        for face in 0..6 {
            for u in [-1.00000001, -1.0, -0.75, 0.0, 0.75, 1.0, 1.00000001] {
                for v in [-1.00000001, -1.0, -0.75, 0.0, 0.75, 1.0, 1.00000001] {
                    let p = direction(face, u, v);
                    let patch = coastal_reconstruction(&world, p);
                    let (bounded, n) = crate::lake_geometry::supports(
                        &world,
                        &patch.nearby[..patch.nearby_count],
                        p,
                    );
                    let (full, m) = crate::lake_geometry::supports(&world, &all, p);
                    assert_eq!(n, m, "missing support at face={face}, u={u}, v={v}");
                    assert!((bounded[0].unwrap().radius - full[0].unwrap().radius).abs() < 1e-12);
                }
            }
        }
    }

    #[test]
    fn retaining_terrain_keeps_detail_instead_of_clamping_a_flat_ring() {
        let mut world = synthetic();
        for cell in &mut world.cells {
            cell.height_m = 60.0;
            cell.water_m = 60.0;
        }
        let index = geometry::cell_index(4, 3, 3, 8);
        world.cells[index].water_m = 100.0;
        world.cells[index].lake = true;
        world.cells[index].lake_id = Some(17);
        world.invalidate_geometry();
        let center = world.cells[index].position;
        let east = normalize(geometry::cross([0.0, 1.0, 0.0], center));
        let chord = 0.85 * world.cells[index].area_m2.sqrt() / world.config.radius_m;
        let angle = 2.0 * (chord * 0.5).asin();
        let p = normalize(std::array::from_fn(|axis| {
            center[axis] * angle.cos() + east[axis] * angle.sin()
        }));
        let patch = coastal_reconstruction(&world, p);
        let low = lake_divide_height(&world, &patch, 59.0, 1.0);
        let high = lake_divide_height(&world, &patch, 61.0, 1.0);
        assert!((high - low - 2.0).abs() < 1e-12);
        assert!(low >= 100.25 - 1e-10);
    }

    #[test]
    fn fine_detail_cannot_erase_a_frozen_dry_lake_spill_control() {
        let mut world = synthetic();
        world.config.detail_m = 500.0;
        let control = geometry::cell_index(4, 3, 3, 8);
        let outlet = geometry::cell_index(4, 4, 3, 8);
        world.cells[control].height_m = 60.0;
        world.cells[control].water_m = 100.0;
        world.cells[control].lake = true;
        world.cells[control].lake_id = Some(17);
        world.cells[outlet].height_m = 100.0;
        world.cells[outlet].water_m = 100.0;
        world.lakes.push(crate::hydrology::Lake {
            id: 17,
            level_m: 100.0,
            cells: vec![control],
            control_cell: control,
            outlet: Some(outlet),
            outflow_m3_s: 600.0,
            ..Default::default()
        });
        for seed in 0..64 {
            world.config.seed = seed;
            world.invalidate_geometry();
            let height = natural_height(&world, world.cells[outlet].position);
            assert!(
                (height - 100.0).abs() < 1e-9,
                "spill erased at seed={seed}: {height}"
            );
        }
    }

    #[test]
    fn subsea_lake_and_marine_fallback_are_separated_by_retaining_terrain() {
        let mut world = synthetic();
        for cell in &mut world.cells {
            cell.height_m = -120.0;
            cell.water_m = -120.0;
        }
        let index = geometry::cell_index(4, 3, 3, 8);
        world.cells[index].water_m = -80.0;
        world.cells[index].lake = true;
        world.cells[index].lake_id = Some(17);
        world.invalidate_geometry();
        let center = world.cells[index].position;
        let east = normalize(geometry::cross([0.0, 1.0, 0.0], center));
        let angular_radius = 0.85 * world.cells[index].area_m2.sqrt() / world.config.radius_m;
        let mut previous: Option<Sample> = None;
        let mut dry = 0;
        for i in 0..=1000 {
            let angle = angular_radius * (0.8 + i as f64 * 0.0004);
            let p = normalize(std::array::from_fn(|axis| {
                center[axis] * angle.cos() + east[axis] * angle.sin()
            }));
            let sample = world.sample(p);
            if sample.water_m == sample.height_m {
                dry += 1;
            }
            if let Some(old) = previous {
                let both_wet = old.water_m > old.height_m && sample.water_m > sample.height_m;
                assert!(!(both_wet && old.water_body_id != sample.water_body_id));
            }
            previous = Some(sample);
        }
        assert!(dry > 0);
    }

    #[test]
    fn fine_detail_cannot_dry_a_shallow_canonical_lake_cell() {
        let mut world = synthetic();
        world.config.detail_m = 500.0;
        let index = geometry::cell_index(4, 3, 3, 8);
        world.cells[index].height_m = 99.99;
        world.cells[index].water_m = 100.0;
        world.cells[index].lake = true;
        world.cells[index].lake_id = Some(17);
        for seed in 0..64 {
            world.config.seed = seed;
            world.invalidate_geometry();
            let sample = world.sample(world.cells[index].position);
            assert!(sample.lake, "detail dried lake at seed={seed}");
            assert!(sample.height_m < 100.0);
            assert_eq!(sample.water_m, 100.0);
        }
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
    fn river_wet_section_matches_the_shared_bank_profile() {
        let mut world = synthetic();
        let a = geometry::cell_index(4, 3, 3, 8);
        let b = geometry::cell_index(4, 4, 3, 8);
        world.cells[a].receiver = Some(b);
        world.cells[a].discharge = 1000.0;
        world.cells[b].discharge = 1000.0;
        let reach = world.river_reach(a).unwrap();
        let p = reach.position(0.5);
        let along = normalize(std::array::from_fn(|i| reach.end[i] - reach.start[i]));
        let across = normalize(geometry::cross(p, along));
        let width = reach.width(0.5);
        let depth = reach.depth(0.5);
        for fraction in [-0.995_f64, -0.9, -0.5, 0.0, 0.5, 0.9, 0.995] {
            let offset = fraction * width * 0.5;
            let q = normalize(std::array::from_fn(|i| {
                p[i] + across[i] * offset / world.config.radius_m
            }));
            let sample = world.sample(q);
            let expected =
                crate::river_geometry::bank_height(offset.abs(), width, depth, 160.0, 0.015);
            assert!((f64::from(sample.height_m) - expected).abs() < 0.002);
            assert!(sample.water_m > sample.height_m);
            assert!((sample.water_m - 160.0).abs() < 0.0001);
            assert!(sample.river > 0.0);
        }
    }

    #[test]
    fn coarse_atlas_spacing_does_not_spread_a_small_river_across_the_continent() {
        let mut world = synthetic();
        let a = geometry::cell_index(4, 3, 3, 8);
        let b = geometry::cell_index(4, 4, 3, 8);
        world.cells[a].receiver = Some(b);
        world.cells[a].discharge = 1000.0;
        world.cells[b].discharge = 1000.0;
        let reach = crate::river_geometry::reach(&world, a).unwrap();
        let center = reach.position(0.5);
        let along = normalize(std::array::from_fn(|i| reach.end[i] - reach.start[i]));
        let across = normalize(geometry::cross(center, along));
        let at = |distance: f64| {
            normalize(std::array::from_fn(|i| {
                center[i] + across[i] * distance / world.config.radius_m
            }))
        };
        let width = reach.width(0.5);
        let spacing = world.cells[a].area_m2.sqrt();
        assert!(river_projection(&world, &reach, at(width * 0.49), spacing).is_some());
        assert!(river_projection(&world, &reach, at(width * 2.0), spacing).is_some());
        assert!(river_projection(&world, &reach, at(width * 20.0), spacing).is_none());
    }

    #[test]
    fn converging_rivers_keep_ground_and_water_continuous_across_wet_edges() {
        let mut world = synthetic();
        let old_radius = world.config.radius_m;
        world.config.radius_m = 10_000.0;
        for cell in &mut world.cells {
            cell.area_m2 *= (world.config.radius_m / old_radius).powi(2);
        }
        let high = geometry::cell_index(4, 3, 3, 8);
        let low = geometry::cell_index(4, 4, 3, 8);
        let join = geometry::cell_index(4, 4, 4, 8);
        for (index, height, receiver) in [
            (high, 160.0, Some(join)),
            (low, 110.0, Some(join)),
            (join, 100.0, None),
        ] {
            world.cells[index].height_m = height;
            world.cells[index].water_m = height;
            world.cells[index].receiver = receiver;
            world.cells[index].discharge = 30_000.0;
        }
        world.invalidate_geometry();
        let reach = world.river_reach(high).unwrap();
        let depth_bound = reach
            .depth(0.0)
            .max(world.river_reach(low).unwrap().depth(0.0));
        let mut boundaries = 0;
        let state = |s: &Sample| (s.water_body_id, s.water_m > s.height_m + 0.0001);
        for step in 0..=8 {
            let t = 0.65 + step as f64 * 0.04;
            let center = reach.position(t);
            let along = normalize(std::array::from_fn(|i| {
                reach.position((t + 0.001).min(1.0))[i] - reach.position((t - 0.001).max(0.0))[i]
            }));
            let across = normalize(geometry::cross(center, along));
            let extent = reach.width(t) * 2.0;
            let at = |offset: f64| {
                normalize(std::array::from_fn(|i| {
                    center[i] + across[i] * offset / world.config.radius_m
                }))
            };
            let mut lo = -extent;
            let mut previous = world.sample(at(lo));
            for i in 1..=200 {
                let hi = extent * (-1.0 + 2.0 * i as f64 / 200.0);
                let sample = world.sample(at(hi));
                assert!(f64::from(sample.water_m - sample.height_m) <= depth_bound + 0.02);
                if state(&sample) != state(&previous) {
                    let mut left = lo;
                    let mut right = hi;
                    let before_state = state(&previous);
                    for _ in 0..32 {
                        if right - left < 0.001 {
                            break;
                        }
                        let middle = 0.5 * (left + right);
                        if state(&world.sample(at(middle))) == before_state {
                            left = middle;
                        } else {
                            right = middle;
                        }
                    }
                    let a = world.sample(at(left));
                    let b = world.sample(at(right));
                    assert!(
                        (a.height_m - b.height_m).abs() < 0.01,
                        "ground step at t={t}: {} -> {}",
                        a.height_m,
                        b.height_m
                    );
                    assert!(
                        (a.water_m - b.water_m).abs() < 0.05,
                        "water step at t={t}: {} -> {}",
                        a.water_m,
                        b.water_m
                    );
                    boundaries += 1;
                }
                previous = sample;
                lo = hi;
            }
        }
        assert!(
            boundaries > 8,
            "fixture did not cross enough hydraulic interfaces"
        );
    }

    #[test]
    fn overlapping_rivers_with_the_same_stage_cannot_raise_the_water_plane() {
        let mut world = synthetic();
        let old_radius = world.config.radius_m;
        world.config.radius_m = 10_000.0;
        for cell in &mut world.cells {
            cell.area_m2 *= (world.config.radius_m / old_radius).powi(2);
        }
        let a = geometry::cell_index(4, 3, 3, 8);
        let b = geometry::cell_index(4, 4, 3, 8);
        let join = geometry::cell_index(4, 4, 4, 8);
        for index in [a, b] {
            world.cells[index].receiver = Some(join);
            world.cells[index].discharge = 30_000.0;
        }
        world.cells[join].discharge = 60_000.0;
        world.invalidate_geometry();
        let reach = world.river_reach(a).unwrap();
        let mut wet = 0;
        for step in 0..=16 {
            let t = 0.6 + step as f64 * 0.025;
            let center = reach.position(t);
            let along = normalize(std::array::from_fn(|i| reach.end[i] - reach.start[i]));
            let across = normalize(geometry::cross(center, along));
            for side in -20..=20 {
                let offset = reach.width(t) * side as f64 / 20.0;
                let p = normalize(std::array::from_fn(|i| {
                    center[i] + across[i] * offset / world.config.radius_m
                }));
                let sample = world.sample(p);
                assert!(
                    sample.water_m <= 160.0001,
                    "common plane raised to {}",
                    sample.water_m
                );
                if sample.water_m > sample.height_m + 0.0001 {
                    wet += 1;
                    assert!(
                        (sample.water_m - 160.0).abs() < 0.0001,
                        "common plane fell to {}",
                        sample.water_m
                    );
                }
            }
        }
        assert!(wet > 100);
    }

    #[test]
    fn fine_vertical_detail_cannot_flip_the_canonical_coastline() {
        let mut world = synthetic();
        world.config.detail_m = 140.0;
        for cell in &mut world.cells {
            cell.height_m = (800.0 * cell.position[0]) as f32;
            cell.ocean = cell.height_m < 0.0;
            cell.water_m = if cell.ocean { 0.0 } else { cell.height_m };
        }
        for y in 0..31 {
            for x in 0..61 {
                let p = direction(
                    4,
                    (f64::from(x) - 30.0) * 0.0047,
                    (f64::from(y) - 15.0) * 0.025,
                );
                let base = coastal_reconstruction(&world, p).height;
                let fine = natural_height(&world, p);
                assert_eq!(base.is_sign_positive(), fine.is_sign_positive());
                assert!((fine - base).abs() <= base.abs() * 0.8 + 1e-9);
            }
        }
    }

    #[test]
    fn lake_detail_protection_fades_across_the_quad_boundary() {
        let mut world = synthetic();
        world.config.detail_m = 140.0;
        world.config.coast_detail_ratio = 0.0;
        for cell in &mut world.cells {
            cell.height_m = 60.0;
            cell.water_m = 60.0;
        }
        let index = geometry::cell_index(4, 3, 3, 8);
        world.cells[index].lake = true;
        world.cells[index].water_m = 100.0;
        world.cells[index].lake_id = Some(17);
        let left = world.sample(direction(4, 0.125 - 1e-9, -0.125));
        let right = world.sample(direction(4, 0.125 + 1e-9, -0.125));
        assert!((left.height_m - right.height_m).abs() < 0.001);
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
            let p = world.river_reach(cells[0]).unwrap().position(t);
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
        let mut previous = 100.0_f64;
        for i in 1..=40 {
            let t = f64::from(i) / 40.0;
            let reach = world.river_reach(outlet).unwrap();
            let p = reach.position(t);
            let sample = world.sample(p);
            assert!(!sample.lake, "lake flooded river at t={t}");
            assert!(sample.river > 0.99);
            assert!(sample.water_body_id.unwrap() >= RIVER_ID_BASE);
            assert!((f64::from(sample.water_m) - reach.level(t)).abs() < 0.1);
            assert!(reach.level(t) <= previous + 1e-6);
            previous = reach.level(t);
        }
        assert!((previous - 60.0).abs() < 1e-6);
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
