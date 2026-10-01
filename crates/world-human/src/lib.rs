//! Deterministic agrarian settlements and atlas-scale roads. This layer does
//! not alter the physical world or claim to simulate a complete economy.

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::BinaryHeap};
use world_core::{geometry, Material, World};

const VERSION: &str = "human-agrarian-v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HumanConfig {
    pub population: u64,
    pub settlements: usize,
    pub min_distance_km: f64,
}

impl Default for HumanConfig {
    fn default() -> Self {
        Self {
            population: 120_000,
            settlements: 24,
            min_distance_km: 80.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SettlementKind {
    Village,
    Town,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settlement {
    pub id: String,
    pub cell: usize,
    pub direction: [f64; 3],
    pub altitude_m: f32,
    pub population: u64,
    pub suitability: f64,
    pub kind: SettlementKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Road {
    pub id: String,
    pub from: String,
    pub to: String,
    pub points: Vec<[f64; 3]>,
    pub altitudes_m: Vec<f32>,
    /// Atlas cells retained for validation; no hidden straight line over water.
    pub cell_path: Vec<usize>,
    pub length_m: f64,
    pub max_grade: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HumanLayer {
    pub id: String,
    pub settlements: Vec<Settlement>,
    pub roads: Vec<Road>,
    pub population: u64,
    pub unplaced_population: u64,
    /// Number of land components containing settlements, without ferry links.
    pub island_components: usize,
    pub unreachable_roads: usize,
}

impl HumanLayer {
    pub fn generate(world: &World, config: HumanConfig) -> Result<Self> {
        ensure!(
            (1..=128).contains(&config.settlements),
            "settlements must be 1..=128"
        );
        ensure!(
            config.population <= 1_000_000_000,
            "prototype population must be <=1 billion"
        );
        ensure!(
            config.min_distance_km.is_finite() && config.min_distance_km >= 0.0,
            "minimum settlement distance must be finite and nonnegative"
        );
        let parameters = serde_json::to_vec(&config)?;
        let id = stable_id(&[VERSION.as_bytes(), world.id.as_bytes(), &parameters]);
        let mut layer = Self {
            id,
            settlements: Vec::new(),
            roads: Vec::new(),
            population: config.population,
            unplaced_population: config.population,
            island_components: 0,
            unreachable_roads: 0,
        };
        if config.population == 0 {
            return Ok(layer);
        }

        let (components, _) = land_components(world);
        let mut candidates: Vec<_> = world
            .cells
            .iter()
            .enumerate()
            .filter_map(|(i, _)| suitability(world, i).map(|score| (i, score)))
            .collect();
        candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let limit = config.settlements.min(config.population as usize);
        for (cell_index, score) in candidates {
            let cell = &world.cells[cell_index];
            if layer.settlements.iter().any(|s| {
                distance(cell.position, s.direction, world.config.radius_m)
                    < config.min_distance_km * 1000.0
            }) {
                continue;
            }
            layer.settlements.push(Settlement {
                id: stable_id(&[layer.id.as_bytes(), &(cell_index as u64).to_le_bytes()]),
                cell: cell_index,
                direction: cell.position,
                altitude_m: cell.height_m,
                population: 0,
                suitability: score,
                kind: SettlementKind::Village,
            });
            if layer.settlements.len() == limit {
                break;
            }
        }
        if layer.settlements.is_empty() {
            return Ok(layer);
        }
        allocate_population(&mut layer.settlements, config.population);
        layer.unplaced_population = 0;
        let mut inhabited: Vec<_> = layer
            .settlements
            .iter()
            .map(|s| components[s.cell])
            .collect();
        inhabited.sort_unstable();
        inhabited.dedup();
        layer.island_components = inhabited.len();

        // Build a geodesic MST separately on each connected land component.
        // A failed grade-constrained path is reported, not replaced by a line.
        let mut edges = Vec::new();
        for i in 0..layer.settlements.len() {
            for j in i + 1..layer.settlements.len() {
                let a = &layer.settlements[i];
                let b = &layer.settlements[j];
                if components[a.cell] == components[b.cell] {
                    edges.push((
                        distance(a.direction, b.direction, world.config.radius_m),
                        i,
                        j,
                    ));
                }
            }
        }
        edges.sort_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        let mut forest: Vec<_> = (0..layer.settlements.len()).collect();
        for (_, a, b) in edges {
            let ca = forest_root(&mut forest, a);
            let cb = forest_root(&mut forest, b);
            if ca == cb {
                continue;
            }
            forest[cb] = ca;
            let from = &layer.settlements[a];
            let to = &layer.settlements[b];
            match route(world, from.cell, to.cell) {
                Some(cell_path) => {
                    let mut length_m = 0.0;
                    let mut max_grade = 0.0_f64;
                    for pair in cell_path.windows(2) {
                        let x = &world.cells[pair[0]];
                        let y = &world.cells[pair[1]];
                        let length = distance(x.position, y.position, world.config.radius_m);
                        length_m += length;
                        max_grade =
                            max_grade.max(f64::from((x.height_m - y.height_m).abs()) / length);
                    }
                    layer.roads.push(Road {
                        id: stable_id(&[layer.id.as_bytes(), from.id.as_bytes(), to.id.as_bytes()]),
                        from: from.id.clone(),
                        to: to.id.clone(),
                        points: cell_path.iter().map(|&i| world.cells[i].position).collect(),
                        altitudes_m: cell_path.iter().map(|&i| world.cells[i].height_m).collect(),
                        cell_path,
                        length_m,
                        max_grade,
                    });
                }
                None => layer.unreachable_roads += 1,
            }
        }
        Ok(layer)
    }
}

fn stable_id(parts: &[&[u8]]) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher.finalize().to_hex()[..16].to_owned()
}

fn distance(a: [f64; 3], b: [f64; 3], radius: f64) -> f64 {
    // Chord/asin is stable for close atlas neighbors and antipodal sites.
    let chord = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    2.0 * radius * (0.5 * chord).clamp(0.0, 1.0).asin()
}

fn dry(world: &World, index: usize) -> bool {
    !world.cells[index].ocean && !world.cells[index].lake
}

fn suitability(world: &World, index: usize) -> Option<f64> {
    if !dry(world, index) {
        return None;
    }
    let cell = &world.cells[index];
    let temperature = f64::from(cell.temperature_c);
    if !(-5.0..=42.0).contains(&temperature) {
        return None;
    }
    let neighbors = geometry::neighbours(index, world.config.resolution);
    let river = cell.discharge >= 20.0
        || neighbors
            .iter()
            .any(|&i| world.cells[i].discharge >= 120.0 && !world.cells[i].ocean);
    let lake = neighbors.iter().any(|&i| world.cells[i].lake);
    // Coast access is a groundwater/rainfall proxy, not fresh seawater.
    let coast = neighbors.iter().any(|&i| world.cells[i].ocean) && cell.moisture >= 0.18;
    if !(river || lake || coast) {
        return None;
    }
    let moisture =
        (f64::from(cell.moisture) + if river || lake { 0.25 } else { 0.0 }).clamp(0.0, 1.0);
    if moisture < 0.15 {
        return None;
    }
    let thermal = ((temperature + 5.0) / 22.0).clamp(0.0, 1.0)
        * ((45.0 - temperature) / 18.0).clamp(0.0, 1.0);
    let soil = match cell.material {
        Material::Alluvium => 1.25,
        Material::Limestone => 1.0,
        Material::Sandstone => 0.9,
        Material::Basalt => 0.95,
        Material::Granite => 0.72,
        Material::Ice => return None,
    };
    let slope = neighbors
        .iter()
        .filter(|&&i| dry(world, i))
        .map(|&i| {
            f64::from((cell.height_m - world.cells[i].height_m).abs())
                / distance(
                    cell.position,
                    world.cells[i].position,
                    world.config.radius_m,
                )
        })
        .fold(0.0_f64, f64::max);
    if slope > 0.25 {
        return None;
    }
    let water = if river {
        1.15
    } else if lake {
        1.05
    } else {
        0.8
    };
    let score = thermal * moisture * soil * water / (1.0 + 12.0 * slope);
    (score > 0.02).then_some(score)
}

fn allocate_population(sites: &mut [Settlement], population: u64) {
    let weights: Vec<u64> = sites
        .iter()
        .map(|s| (s.suitability * 1_000_000.0).round().max(1.0) as u64)
        .collect();
    let sum: u128 = weights.iter().map(|&w| u128::from(w)).sum();
    let remaining = population - sites.len() as u64;
    let mut remainders = Vec::with_capacity(sites.len());
    let mut assigned = 0;
    for (index, (site, weight)) in sites.iter_mut().zip(&weights).enumerate() {
        let product = u128::from(remaining) * u128::from(*weight);
        site.population = 1 + (product / sum) as u64;
        assigned += site.population;
        remainders.push((product % sum, index));
    }
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for &(_, index) in remainders.iter().take((population - assigned) as usize) {
        sites[index].population += 1;
    }
    for site in sites {
        site.kind = if site.population >= 5_000 {
            SettlementKind::Town
        } else {
            SettlementKind::Village
        };
    }
}

fn land_components(world: &World) -> (Vec<usize>, usize) {
    let mut labels = vec![usize::MAX; world.cells.len()];
    let mut component = 0;
    let mut stack = Vec::new();
    for start in 0..world.cells.len() {
        if labels[start] != usize::MAX || !dry(world, start) {
            continue;
        }
        labels[start] = component;
        stack.push(start);
        while let Some(index) = stack.pop() {
            for next in geometry::neighbours(index, world.config.resolution) {
                if labels[next] == usize::MAX && dry(world, next) {
                    labels[next] = component;
                    stack.push(next);
                }
            }
        }
        component += 1;
    }
    (labels, component)
}

fn forest_root(forest: &mut [usize], mut index: usize) -> usize {
    while forest[index] != index {
        forest[index] = forest[forest[index]];
        index = forest[index];
    }
    index
}

fn offset(index: usize, dx: i32, dy: i32, n: u32) -> usize {
    let face_area = n as usize * n as usize;
    let face = (index / face_area) as u8;
    let local = index % face_area;
    let x = (local % n as usize) as i32 + dx;
    let y = (local / n as usize) as i32 + dy;
    if x >= 0 && y >= 0 && x < n as i32 && y < n as i32 {
        geometry::cell_index(face, x as u32, y as u32, n)
    } else {
        geometry::direction_index(
            geometry::direction(
                face,
                2.0 * (f64::from(x) + 0.5) / f64::from(n) - 1.0,
                2.0 * (f64::from(y) + 0.5) / f64::from(n) - 1.0,
            ),
            n,
        )
    }
}

#[derive(Clone, Copy)]
struct Queue {
    estimated: f64,
    cost: f64,
    cell: usize,
}
impl PartialEq for Queue {
    fn eq(&self, b: &Self) -> bool {
        self.estimated == b.estimated && self.cell == b.cell
    }
}
impl Eq for Queue {}
impl PartialOrd for Queue {
    fn partial_cmp(&self, b: &Self) -> Option<Ordering> {
        Some(self.cmp(b))
    }
}
impl Ord for Queue {
    fn cmp(&self, b: &Self) -> Ordering {
        b.estimated
            .total_cmp(&self.estimated)
            .then_with(|| b.cell.cmp(&self.cell))
    }
}

fn route(world: &World, start: usize, goal: usize) -> Option<Vec<usize>> {
    let mut costs = vec![f64::INFINITY; world.cells.len()];
    let mut parents = vec![usize::MAX; world.cells.len()];
    let mut queue = BinaryHeap::new();
    let radius = world.config.radius_m;
    let target = world.cells[goal].position;
    costs[start] = 0.0;
    queue.push(Queue {
        estimated: distance(world.cells[start].position, target, radius),
        cost: 0.0,
        cell: start,
    });
    while let Some(current) = queue.pop() {
        if current.cost > costs[current.cell] {
            continue;
        }
        if current.cell == goal {
            let mut path = vec![goal];
            let mut cell = goal;
            while cell != start {
                cell = parents[cell];
                path.push(cell);
            }
            path.reverse();
            return Some(path);
        }
        for (dx, dy) in [
            (-1, 0),
            (1, 0),
            (0, -1),
            (0, 1),
            (-1, -1),
            (-1, 1),
            (1, -1),
            (1, 1),
        ] {
            let next = offset(current.cell, dx, dy, world.config.resolution);
            if next == current.cell || !dry(world, next) {
                continue;
            }
            if dx != 0
                && dy != 0
                && (!dry(world, offset(current.cell, dx, 0, world.config.resolution))
                    || !dry(world, offset(current.cell, 0, dy, world.config.resolution)))
            {
                continue;
            }
            let a = &world.cells[current.cell];
            let b = &world.cells[next];
            let length = distance(a.position, b.position, radius);
            let grade = f64::from((a.height_m - b.height_m).abs()) / length;
            if grade > 0.25 {
                continue;
            }
            // River crossings incur bridge effort; lakes and seas are forbidden.
            let bridge = if a.discharge.max(b.discharge) >= 120.0 {
                0.8
            } else {
                0.0
            };
            let cost = current.cost + length * (1.0 + 6.0 * grade + 80.0 * grade.powi(2) + bridge);
            if cost < costs[next] {
                costs[next] = cost;
                parents[next] = current.cell;
                queue.push(Queue {
                    estimated: cost + distance(b.position, target, radius),
                    cost,
                    cell: next,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use world_core::WorldConfig;

    fn world() -> World {
        World::generate(WorldConfig {
            resolution: 16,
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn population_budget_and_identity_are_deterministic() {
        let world = world();
        let config = HumanConfig {
            population: 12_345,
            settlements: 12,
            min_distance_km: 30.0,
        };
        let a = HumanLayer::generate(&world, config.clone()).unwrap();
        let b = HumanLayer::generate(&world, config).unwrap();
        assert!(!a.settlements.is_empty());
        assert_eq!(
            a.settlements.iter().map(|s| s.population).sum::<u64>() + a.unplaced_population,
            a.population
        );
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn roads_follow_dry_atlas_cells_and_have_valid_endpoints() {
        let world = world();
        let human = HumanLayer::generate(&world, HumanConfig::default()).unwrap();
        assert!(!human.roads.is_empty());
        for road in &human.roads {
            assert!(road.cell_path.iter().all(|&i| dry(&world, i)));
            let from = human
                .settlements
                .iter()
                .find(|s| s.id == road.from)
                .unwrap();
            let to = human.settlements.iter().find(|s| s.id == road.to).unwrap();
            assert_eq!(road.cell_path.first(), Some(&from.cell));
            assert_eq!(road.cell_path.last(), Some(&to.cell));
            assert_eq!(road.points.len(), road.cell_path.len());
            assert!(road.length_m > 0.0 && road.max_grade <= 0.25 + 1e-9);
        }
    }

    #[test]
    fn settlement_spacing_and_small_population_are_respected() {
        let world = world();
        let human = HumanLayer::generate(
            &world,
            HumanConfig {
                population: 3,
                settlements: 20,
                min_distance_km: 100.0,
            },
        )
        .unwrap();
        assert!(human.settlements.len() <= 3);
        assert_eq!(
            human.settlements.iter().map(|s| s.population).sum::<u64>() + human.unplaced_population,
            3
        );
        for (i, a) in human.settlements.iter().enumerate() {
            assert!(dry(&world, a.cell));
            for b in &human.settlements[i + 1..] {
                assert!(distance(a.direction, b.direction, world.config.radius_m) >= 100_000.0);
            }
        }
    }

    #[test]
    fn no_arable_land_reports_unplaced_population() {
        let mut world = world();
        for cell in &mut world.cells {
            cell.ocean = true;
        }
        let layer = HumanLayer::generate(&world, HumanConfig::default()).unwrap();
        assert!(layer.settlements.is_empty() && layer.roads.is_empty());
        assert_eq!(layer.unplaced_population, layer.population);
    }
}
