//! Fixed metric reservoir domains. Basin labels include dry fringes, while the
//! actual water shore is the intersection of the terrain and its horizontal plane.
use crate::{geometry::dot, World};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug)]
struct Body {
    id: u64,
    lake_index: Option<usize>,
    source_cell: usize,
}

#[derive(Debug)]
struct Guard {
    label: u32,
    cell: usize,
    radius_m: f64,
}

const GUARD_HALO_M: f64 = 20_000.0;
fn guard_key(p: [f64; 3], radius: f64, spacing: f64) -> [i64; 3] {
    p.map(|v| (v * radius / spacing).floor() as i64)
}

#[derive(Debug)]
pub(crate) struct LakeDomains {
    labels: Box<[u32]>,
    bodies: Vec<Body>,
    spill_controls: HashMap<usize, u32>,
    guards: Vec<Guard>,
    guard_buckets: HashMap<[i64; 3], Vec<u32>>,
    guard_spacing_m: f64,
}

#[derive(Clone, Copy)]
pub(crate) struct Support {
    pub id: u64,
    pub level: f64,
    pub radius: f64,
}

impl LakeDomains {
    fn compile(world: &World) -> Self {
        let mut labels = vec![0; world.cells.len()];
        let mut bodies = Vec::new();
        let mut lookup = HashMap::new();
        let mut spill_controls = HashMap::new();
        for (lake_index, lake) in world.lakes.iter().enumerate() {
            let label = bodies.len() as u32 + 1;
            lookup.insert(lake.id, label);
            bodies.push(Body {
                id: lake.id,
                lake_index: Some(lake_index),
                source_cell: lake.control_cell,
            });
            for &index in &lake.cells {
                if let Some(slot) = labels.get_mut(index) {
                    *slot = label;
                }
            }
            if lake.outflow_m3_s > 0.0 {
                for index in [Some(lake.control_cell), lake.outlet].into_iter().flatten() {
                    let existing = spill_controls.get(&index).copied();
                    let replace = existing.is_none_or(|old: u32| {
                        let old_body: &Body = &bodies[old as usize - 1];
                        let old_lake = &world.lakes[old_body.lake_index.unwrap()];
                        lake.level_m > old_lake.level_m
                    });
                    if replace {
                        spill_controls.insert(index, label);
                    }
                }
            }
        }
        for (index, cell) in world
            .cells
            .iter()
            .enumerate()
            .filter(|(_, c)| c.lake && !c.ocean)
        {
            let id = cell
                .lake_id
                .unwrap_or(1 + u64::from(cell.water_m.to_bits()));
            let label = *lookup.entry(id).or_insert_with(|| {
                bodies.push(Body {
                    id,
                    lake_index: None,
                    source_cell: index,
                });
                bodies.len() as u32
            });
            labels[index] = label;
        }
        // A separate spatial index covers full procedural-object support, even
        // when a nearby lake falls outside the bounded interpolation stencil.
        let guard_spacing_m =
            (2.0 * world.config.radius_m / f64::from(world.config.resolution)).max(16_384.0);
        let mut guards = Vec::new();
        for (cell, &label) in labels.iter().enumerate() {
            if label != 0 {
                guards.push(Guard {
                    label,
                    cell,
                    radius_m: 1.3 * 0.85 * world.cells[cell].area_m2.sqrt(),
                });
            }
        }
        let mut controls: Vec<_> = spill_controls.iter().collect();
        controls.sort_unstable_by_key(|(index, _)| **index);
        for (&cell, &label) in controls {
            guards.push(Guard {
                label,
                cell,
                radius_m: 0.8 * world.cells[cell].area_m2.sqrt(),
            });
        }
        let mut guard_buckets: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
        for (i, guard) in guards.iter().enumerate() {
            let cell = &world.cells[guard.cell];
            let extent = (guard.radius_m + GUARD_HALO_M) / world.config.radius_m;
            let lo = guard_key(
                cell.position.map(|v| v - extent),
                world.config.radius_m,
                guard_spacing_m,
            );
            let hi = guard_key(
                cell.position.map(|v| v + extent),
                world.config.radius_m,
                guard_spacing_m,
            );
            for x in lo[0]..=hi[0] {
                for y in lo[1]..=hi[1] {
                    for z in lo[2]..=hi[2] {
                        guard_buckets.entry([x, y, z]).or_default().push(i as u32);
                    }
                }
            }
        }
        Self {
            labels: labels.into_boxed_slice(),
            bodies,
            spill_controls,
            guards,
            guard_buckets,
            guard_spacing_m,
        }
    }

    fn level(&self, world: &World, body: &Body) -> Option<f64> {
        if let Some(index) = body.lake_index {
            // Public synthetic worlds can remove reservoirs. A removed record
            // is not allowed to survive through a previously compiled cache.
            world
                .lakes
                .get(index)
                .filter(|l| l.id == body.id)
                .map(|l| f64::from(l.level_m))
        } else {
            let cell = world.cells.get(body.source_cell)?;
            let id = cell
                .lake_id
                .unwrap_or(1 + u64::from(cell.water_m.to_bits()));
            (cell.lake && !cell.ocean && id == body.id).then_some(f64::from(cell.water_m))
        }
    }
}

/// Clearance from the retaining domain and external spill controls in atlas
/// coordinates. A full-object exclusion can use a conservative metric support;
/// no camera/LOD or procedural terrain cache is involved.
pub(crate) fn geographic_clearance(world: &World, q: [f64; 3]) -> f64 {
    let domains = world
        .hydraulic_cache
        .get_or_init(|| Arc::new(LakeDomains::compile(world)));
    let key = guard_key(q, world.config.radius_m, domains.guard_spacing_m);
    let Some(indices) = domains.guard_buckets.get(&key) else {
        return GUARD_HALO_M;
    };
    let mut clearance = GUARD_HALO_M;
    for &i in indices {
        let guard = &domains.guards[i as usize];
        let body = &domains.bodies[guard.label as usize - 1];
        if domains.level(world, body).is_none() {
            continue;
        }
        let delta = std::array::from_fn(|j| q[j] - world.cells[guard.cell].position[j]);
        clearance =
            clearance.min(dot(delta, delta).sqrt() * world.config.radius_m - guard.radius_m);
    }
    clearance
}

pub(crate) fn control_level(world: &World, index: usize) -> Option<f64> {
    let domains = world
        .hydraulic_cache
        .get_or_init(|| Arc::new(LakeDomains::compile(world)));
    let label = domains.spill_controls.get(&index)?;
    let body = &domains.bodies[*label as usize - 1];
    let lake = world.lakes.get(body.lake_index?)?;
    (lake.id == body.id
        && lake.outflow_m3_s > 0.0
        && (index == lake.control_cell || lake.outlet == Some(index)))
    .then_some(f64::from(lake.level_m))
}

pub(crate) fn supports(
    world: &World,
    nearby: &[usize],
    p: [f64; 3],
) -> ([Option<Support>; 48], usize) {
    let domains = world
        .hydraulic_cache
        .get_or_init(|| Arc::new(LakeDomains::compile(world)));
    let mut result: [Option<Support>; 48] = [None; 48];
    let mut count = 0;
    for &index in nearby {
        let label = domains.labels[index];
        if label == 0 {
            continue;
        }
        let body = &domains.bodies[label as usize - 1];
        let Some(level) = domains.level(world, body) else {
            continue;
        };
        let cell = &world.cells[index];
        let delta = std::array::from_fn(|axis| p[axis] - cell.position[axis]);
        let distance2 = dot(delta, delta);
        // A union of metric neighbourhoods is independent of a face's axes and
        // of the display quad. Only its collar constrains missing retaining land;
        // existing terrain above the plane is never changed.
        let radius =
            distance2.sqrt() * world.config.radius_m / (0.85 * cell.area_m2.sqrt()).max(1.0);
        if radius > 1.3 {
            continue;
        }
        if let Some(slot) = result[..count]
            .iter()
            .position(|s| s.is_some_and(|s| s.id == body.id))
        {
            let old = result[slot].as_mut().unwrap();
            old.radius = old.radius.min(radius);
        } else {
            result[count] = Some(Support {
                id: body.id,
                level,
                radius,
            });
            count += 1;
        }
    }
    (result, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        geometry::{cell_index, cross, normalize},
        WorldConfig,
    };

    fn single_lake() -> (World, usize) {
        let mut world = World::generate(WorldConfig {
            resolution: 8,
            erosion_steps: 0,
            coast_detail_ratio: 0.0,
            ..Default::default()
        })
        .unwrap();
        world.lakes.clear();
        for cell in &mut world.cells {
            cell.lake = false;
            cell.lake_id = None;
        }
        let index = cell_index(4, 3, 3, 8);
        world.cells[index].lake = true;
        world.cells[index].ocean = false;
        world.cells[index].lake_id = Some(17);
        world.cells[index].water_m = 100.0;
        world.invalidate_geometry();
        (world, index)
    }

    #[test]
    fn reservoir_support_is_isotropic_in_physical_space() {
        let (world, index) = single_lake();
        let center = world.cells[index].position;
        let east = normalize(cross([0.0, 1.0, 0.0], center));
        let north = cross(center, east);
        let angle = 0.7 * world.cells[index].area_m2.sqrt() / world.config.radius_m;
        let mut reference: Option<f64> = None;
        for i in 0..64 {
            let phi = i as f64 * std::f64::consts::TAU / 64.0;
            let p = normalize(std::array::from_fn(|axis| {
                center[axis] * angle.cos()
                    + (east[axis] * phi.cos() + north[axis] * phi.sin()) * angle.sin()
            }));
            let (support, count) = supports(&world, &[index], p);
            assert_eq!(count, 1);
            let radius = support[0].unwrap().radius;
            if let Some(old) = reference {
                assert!((radius - old).abs() < 1e-12);
            } else {
                reference = Some(radius);
            }
        }
    }

    #[test]
    fn procedural_clearance_index_matches_full_spherical_domain_distance() {
        let (world, index) = single_lake();
        let center = world.cells[index].position;
        let east = normalize(cross([0.0, 1.0, 0.0], center));
        let north = cross(center, east);
        let body_radius = 1.3 * 0.85 * world.cells[index].area_m2.sqrt();
        for phi in 0..64 {
            let theta = phi as f64 * std::f64::consts::TAU / 64.0;
            for offset in [-1.0, 0.0, 0.001, 1000.0, 2301.0, 19_999.0, 20_001.0] {
                let chord = body_radius + offset;
                let angle = 2.0 * (chord / (2.0 * world.config.radius_m)).asin();
                let p = normalize(std::array::from_fn(|j| {
                    center[j] * angle.cos()
                        + (east[j] * theta.cos() + north[j] * theta.sin()) * angle.sin()
                }));
                let expected = offset.min(GUARD_HALO_M);
                assert!((geographic_clearance(&world, p) - expected).abs() < 1e-7);
            }
        }
    }

    #[test]
    fn immutable_domain_cache_is_shared_by_clones_and_invalidated_by_mutation() {
        let (world, index) = single_lake();
        let _ = supports(&world, &[index], world.cells[index].position);
        let mut changed = world.clone();
        assert!(Arc::ptr_eq(
            world.hydraulic_cache.get().unwrap(),
            changed.hydraulic_cache.get().unwrap()
        ));
        changed.cells[index].lake_id = Some(18);
        changed.invalidate_geometry();
        let (support, count) = supports(&changed, &[index], changed.cells[index].position);
        assert_eq!(count, 1);
        assert_eq!(support[0].unwrap().id, 18);
        let (old, _) = supports(&world, &[index], world.cells[index].position);
        assert_eq!(old[0].unwrap().id, 17);
        assert!(!Arc::ptr_eq(
            world.hydraulic_cache.get().unwrap(),
            changed.hydraulic_cache.get().unwrap()
        ));
    }
}
