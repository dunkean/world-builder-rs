//! Depression analysis is distinct from physical lake storage. Priority-Flood
//! supplies spill topology; finite runoff, evaporation and infiltration determine
//! reservoir levels. This coarse reservoir model is not the full FSM hierarchy.
//! References: doi:10.1016/j.cageo.2013.04.024; doi:10.5194/esurf-9-105-2021.

use crate::{
    geometry::{cell_index, direction, direction_index, dot},
    Cell, WorldConfig,
};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, VecDeque},
};

pub const YEAR_SECONDS: f64 = 31_557_600.0;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Lake {
    pub id: u64,
    pub level_m: f32,
    pub spill_level_m: f32,
    pub area_m2: f64,
    pub volume_m3: f64,
    pub inflow_m3_s: f64,
    pub outflow_m3_s: f64,
    pub evaporation_m3_s: f64,
    pub infiltration_m3_s: f64,
    pub storage_change_m3_s: f64,
    pub outlet: Option<usize>,
    pub control_cell: usize,
    pub cells: Vec<usize>,
}

#[derive(Clone)]
pub(crate) struct Depression {
    pub cells: Vec<usize>,
    pub level: f64,
    pub pit: usize,
    pub exit: usize,
    pub outlet: usize,
}

pub(crate) fn neighbours8(index: usize, n: u32) -> [usize; 8] {
    let size = n as usize * n as usize;
    let face = (index / size) as u8;
    let x = (index % size % n as usize) as i32;
    let y = (index % size / n as usize) as i32;
    std::array::from_fn(|k| {
        let (dx, dy) = [
            (-1, 0),
            (1, 0),
            (0, -1),
            (0, 1),
            (-1, -1),
            (1, -1),
            (-1, 1),
            (1, 1),
        ][k];
        let (nx, ny) = (x + dx, y + dy);
        if nx >= 0 && ny >= 0 && nx < n as i32 && ny < n as i32 {
            cell_index(face, nx as u32, ny as u32, n)
        } else {
            direction_index(
                direction(
                    face,
                    2.0 * (f64::from(nx) + 0.5) / f64::from(n) - 1.0,
                    2.0 * (f64::from(ny) + 0.5) / f64::from(n) - 1.0,
                ),
                n,
            )
        }
    })
}

pub(crate) fn distance(a: &Cell, b: &Cell, radius: f64) -> f64 {
    (2.0 * (1.0 - dot(a.position, b.position)).max(0.0)).sqrt() * radius
}

#[derive(Clone, Copy)]
struct Entry {
    level: f32,
    index: usize,
}
impl PartialEq for Entry {
    fn eq(&self, rhs: &Self) -> bool {
        self.level.to_bits() == rhs.level.to_bits() && self.index == rhs.index
    }
}
impl Eq for Entry {}
impl Ord for Entry {
    fn cmp(&self, rhs: &Self) -> Ordering {
        rhs.level
            .total_cmp(&self.level)
            .then(rhs.index.cmp(&self.index))
    }
}
impl PartialOrd for Entry {
    fn partial_cmp(&self, rhs: &Self) -> Option<Ordering> {
        Some(self.cmp(rhs))
    }
}

pub(crate) fn route_potential(cells: &mut [Cell], n: u32) -> Vec<usize> {
    for c in cells.iter_mut() {
        c.ocean = false;
        c.lake = false;
        c.lake_id = None;
        c.receiver = None;
        c.water_m = c.height_m;
        c.routing_height_m = c.height_m;
    }
    let start = (0..cells.len())
        .min_by(|&a, &b| {
            cells[a]
                .height_m
                .total_cmp(&cells[b].height_m)
                .then(a.cmp(&b))
        })
        .unwrap();
    let mut queue = VecDeque::from([start]);
    cells[start].ocean = true;
    while let Some(i) = queue.pop_front() {
        for j in neighbours8(i, n) {
            if !cells[j].ocean && cells[j].height_m <= 0.0 {
                cells[j].ocean = true;
                queue.push_back(j);
            }
        }
    }
    let mut visited = vec![false; cells.len()];
    let mut heap = BinaryHeap::new();
    let mut order = Vec::with_capacity(cells.len());
    for (i, c) in cells.iter_mut().enumerate() {
        if c.ocean {
            visited[i] = true;
            c.water_m = 0.0;
            c.routing_height_m = 0.0;
            order.push(i);
        }
    }
    for &i in &order {
        for j in neighbours8(i, n) {
            if !visited[j] {
                visited[j] = true;
                cells[j].receiver = Some(i);
                cells[j].routing_height_m = cells[j].height_m.max(0.0);
                heap.push(Entry {
                    level: cells[j].routing_height_m,
                    index: j,
                });
            }
        }
    }
    while let Some(Entry { level, index }) = heap.pop() {
        order.push(index);
        for j in neighbours8(index, n) {
            if !visited[j] {
                visited[j] = true;
                cells[j].receiver = Some(index);
                cells[j].routing_height_m = cells[j].height_m.max(level);
                heap.push(Entry {
                    level: cells[j].routing_height_m,
                    index: j,
                });
            }
        }
    }
    let mut rank = vec![0; cells.len()];
    for (k, &i) in order.iter().enumerate() {
        rank[i] = k;
    }
    // Prefer actual steepest descent where available, with flood rank as the flat tie.
    for &i in &order {
        if cells[i].ocean {
            continue;
        }
        let mut best = cells[i].receiver.unwrap();
        let mut slope = f64::NEG_INFINITY;
        for j in neighbours8(i, n) {
            if rank[j] >= rank[i] {
                continue;
            }
            let candidate = f64::from(cells[i].routing_height_m - cells[j].routing_height_m)
                / distance(&cells[i], &cells[j], 1.0).max(1e-15);
            if candidate > slope {
                slope = candidate;
                best = j;
            }
        }
        cells[i].receiver = Some(best);
    }
    order
}

pub(crate) fn depressions(cells: &[Cell], n: u32, order: &[usize]) -> Vec<Depression> {
    let mut labels = vec![false; cells.len()];
    let mut rank = vec![0; cells.len()];
    for (k, &i) in order.iter().enumerate() {
        rank[i] = k;
    }
    let mut groups = Vec::new();
    for start in 0..cells.len() {
        if labels[start]
            || cells[start].ocean
            || cells[start].routing_height_m - cells[start].height_m < 1.0
        {
            continue;
        }
        let level = cells[start].routing_height_m;
        let mut queue = VecDeque::from([start]);
        labels[start] = true;
        let mut members = Vec::new();
        while let Some(i) = queue.pop_front() {
            members.push(i);
            for j in neighbours8(i, n) {
                if !labels[j]
                    && !cells[j].ocean
                    && cells[j].routing_height_m - cells[j].height_m >= 1.0
                    && (cells[j].routing_height_m - level).abs() < 0.05
                {
                    labels[j] = true;
                    queue.push_back(j);
                }
            }
        }
        let exit = *members.iter().min_by_key(|&&i| rank[i]).unwrap();
        let pit = *members
            .iter()
            .min_by(|&&a, &&b| {
                cells[a]
                    .height_m
                    .total_cmp(&cells[b].height_m)
                    .then(a.cmp(&b))
            })
            .unwrap();
        if let Some(outlet) = cells[exit].receiver {
            groups.push(Depression {
                cells: members,
                level: f64::from(level),
                pit,
                exit,
                outlet,
            });
        }
    }
    groups
}

pub(crate) fn local_runoff(c: &Cell) -> f64 {
    if c.ocean {
        return 0.0;
    }
    let evap = (f64::from(c.temperature_c) / 45.0).clamp(0.0, 0.75);
    c.area_m2 * f64::from(c.moisture).powi(2) * 1.6 * (1.0 - evap) / YEAR_SECONDS
}

pub(crate) fn topological(cells: &[Cell]) -> Vec<usize> {
    let mut indegree = vec![0usize; cells.len()];
    for c in cells {
        if let Some(j) = c.receiver {
            indegree[j] += 1;
        }
    }
    let mut queue: VecDeque<usize> = indegree
        .iter()
        .enumerate()
        .filter(|(_, d)| **d == 0)
        .map(|(i, _)| i)
        .collect();
    let mut order = Vec::with_capacity(cells.len());
    while let Some(i) = queue.pop_front() {
        order.push(i);
        if let Some(j) = cells[i].receiver {
            indegree[j] -= 1;
            if indegree[j] == 0 {
                queue.push_back(j);
            }
        }
    }
    order
}

fn reroute_group(
    cells: &mut [Cell],
    group: &Depression,
    n: u32,
    control: usize,
    outlet: Option<usize>,
    labels: &[usize],
    group_id: usize,
) {
    let mut queue = VecDeque::from([control]);
    let mut visited = std::collections::HashSet::from([control]);
    cells[control].receiver = outlet;
    while let Some(i) = queue.pop_front() {
        for j in neighbours8(i, n) {
            if labels[j] == group_id && visited.insert(j) {
                cells[j].receiver = Some(i);
                queue.push_back(j);
            }
        }
    }
    debug_assert_eq!(visited.len(), group.cells.len());
}

fn reservoir(group: &Depression, cells: &[Cell], input: f64, seconds: f64) -> Lake {
    let min = f64::from(cells[group.pit].height_m);
    let max = group.level;
    let quantities = |level: f64| {
        let mut volume = 0.0;
        let mut evaporation = 0.0;
        let mut infiltration = 0.0;
        let mut area = 0.0;
        for &i in &group.cells {
            let c = &cells[i];
            let depth = (level - f64::from(c.height_m)).max(0.0);
            volume += depth * c.area_m2;
            let wet_fraction = (depth / 2.0).clamp(0.0, 1.0);
            let wet = c.area_m2 * wet_fraction;
            area += wet;
            let evap_depth = (0.35 + 0.055 * f64::from(c.temperature_c.max(0.0))).clamp(0.1, 2.5);
            evaporation += wet * evap_depth / YEAR_SECONDS;
            infiltration += wet * 0.08 / YEAR_SECONDS;
        }
        (volume, evaporation, infiltration, area)
    };
    let available = input * seconds;
    let (capacity, evap_full, seep_full, _) = quantities(max);
    let full = available >= capacity + seconds * (evap_full + seep_full);
    let mut lo = min;
    let mut hi = max;
    if !full {
        for _ in 0..48 {
            let mid = 0.5 * (lo + hi);
            let (v, e, s, _) = quantities(mid);
            if v + seconds * (e + s) > available {
                hi = mid;
            } else {
                lo = mid;
            }
        }
    }
    let level = if full { max } else { 0.5 * (lo + hi) };
    let (volume, evaporation, infiltration, area) = quantities(level);
    let outflow = (input - evaporation - infiltration - volume / seconds).max(0.0);
    Lake {
        id: group.cells.iter().min().unwrap().to_owned() as u64 + 1,
        level_m: level as f32,
        spill_level_m: max as f32,
        area_m2: area,
        volume_m3: volume,
        inflow_m3_s: input,
        outflow_m3_s: outflow,
        evaporation_m3_s: evaporation,
        infiltration_m3_s: infiltration,
        storage_change_m3_s: volume / seconds,
        outlet: if full { Some(group.outlet) } else { None },
        control_cell: group.exit,
        cells: group.cells.clone(),
    }
}

pub(crate) fn physical_water(cells: &mut [Cell], config: &WorldConfig) -> (Vec<Lake>, f64, f64) {
    let order = route_potential(cells, config.resolution);
    let groups = depressions(cells, config.resolution, &order);
    let mut labels = vec![usize::MAX; cells.len()];
    for (g, group) in groups.iter().enumerate() {
        for &i in &group.cells {
            labels[i] = g;
        }
    }
    let mut controls = vec![usize::MAX; cells.len()];
    for (g, group) in groups.iter().enumerate() {
        controls[group.exit] = g;
        reroute_group(
            cells,
            group,
            config.resolution,
            group.exit,
            Some(group.outlet),
            &labels,
            g,
        );
    }
    let mut discharge: Vec<f64> = cells.iter().map(local_runoff).collect();
    let total: f64 = discharge.iter().sum();
    let mut lakes: Vec<Option<Lake>> = vec![None; groups.len()];
    let topo = topological(cells);
    assert_eq!(
        topo.len(),
        cells.len(),
        "depression quotient introduced a cycle"
    );
    for i in topo {
        let group = controls[i];
        if group != usize::MAX {
            let lake = reservoir(
                &groups[group],
                cells,
                discharge[i],
                config.lake_fill_years * YEAR_SECONDS,
            );
            discharge[i] = lake.outflow_m3_s;
            lakes[group] = Some(lake);
        }
        if let Some(j) = cells[i].receiver {
            discharge[j] += discharge[i];
        }
    }
    let mut lakes: Vec<Lake> = lakes.into_iter().flatten().collect();
    for (g, lake) in lakes.iter_mut().enumerate() {
        let group = &groups[g];
        if lake.outlet.is_none() {
            lake.control_cell = group.pit;
            reroute_group(cells, group, config.resolution, group.pit, None, &labels, g);
        }
        for &i in &group.cells {
            if f64::from(cells[i].height_m) < f64::from(lake.level_m) {
                cells[i].lake = true;
                cells[i].lake_id = Some(lake.id);
                cells[i].water_m = lake.level_m;
            }
        }
    }
    // Recompute after closed reservoirs change their internal graph; the same
    // physically accounted losses are applied exactly once at each control.
    let mut losses = vec![0.0; cells.len()];
    for l in &lakes {
        losses[l.control_cell] = l.evaporation_m3_s + l.infiltration_m3_s + l.storage_change_m3_s;
    }
    let mut discharge: Vec<f64> = cells.iter().map(local_runoff).collect();
    let topo = topological(cells);
    assert_eq!(topo.len(), cells.len());
    for i in topo {
        discharge[i] = (discharge[i] - losses[i]).max(0.0);
        if let Some(j) = cells[i].receiver {
            discharge[j] += discharge[i];
        }
    }
    let outlet = cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c.ocean)
        .map(|(i, _)| discharge[i])
        .sum();
    for (c, q) in cells.iter_mut().zip(discharge) {
        c.discharge = q as f32;
    }
    (lakes, total, outlet)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lake_level_comes_from_water_volume_and_losses_not_spill_potential() {
        let world = crate::World::generate(WorldConfig {
            resolution: 8,
            erosion_steps: 0,
            ..Default::default()
        })
        .unwrap();
        let mut cells = vec![world.cells[0].clone(); 3];
        for (c, h) in cells.iter_mut().zip([0.0, 20.0, 40.0]) {
            c.height_m = h;
            c.area_m2 = 1000.0;
            c.temperature_c = 20.0;
        }
        let group = Depression {
            cells: vec![0, 1, 2],
            level: 60.0,
            pit: 0,
            exit: 2,
            outlet: 0,
        };
        let dry = reservoir(&group, &cells, 0.001, YEAR_SECONDS);
        assert!(dry.level_m > 0.0 && dry.level_m < 60.0);
        assert!(dry.outlet.is_none());
        let wet = reservoir(&group, &cells, 100.0, YEAR_SECONDS);
        assert_eq!(wet.level_m, 60.0);
        assert!(wet.outflow_m3_s > 0.0);
        for lake in [dry, wet] {
            let residual = lake.inflow_m3_s
                - lake.outflow_m3_s
                - lake.evaporation_m3_s
                - lake.infiltration_m3_s
                - lake.volume_m3 / YEAR_SECONDS;
            assert!(residual.abs() < 1e-10);
        }
    }
}
