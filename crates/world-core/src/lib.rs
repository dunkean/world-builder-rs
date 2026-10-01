//! Deterministic prototype planet compiler: global spherical drainage, persistent
//! atlas and continuous spatial detail. This is a reduced model, not a climate GCM.

pub mod geometry;

use anyhow::{bail, Context, Result};
use geometry::{cell_area, cell_index, cross, direction, dot, neighbours, normalize, FACE_BASES};
pub use geometry::{CubeFace, TileKey};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, VecDeque},
};

pub const GENERATOR_VERSION: &str = "prototype-0.4";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WorldConfig {
    pub seed: u64,
    pub radius_m: f64,
    pub resolution: u32,
    pub ocean_fraction: f64,
    pub relief_m: f64,
    pub detail_m: f64,
    pub axial_tilt_deg: f64,
    pub gravity_m_s2: f64,
    /// Alternative to ocean_fraction: total ocean water volume in cubic metres.
    pub water_volume_m3: Option<f64>,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            seed: 42,
            radius_m: 1_737_000.0,
            resolution: 96,
            ocean_fraction: 0.58,
            relief_m: 6_000.0,
            detail_m: 140.0,
            axial_tilt_deg: 23.4,
            gravity_m_s2: 3.0,
            water_volume_m3: None,
        }
    }
}

impl WorldConfig {
    pub fn validate(&self) -> Result<()> {
        if !(8..=1024).contains(&self.resolution) {
            bail!("resolution must be between 8 and 1024 cells per cube face");
        }
        if !self.radius_m.is_finite() || !(10_000.0..=100_000_000.0).contains(&self.radius_m) {
            bail!("radius_m must be finite and between 10 km and 100,000 km");
        }
        if !self.ocean_fraction.is_finite() || !(0.05..=0.95).contains(&self.ocean_fraction) {
            bail!("ocean_fraction must be between 0.05 and 0.95");
        }
        if !self.relief_m.is_finite() || !(10.0..=self.radius_m * 0.08).contains(&self.relief_m) {
            bail!("relief_m must be positive and smaller than 8% of radius");
        }
        if !self.detail_m.is_finite() || !(0.0..=self.relief_m).contains(&self.detail_m) {
            bail!("detail_m must be finite and between zero and relief_m");
        }
        if !self.axial_tilt_deg.is_finite() || !(0.0..=90.0).contains(&self.axial_tilt_deg) {
            bail!("axial_tilt_deg must be between zero and 90 degrees");
        }
        if !self.gravity_m_s2.is_finite() || !(0.05..=100.0).contains(&self.gravity_m_s2) {
            bail!("gravity_m_s2 must be finite and between 0.05 and 100");
        }
        if let Some(volume) = self.water_volume_m3 {
            if !volume.is_finite() || volume <= 0.0 {
                bail!("water_volume_m3 must be finite and positive");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u8)]
pub enum Biome {
    Ocean,
    Lake,
    Ice,
    Tundra,
    Desert,
    Grassland,
    Forest,
    Rainforest,
    Wetland,
    Rock,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Material {
    Basalt,
    Granite,
    Sandstone,
    Limestone,
    Alluvium,
    Ice,
}

impl Material {
    pub fn erodability(self) -> f32 {
        match self {
            Self::Basalt => 0.35,
            Self::Granite => 0.45,
            Self::Sandstone => 1.3,
            Self::Limestone => 0.9,
            Self::Alluvium => 1.8,
            Self::Ice => 0.2,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cell {
    /// Unit vector. World positions are (radius_m + height_m) * position.
    pub position: [f64; 3],
    pub height_m: f32,
    /// Absolute surface altitude; equals height_m on dry cells.
    pub water_m: f32,
    pub temperature_c: f32,
    pub moisture: f32,
    /// Mean runoff accumulated through the actual global drainage graph, m³/s.
    pub discharge: f32,
    pub biome: Biome,
    pub material: Material,
    pub erodability: f32,
    pub receiver: Option<usize>,
    pub area_m2: f64,
    pub ocean: bool,
    pub lake: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct WorldStats {
    pub cells: u64,
    pub land_fraction: f64,
    pub ocean_fraction: f64,
    pub lake_cells: u64,
    pub river_cells: u64,
    pub min_height_m: f32,
    pub max_height_m: f32,
    pub max_discharge_m3_s: f32,
    pub surface_area_m2: f64,
    pub total_runoff_m3_s: f64,
    pub outlet_discharge_m3_s: f64,
    pub ocean_volume_m3: f64,
    pub water_balance_relative_error: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct World {
    pub config: WorldConfig,
    pub id: String,
    pub cells: Vec<Cell>,
    /// Absolute datum selected from raw tectonic relief, before zeroing heights.
    pub sea_level_m: f32,
    pub stats: WorldStats,
    /// Presentation phase, not a geological revision. None means annual atlas.
    #[serde(default)]
    pub seasonal_phase: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub direction: [f64; 3],
    pub height_m: f32,
    pub water_m: f32,
    pub temperature_c: f32,
    pub moisture: f32,
    pub discharge: f32,
    pub biome: Biome,
    /// Continuous biome mixture, in enum order, for seamless terrain shading.
    #[serde(default)]
    pub biome_mix: [f32; 10],
    pub material: Material,
    pub erodability: f32,
    pub ocean: bool,
    pub lake: bool,
    pub river: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tile {
    pub key: TileKey,
    pub segments: u32,
    pub vertices: Vec<Sample>,
    pub indices: Vec<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiverSegment {
    pub id: String,
    pub from: usize,
    pub to: usize,
    pub start: [f64; 3],
    pub end: [f64; 3],
    pub start_height_m: f32,
    pub end_height_m: f32,
    pub discharge_m3_s: f32,
    pub width_m: f32,
}

#[derive(Clone, Copy)]
struct Plate {
    site: [f64; 3],
    omega: [f64; 3],
    continental: f64,
}

fn hash64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

fn random(seed: u64) -> f64 {
    (hash64(seed) >> 11) as f64 / ((1_u64 << 53) as f64)
}

fn noise(seed: u64, p: [f64; 3], frequency: f64) -> f64 {
    let q = [p[0] * frequency, p[1] * frequency, p[2] * frequency];
    let base = [
        q[0].floor() as i64,
        q[1].floor() as i64,
        q[2].floor() as i64,
    ];
    let mut t = [
        q[0] - base[0] as f64,
        q[1] - base[1] as f64,
        q[2] - base[2] as f64,
    ];
    for value in &mut t {
        *value = *value * *value * (3.0 - 2.0 * *value);
    }
    let mut value = 0.0;
    for z in 0..2 {
        for y in 0..2 {
            for x in 0..2 {
                let key = seed
                    ^ ((base[0] + x) as u64).wrapping_mul(0x8da6b343)
                    ^ ((base[1] + y) as u64).wrapping_mul(0xd8163841)
                    ^ ((base[2] + z) as u64).wrapping_mul(0xcb1ab31f);
                let w = (if x == 0 { 1.0 - t[0] } else { t[0] })
                    * (if y == 0 { 1.0 - t[1] } else { t[1] })
                    * (if z == 0 { 1.0 - t[2] } else { t[2] });
                value += w * (2.0 * random(key) - 1.0);
            }
        }
    }
    value
}

fn fbm(seed: u64, p: [f64; 3], frequency: f64, octaves: u32) -> f64 {
    let mut value = 0.0;
    let mut amplitude = 1.0;
    let mut frequency = frequency;
    for octave in 0..octaves {
        value += amplitude * noise(seed.wrapping_add(u64::from(octave) * 97), p, frequency);
        amplitude *= 0.5;
        frequency *= 2.0;
    }
    value
}

fn plates(seed: u64) -> Vec<Plate> {
    (0..14)
        .map(|i| {
            // Fibonacci sites with seeded rotation avoids tiny random Voronoi plates.
            let z = 1.0 - 2.0 * (f64::from(i) + 0.5) / 14.0;
            let angle = f64::from(i) * 2.399963229728653 + random(seed) * 6.283185307179586;
            let r = (1.0 - z * z).sqrt();
            Plate {
                site: [r * angle.cos(), z, r * angle.sin()],
                omega: [
                    random(seed.wrapping_add(i as u64 * 13 + 1)) - 0.5,
                    random(seed.wrapping_add(i as u64 * 13 + 2)) - 0.5,
                    random(seed.wrapping_add(i as u64 * 13 + 3)) - 0.5,
                ],
                continental: random(seed.wrapping_add(i as u64 * 13 + 4)),
            }
        })
        .collect()
}

fn raw_height(config: &WorldConfig, plates: &[Plate], p: [f64; 3]) -> f64 {
    let mut nearest = [(f64::NEG_INFINITY, 0usize); 2];
    let mut continental = 0.0;
    let mut weights = 0.0;
    for (i, plate) in plates.iter().enumerate() {
        let score = dot(p, plate.site);
        let weight = (12.0 * (score - 1.0)).exp();
        continental += weight * plate.continental;
        weights += weight;
        if score > nearest[0].0 {
            nearest[1] = nearest[0];
            nearest[0] = (score, i);
        } else if score > nearest[1].0 {
            nearest[1] = (score, i);
        }
    }
    let a = plates[nearest[0].1];
    let b = plates[nearest[1].1];
    let va = cross(a.omega, p);
    let vb = cross(b.omega, p);
    let boundary_direction = normalize([
        b.site[0] - a.site[0],
        b.site[1] - a.site[1],
        b.site[2] - a.site[2],
    ]);
    let convergence = dot(
        [va[0] - vb[0], va[1] - vb[1], va[2] - vb[2]],
        boundary_direction,
    );
    let boundary = (-((nearest[0].0 - nearest[1].0) / 0.065).powi(2)).exp();
    let continent =
        1.4 * (continental / weights - 0.5) + 0.8 * fbm(config.seed ^ 0xabc1, p, 2.8, 4);
    let texture = fbm(config.seed ^ 0x9832, p, 18.0, 4);
    let mountain =
        convergence.max(0.0) * boundary * (0.35 + 0.65 * (texture + 0.5).clamp(0.0, 1.0));
    let trench = convergence.min(0.0) * boundary * 0.25;
    config.relief_m * (continent + 0.65 * mountain + trench + 0.10 * texture)
}

fn choose_sea_level(cells: &[Cell], config: &WorldConfig) -> f32 {
    // Minimax spill levels ensure the ocean is the connected component containing
    // the global minimum. Isolated depressions do not consume marine volume.
    let start = (0..cells.len())
        .min_by(|&a, &b| cells[a].height_m.total_cmp(&cells[b].height_m))
        .unwrap();
    let mut spill = vec![f32::INFINITY; cells.len()];
    let mut visited = vec![false; cells.len()];
    let mut heap = BinaryHeap::new();
    spill[start] = cells[start].height_m;
    visited[start] = true;
    heap.push(FloodEntry {
        level: spill[start],
        index: start,
    });
    while let Some(FloodEntry { level, index }) = heap.pop() {
        for next in neighbours(index, config.resolution) {
            if visited[next] {
                continue;
            }
            visited[next] = true;
            spill[next] = cells[next].height_m.max(level);
            heap.push(FloodEntry {
                level: spill[next],
                index: next,
            });
        }
    }
    if let Some(volume) = config.water_volume_m3 {
        let mut lo = cells
            .iter()
            .map(|c| f64::from(c.height_m))
            .fold(f64::INFINITY, f64::min);
        let mut hi = cells
            .iter()
            .map(|c| f64::from(c.height_m))
            .fold(f64::NEG_INFINITY, f64::max)
            + volume / cells.iter().map(|c| c.area_m2).sum::<f64>();
        for _ in 0..48 {
            let mid = 0.5 * (lo + hi);
            let water: f64 = cells
                .iter()
                .zip(&spill)
                .filter(|(_, s)| f64::from(**s) <= mid)
                .map(|(c, _)| (mid - f64::from(c.height_m)).max(0.0) * c.area_m2)
                .sum();
            if water < volume {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        return (0.5 * (lo + hi)) as f32;
    }
    let total: f64 = cells.iter().map(|c| c.area_m2).sum();
    let mut sorted: Vec<usize> = (0..cells.len()).collect();
    sorted.sort_unstable_by(|&a, &b| spill[a].total_cmp(&spill[b]).then(a.cmp(&b)));
    let mut area = 0.0;
    for i in sorted {
        area += cells[i].area_m2;
        if area >= config.ocean_fraction * total {
            return spill[i];
        }
    }
    0.0
}

#[derive(Clone, Copy)]
struct FloodEntry {
    level: f32,
    index: usize,
}
impl PartialEq for FloodEntry {
    fn eq(&self, other: &Self) -> bool {
        self.level.to_bits() == other.level.to_bits() && self.index == other.index
    }
}
impl Eq for FloodEntry {}
impl Ord for FloodEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .level
            .total_cmp(&self.level)
            .then(other.index.cmp(&self.index))
    }
}
impl PartialOrd for FloodEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Identify the connected marine component, then fill-and-spill land depressions.
/// Flood parents are a topological witness: every land receiver was visited first.
fn drainage(cells: &mut [Cell], n: u32) -> Vec<usize> {
    for cell in cells.iter_mut() {
        cell.ocean = false;
        cell.lake = false;
        cell.receiver = None;
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
        for j in neighbours(i, n) {
            if !cells[j].ocean && cells[j].height_m <= 0.0 {
                cells[j].ocean = true;
                queue.push_back(j);
            }
        }
    }
    let mut heap = BinaryHeap::new();
    let mut visited = vec![false; cells.len()];
    let mut order = Vec::with_capacity(cells.len());
    for (i, cell) in cells.iter_mut().enumerate() {
        if cell.ocean {
            cell.water_m = 0.0;
            visited[i] = true;
            heap.push(FloodEntry {
                level: 0.0,
                index: i,
            });
        }
    }
    while let Some(FloodEntry { level, index }) = heap.pop() {
        order.push(index);
        for next in neighbours(index, n) {
            if visited[next] {
                continue;
            }
            visited[next] = true;
            let cell = &mut cells[next];
            cell.receiver = Some(index);
            cell.water_m = cell.height_m.max(level);
            cell.lake = cell.water_m - cell.height_m > 0.5;
            heap.push(FloodEntry {
                level: cell.water_m,
                index: next,
            });
        }
    }
    order
}

fn daily_insolation(latitude: f64, declination: f64) -> f64 {
    let h = (-latitude.tan() * declination.tan())
        .clamp(-1.0, 1.0)
        .acos();
    (h * latitude.sin() * declination.sin() + latitude.cos() * declination.cos() * h.sin()).max(0.0)
        / std::f64::consts::PI
}

fn annual_insolation(latitude: f64, tilt: f64) -> f64 {
    (0..12)
        .map(|i| {
            let longitude = std::f64::consts::TAU * (f64::from(i) + 0.5) / 12.0;
            let declination = (tilt.sin() * longitude.sin()).asin();
            daily_insolation(latitude, declination)
        })
        .sum::<f64>()
        / 12.0
}

fn climate(cells: &mut [Cell], config: &WorldConfig) {
    let tilt = config.axial_tilt_deg.to_radians();
    let equator_flux = annual_insolation(0.0, tilt);
    for cell in cells {
        let latitude = cell.position[1].asin().abs();
        let latitude_deg = latitude.to_degrees();
        let temperature = 30.0 + 300.0 * (annual_insolation(latitude, tilt) - equator_flux)
            - 0.0065 * f64::from(cell.height_m.max(0.0));
        // Latitude circulation bands plus spatial variability. No claim of a GCM.
        let equatorial = (-((latitude_deg - 6.0) / 18.0).powi(2)).exp();
        let temperate = (-((latitude_deg - 52.0) / 17.0).powi(2)).exp();
        let dryness = (-((latitude_deg - 28.0) / 10.0).powi(2)).exp();
        let precipitation = 0.35 + 0.60 * equatorial + 0.35 * temperate - 0.34 * dryness
            + 0.22 * fbm(config.seed ^ 0x8142, cell.position, 5.0, 3);
        cell.temperature_c = temperature as f32;
        cell.moisture = precipitation.clamp(0.01, 1.0) as f32;
    }
}

fn accumulate(cells: &mut [Cell], order: &[usize]) -> (f64, f64) {
    let mut discharge = vec![0.0f64; cells.len()];
    let mut total = 0.0;
    for (i, cell) in cells.iter().enumerate() {
        if !cell.ocean {
            // Annual runoff depth: 1.6 m maximum, reduced by evapotranspiration.
            let evap = (f64::from(cell.temperature_c) / 45.0).clamp(0.0, 0.75);
            let runoff_m_s = f64::from(cell.moisture).powi(2) * 1.6 * (1.0 - evap) / 31_557_600.0;
            discharge[i] = cell.area_m2 * runoff_m_s;
            total += discharge[i];
        }
    }
    for &i in order.iter().rev() {
        if let Some(receiver) = cells[i].receiver {
            discharge[receiver] += discharge[i];
        }
    }
    let outlet = cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c.ocean)
        .map(|(i, _)| discharge[i])
        .sum();
    for (cell, q) in cells.iter_mut().zip(discharge) {
        cell.discharge = q as f32;
    }
    (total, outlet)
}

fn classify(cell: &Cell) -> Biome {
    if cell.ocean {
        return Biome::Ocean;
    }
    if cell.lake {
        return Biome::Lake;
    }
    if cell.temperature_c < -8.0 {
        return Biome::Ice;
    }
    if cell.temperature_c < 3.0 {
        return Biome::Tundra;
    }
    if cell.moisture < 0.22 {
        return Biome::Desert;
    }
    if cell.discharge > 3500.0 && cell.height_m < 350.0 {
        return Biome::Wetland;
    }
    if cell.temperature_c > 19.0 && cell.moisture > 0.70 {
        return Biome::Rainforest;
    }
    if cell.moisture > 0.46 {
        return Biome::Forest;
    }
    if cell.height_m > 4200.0 {
        return Biome::Rock;
    }
    Biome::Grassland
}

impl World {
    fn channel_width(&self, discharge: f32) -> f64 {
        // Geometrically similar Darcy-Weisbach channels: velocity scales as sqrt(g),
        // area as 1/sqrt(g), and width as g^(-1/4), at fixed discharge.
        (f64::from(discharge).sqrt() * 1.8 * (9.81 / self.config.gravity_m_s2).powf(0.25))
            .clamp(8.0, 2500.0)
    }

    pub fn rivers(&self) -> Vec<RiverSegment> {
        self.cells
            .iter()
            .enumerate()
            .filter_map(|(i, cell)| {
                if cell.ocean || cell.lake || cell.discharge < 120.0 {
                    return None;
                }
                let receiver = cell.receiver?;
                let next = &self.cells[receiver];
                Some(RiverSegment {
                    id: format!("{}-river-{i}", self.id),
                    from: i,
                    to: receiver,
                    start: cell.position,
                    end: next.position,
                    start_height_m: cell.height_m,
                    end_height_m: next.height_m,
                    discharge_m3_s: cell.discharge,
                    width_m: self.channel_width(cell.discharge) as f32,
                })
            })
            .collect()
    }

    /// Reduced seasonal forcing with a fixed one-twelfth-year thermal lag.
    /// Discharge remains mean annual: there is no seasonal reservoir solver yet.
    pub fn seasonal_sample(&self, p: [f64; 3], phase: f64) -> Sample {
        if self.seasonal_phase.is_some() {
            let mut annual = self.clone();
            climate(&mut annual.cells, &annual.config);
            annual.seasonal_phase = None;
            return annual.seasonal_sample(p, phase);
        }
        let mut sample = self.sample(p);
        let phase = if phase.is_finite() {
            phase.rem_euclid(1.0)
        } else {
            0.0
        };
        let tilt = self.config.axial_tilt_deg.to_radians();
        let latitude = sample.direction[1].asin();
        let declination =
            (tilt.sin() * (std::f64::consts::TAU * (phase - 1.0 / 12.0)).sin()).asin();
        let forcing = daily_insolation(latitude, declination) - annual_insolation(latitude, tilt);
        let inertia = if sample.ocean || sample.lake {
            0.35
        } else {
            1.0
        };
        sample.temperature_c += (90.0 * forcing * inertia) as f32;
        sample.moisture = (sample.moisture + (0.25 * forcing) as f32).clamp(0.0, 1.0);
        sample
    }

    /// Cheap seasonal presentation of the atlas. Geometry, discharge, water
    /// surfaces, graph, geological identity and annual statistics stay fixed.
    pub fn seasonal_world(&self, phase: f64) -> Result<Self> {
        if !phase.is_finite() {
            bail!("seasonal phase must be finite");
        }
        let phase = phase.rem_euclid(1.0);
        let mut view = self.clone();
        if self.seasonal_phase.is_some() {
            climate(&mut view.cells, &view.config);
        }
        let tilt = view.config.axial_tilt_deg.to_radians();
        let declination =
            (tilt.sin() * (std::f64::consts::TAU * (phase - 1.0 / 12.0)).sin()).asin();
        for cell in &mut view.cells {
            let latitude = cell.position[1].asin();
            let forcing =
                daily_insolation(latitude, declination) - annual_insolation(latitude, tilt);
            let inertia = if cell.ocean || cell.lake { 0.35 } else { 1.0 };
            cell.temperature_c += (90.0 * forcing * inertia) as f32;
            cell.moisture = (cell.moisture + (0.25 * forcing) as f32).clamp(0.0, 1.0);
            cell.biome = classify(cell);
        }
        view.seasonal_phase = Some(phase);
        Ok(view)
    }

    pub fn generate(config: WorldConfig) -> Result<Self> {
        config.validate()?;
        let fingerprint = serde_json::to_vec(&config).context("serialize world config")?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(GENERATOR_VERSION.as_bytes());
        hasher.update(&fingerprint);
        let id = hasher.finalize().to_hex()[..20].to_string();
        let n = config.resolution;
        let plate_sites = plates(config.seed);
        let mut cells: Vec<Cell> = (0..6usize * n as usize * n as usize)
            .into_par_iter()
            .map(|i| {
                let face = (i / (n as usize * n as usize)) as u8;
                let local = i % (n as usize * n as usize);
                let x = (local % n as usize) as u32;
                let y = (local / n as usize) as u32;
                let p = direction(
                    face,
                    2.0 * (f64::from(x) + 0.5) / f64::from(n) - 1.0,
                    2.0 * (f64::from(y) + 0.5) / f64::from(n) - 1.0,
                );
                let height = raw_height(&config, &plate_sites, p) as f32;
                let continental = plate_sites
                    .iter()
                    .max_by(|a, b| dot(p, a.site).total_cmp(&dot(p, b.site)))
                    .unwrap()
                    .continental;
                let material = if continental < 0.32 {
                    Material::Basalt
                } else if continental > 0.65 {
                    Material::Granite
                } else if noise(config.seed ^ 0x72351, p, 8.0) > 0.0 {
                    Material::Sandstone
                } else {
                    Material::Limestone
                };
                Cell {
                    position: p,
                    height_m: height,
                    water_m: height,
                    temperature_c: 0.0,
                    moisture: 0.0,
                    discharge: 0.0,
                    biome: Biome::Rock,
                    material,
                    erodability: material.erodability(),
                    receiver: None,
                    area_m2: cell_area(face, x, y, n, config.radius_m),
                    ocean: false,
                    lake: false,
                }
            })
            .collect();
        let sea_level_m = choose_sea_level(&cells, &config);
        for cell in &mut cells {
            cell.height_m -= sea_level_m;
        }
        let order = drainage(&mut cells, n);
        climate(&mut cells, &config);
        accumulate(&mut cells, &order);
        // Bounded synthetic incision; a second global solve updates lake spill levels.
        for cell in &mut cells {
            if !cell.ocean && !cell.lake {
                let incision = (cell.discharge / 180.0).sqrt().min(1.5)
                    * config.relief_m as f32
                    * 0.012
                    * cell.erodability;
                cell.height_m -= incision.min(cell.height_m.max(0.0) * 0.15);
            }
        }
        let order = drainage(&mut cells, n);
        climate(&mut cells, &config);
        let (total_runoff, outlet_discharge) = accumulate(&mut cells, &order);
        for cell in &mut cells {
            cell.biome = classify(cell);
            if !cell.ocean && cell.height_m < 220.0 && cell.discharge > 1500.0 {
                cell.material = Material::Alluvium;
            }
            if cell.biome == Biome::Ice {
                cell.material = Material::Ice;
            }
            cell.erodability = cell.material.erodability();
        }
        let surface_area: f64 = cells.iter().map(|c| c.area_m2).sum();
        let ocean_area: f64 = cells.iter().filter(|c| c.ocean).map(|c| c.area_m2).sum();
        let stats = WorldStats {
            cells: cells.len() as u64,
            land_fraction: 1.0 - ocean_area / surface_area,
            ocean_fraction: ocean_area / surface_area,
            lake_cells: cells.iter().filter(|c| c.lake).count() as u64,
            river_cells: cells
                .iter()
                .filter(|c| !c.ocean && !c.lake && c.discharge >= 120.0)
                .count() as u64,
            min_height_m: cells
                .iter()
                .map(|c| c.height_m)
                .fold(f32::INFINITY, f32::min),
            max_height_m: cells
                .iter()
                .map(|c| c.height_m)
                .fold(f32::NEG_INFINITY, f32::max),
            max_discharge_m3_s: cells.iter().map(|c| c.discharge).fold(0.0, f32::max),
            surface_area_m2: surface_area,
            total_runoff_m3_s: total_runoff,
            outlet_discharge_m3_s: outlet_discharge,
            ocean_volume_m3: cells
                .iter()
                .filter(|c| c.ocean)
                .map(|c| -f64::from(c.height_m) * c.area_m2)
                .sum(),
            water_balance_relative_error: (total_runoff - outlet_discharge).abs()
                / total_runoff.max(1.0),
        };
        let world = Self {
            config,
            id,
            cells,
            sea_level_m,
            stats,
            seasonal_phase: None,
        };
        world.validate()?;
        Ok(world)
    }

    /// Independently recompute drainage indegrees and outlet conservation.
    pub fn validate(&self) -> Result<()> {
        self.config.validate()?;
        let expected = 6usize * self.config.resolution as usize * self.config.resolution as usize;
        if self.cells.len() != expected {
            bail!("atlas size does not match resolution");
        }
        let mut indegrees = vec![0usize; expected];
        let mut recomputed = vec![0.0f64; expected];
        let annual_cells = if self.seasonal_phase.is_some() {
            let mut cells = self.cells.clone();
            climate(&mut cells, &self.config);
            Some(cells)
        } else {
            None
        };
        for (i, c) in self.cells.iter().enumerate() {
            if !c.height_m.is_finite()
                || !c.water_m.is_finite()
                || !c.discharge.is_finite()
                || !c.moisture.is_finite()
                || !c.temperature_c.is_finite()
                || !c.area_m2.is_finite()
                || c.area_m2 <= 0.0
                || (dot(c.position, c.position) - 1.0).abs() > 1e-10
                || !(0.0..=1.0).contains(&c.moisture)
                || c.discharge < 0.0
            {
                bail!("invalid physical value at cell {i}");
            }
            if !c.ocean {
                let annual = annual_cells.as_ref().map_or(c, |cells| &cells[i]);
                let evap = (f64::from(annual.temperature_c) / 45.0).clamp(0.0, 0.75);
                recomputed[i] = c.area_m2 * f64::from(annual.moisture).powi(2) * 1.6 * (1.0 - evap)
                    / 31_557_600.0;
            }
            if let Some(receiver) = c.receiver {
                if receiver >= expected || receiver == i {
                    bail!("invalid receiver at cell {i}");
                }
                if !neighbours(i, self.config.resolution).contains(&receiver) {
                    bail!("nonadjacent receiver at cell {i}");
                }
                if self.cells[receiver].water_m > c.water_m + 0.002 {
                    bail!("uphill water routing at cell {i}");
                }
                indegrees[receiver] += 1;
            } else if !c.ocean {
                bail!("land cell {i} has no outlet");
            }
        }
        let mut queue: VecDeque<usize> = indegrees
            .iter()
            .enumerate()
            .filter(|(_, d)| **d == 0)
            .map(|(i, _)| i)
            .collect();
        let mut visited = 0;
        while let Some(i) = queue.pop_front() {
            visited += 1;
            let residual = (recomputed[i] - f64::from(self.cells[i].discharge)).abs();
            if residual > 1e-5 * recomputed[i].max(1.0) {
                bail!(
                    "stored discharge disagrees with independently accumulated runoff at cell {i}"
                );
            }
            if let Some(receiver) = self.cells[i].receiver {
                recomputed[receiver] += recomputed[i];
                indegrees[receiver] -= 1;
                if indegrees[receiver] == 0 {
                    queue.push_back(receiver);
                }
            }
        }
        if visited != expected {
            bail!("drainage contains a cycle");
        }
        let expected_area = 4.0 * std::f64::consts::PI * self.config.radius_m.powi(2);
        let area: f64 = self.cells.iter().map(|c| c.area_m2).sum();
        if (area - expected_area).abs() / expected_area > 1e-8 {
            bail!("spherical cell areas do not cover the planet");
        }
        if self.stats.water_balance_relative_error > 1e-8 {
            bail!("water budget did not close");
        }
        Ok(())
    }

    /// Bilinear atlas interpolation with a continuous partition at cube seams. Fine noise is
    /// addressed by position, never by camera, tile seed, cache or request order.
    pub fn sample(&self, p: [f64; 3]) -> Sample {
        let p = normalize(p);
        let n = self.config.resolution;
        let mut candidates = [(0usize, 0.0f64); 12];
        let mut candidate_count = 0;
        let mut total_weight = 0.0;
        for (face, [normal, basis_u, basis_v]) in FACE_BASES.iter().enumerate() {
            let plane = dot(p, *normal);
            if plane <= 0.0 {
                continue;
            }
            let u = dot(p, *basis_u) / plane;
            let v = dot(p, *basis_v) / plane;
            let face_weight = (1.0 + 1.0 / f64::from(n) - u.abs().max(v.abs())).max(0.0);
            if face_weight == 0.0 {
                continue;
            }
            let fx = ((u + 1.0) * 0.5 * f64::from(n) - 0.5).clamp(0.0, f64::from(n - 1));
            let fy = ((v + 1.0) * 0.5 * f64::from(n) - 0.5).clamp(0.0, f64::from(n - 1));
            let ix = fx.floor() as u32;
            let iy = fy.floor() as u32;
            let tx = fx - f64::from(ix);
            let ty = fy - f64::from(iy);
            for (dx, dy, w) in [
                (0, 0, (1.0 - tx) * (1.0 - ty)),
                (1, 0, tx * (1.0 - ty)),
                (0, 1, (1.0 - tx) * ty),
                (1, 1, tx * ty),
            ] {
                let index = cell_index(face as u8, (ix + dx).min(n - 1), (iy + dy).min(n - 1), n);
                candidates[candidate_count] = (index, w * face_weight);
                candidate_count += 1;
                total_weight += w * face_weight;
            }
        }
        let mut height = 0.0;
        let mut temperature = 0.0;
        let mut moisture = 0.0;
        let mut discharge = 0.0;
        let mut lake_fraction = 0.0;
        let mut ocean_fraction = 0.0;
        let mut water_depth = 0.0;
        let mut erodability = 0.0;
        let mut selected = 0usize;
        let mut max_weight = -1.0;
        let mut river = 0.0f64;
        let mut biome_mix = [0.0f32; 10];
        for &(index, raw_weight) in &candidates[..candidate_count] {
            let w = raw_weight / total_weight;
            let cell = &self.cells[index];
            height += w * f64::from(cell.height_m);
            temperature += w * f64::from(cell.temperature_c);
            moisture += w * f64::from(cell.moisture);
            discharge += w * f64::from(cell.discharge);
            lake_fraction += w * f64::from(u8::from(cell.lake));
            ocean_fraction += w * f64::from(u8::from(cell.ocean));
            water_depth += w * f64::from((cell.water_m - cell.height_m).max(0.0));
            erodability += w * f64::from(cell.erodability);
            biome_mix[cell.biome as usize] += w as f32;
            if w > max_weight {
                selected = index;
                max_weight = w;
            }
            if cell.discharge >= 120.0 && !cell.ocean && !cell.lake {
                if let Some(receiver) = cell.receiver {
                    let end = &self.cells[receiver];
                    let a = cell.position;
                    let b = end.position;
                    let segment = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                    let delta = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
                    let t = (dot(delta, segment) / dot(segment, segment)).clamp(0.0, 1.0);
                    let closest = normalize([
                        a[0] + t * segment[0],
                        a[1] + t * segment[1],
                        a[2] + t * segment[2],
                    ]);
                    let distance =
                        (2.0 * (1.0 - dot(p, closest)).max(0.0)).sqrt() * self.config.radius_m;
                    let width = self.channel_width(cell.discharge);
                    // Fade candidates as their interpolation support vanishes,
                    // preventing segments from disappearing at atlas-cell boundaries.
                    let support = (w * 8.0).clamp(0.0, 1.0);
                    let influence = (1.0 - distance / (width * 2.0)).clamp(0.0, 1.0) * support;
                    if influence > river {
                        river = influence;
                    }
                }
            }
        }
        let cell = &self.cells[selected];
        let coast_envelope = (height.abs() / 180.0).min(1.0);
        let river_taper = river * river * (3.0 - 2.0 * river);
        let detail = fbm(self.config.seed ^ 0x44281, p, f64::from(n) * 1.2, 14)
            * self.config.detail_m
            * coast_envelope
            * (1.0 - river_taper)
            * (1.0 - 0.92 * lake_fraction)
            * (1.3 - 0.4 * erodability).clamp(0.3, 1.4);
        let coarse_height = height;
        height += detail;
        // The atlas bed cannot be imposed on an independently interpolated fine
        // surface: doing so introduced cliffs as soon as river became positive.
        // This visual channel has a bounded depth and a C1 bank taper instead.
        let channel_depth = (0.8 + discharge.max(1.0).log10() * 2.2).clamp(1.0, 20.0);
        height -= channel_depth * river_taper;
        let ocean = ocean_fraction > 0.0 && height <= 0.0;
        let lake = !ocean && lake_fraction > 0.0 && water_depth > 0.5;
        let water = if ocean {
            0.0
        } else if lake {
            coarse_height + water_depth
        } else {
            height + river * 1.5
        };
        let mut biome = cell.biome;
        if river > 0.1 && matches!(biome, Biome::Desert | Biome::Grassland) {
            biome = Biome::Wetland;
        }
        Sample {
            direction: p,
            height_m: height as f32,
            water_m: water.max(height) as f32,
            temperature_c: temperature as f32,
            moisture: moisture as f32,
            discharge: discharge as f32,
            biome,
            biome_mix,
            material: cell.material,
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
        let count = 1u32 << key.level;
        if key.x >= count || key.y >= count || !(2..=256).contains(&segments) {
            bail!("invalid tile coordinates or segments (2..=256)");
        }
        let mut vertices = Vec::with_capacity(((segments + 1) * (segments + 1)) as usize);
        for y in 0..=segments {
            for x in 0..=segments {
                // The integer numerator makes shared dyadic edges identically addressed.
                let denominator = f64::from(count) * f64::from(segments);
                let u = 2.0 * (f64::from(key.x) * f64::from(segments) + f64::from(x)) / denominator
                    - 1.0;
                let v = 2.0 * (f64::from(key.y) * f64::from(segments) + f64::from(y)) / denominator
                    - 1.0;
                vertices.push(self.sample(direction(key.face, u, v)));
            }
        }
        let mut indices = Vec::with_capacity((segments * segments * 6) as usize);
        for y in 0..segments {
            for x in 0..segments {
                let a = y * (segments + 1) + x;
                let b = a + 1;
                let c = a + segments + 1;
                let d = c + 1;
                indices.extend_from_slice(&[a, b, d, a, d, c]);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn small_world() -> World {
        World::generate(WorldConfig {
            resolution: 16,
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn deterministic_global_drainage_conserves_water() {
        let world = small_world();
        world.validate().unwrap();
        assert!(world.stats.river_cells > 0);
        assert!(world.stats.ocean_fraction > 0.1 && world.stats.ocean_fraction < 0.9);
        assert!(world.stats.water_balance_relative_error < 1e-12);
        let again = small_world();
        assert_eq!(world.id, again.id);
        for (a, b) in world.cells.iter().zip(&again.cells) {
            assert_eq!(a.height_m.to_bits(), b.height_m.to_bits());
            assert_eq!(a.receiver, b.receiver);
            assert_eq!(a.discharge.to_bits(), b.discharge.to_bits());
        }
    }

    #[test]
    fn neighbouring_tiles_share_exact_samples() {
        let world = small_world();
        let a = world
            .tile(
                TileKey {
                    face: 0,
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
                    face: 0,
                    level: 1,
                    x: 1,
                    y: 0,
                },
                8,
            )
            .unwrap();
        for y in 0..=8 {
            let left = &a.vertices[y * 9 + 8];
            let right = &b.vertices[y * 9];
            assert_eq!(left.direction, right.direction);
            assert_eq!(left.height_m.to_bits(), right.height_m.to_bits());
        }
        // Independent face coordinates converge to the same surface at a seam.
        for v in [-0.9, -0.43, 0.0, 0.62, 0.91] {
            let before = world.sample(direction(0, 1.0 - 1e-9, v));
            let after = world.sample(direction(0, 1.0 + 1e-9, v));
            assert!(
                (before.height_m - after.height_m).abs() < 0.002,
                "seam v={v}: before={before:?}; after={after:?}"
            );
        }
    }

    #[test]
    fn volume_mode_and_invalid_configs_are_checked() {
        let mut config = WorldConfig {
            resolution: 8,
            ..Default::default()
        };
        config.radius_m = f64::NAN;
        assert!(World::generate(config).is_err());
        let world = World::generate(WorldConfig {
            resolution: 8,
            water_volume_m3: Some(1e16),
            ..Default::default()
        })
        .unwrap();
        assert!(world.stats.ocean_volume_m3 > 0.0);
        let maximal_seed = World::generate(WorldConfig {
            resolution: 8,
            seed: u64::MAX,
            ..Default::default()
        });
        assert!(maximal_seed.is_ok());
    }

    #[test]
    fn seasons_lithology_and_gravity_affect_the_model() {
        let world = small_world();
        let north = normalize([0.6, 0.8, 0.0]);
        let summer = world.seasonal_sample(north, 0.33);
        let winter = world.seasonal_sample(north, 0.83);
        assert!(summer.temperature_c > winter.temperature_c + 3.0);
        let no_tilt = World::generate(WorldConfig {
            resolution: 8,
            axial_tilt_deg: 0.0,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            no_tilt.seasonal_sample(north, 0.33).temperature_c,
            no_tilt.seasonal_sample(north, 0.83).temperature_c
        );
        assert!(Material::Sandstone.erodability() > Material::Granite.erodability());
        let width = world.channel_width(1000.0);
        let mut higher_gravity = world.clone();
        higher_gravity.config.gravity_m_s2 *= 4.0;
        assert!(higher_gravity.channel_width(1000.0) < width);
        for river in world.rivers() {
            assert_eq!(world.cells[river.from].receiver, Some(river.to));
        }
        let mut corrupt = world.clone();
        let river = corrupt
            .cells
            .iter_mut()
            .find(|c| c.discharge > 10.0)
            .unwrap();
        river.discharge *= 2.0;
        assert!(corrupt.validate().is_err());
    }

    #[test]
    fn seasonal_views_preserve_the_physical_world() {
        let world = small_world();
        let summer = world.seasonal_world(0.33).unwrap();
        let winter = summer.seasonal_world(0.83).unwrap();
        assert_eq!(world.id, summer.id);
        assert_eq!(world.id, winter.id);
        assert_eq!(
            serde_json::to_string(&world.stats).unwrap(),
            serde_json::to_string(&summer.stats).unwrap()
        );
        let mut temperature_changed = false;
        for ((annual, warm), cold) in world.cells.iter().zip(&summer.cells).zip(&winter.cells) {
            assert_eq!(annual.height_m, warm.height_m);
            assert_eq!(annual.water_m, cold.water_m);
            assert_eq!(annual.receiver, warm.receiver);
            assert_eq!(annual.discharge, cold.discharge);
            temperature_changed |= warm.temperature_c != cold.temperature_c;
        }
        assert!(temperature_changed);
        summer.validate().unwrap();
        winter.validate().unwrap();
        assert!(world.seasonal_world(f64::NAN).is_err());
    }

    #[test]
    fn fine_river_banks_do_not_jump_to_incompatible_atlas_beds() {
        let world = World::generate(WorldConfig {
            resolution: 32,
            ..Default::default()
        })
        .unwrap();
        let mut max_jump = 0.0f32;
        let mut checked = 0;
        for river in world.rivers().into_iter().take(1000) {
            assert!(river.end_height_m <= river.start_height_m + 0.002);
            let center = normalize([
                river.start[0] + river.end[0],
                river.start[1] + river.end[1],
                river.start[2] + river.end[2],
            ]);
            let along = [
                river.end[0] - river.start[0],
                river.end[1] - river.start[1],
                river.end[2] - river.start[2],
            ];
            let side = normalize(cross(center, along));
            let bank_angle = 2.0 * f64::from(river.width_m) / world.config.radius_m;
            let offset = 0.10 / world.config.radius_m;
            for sign in [-1.0, 1.0] {
                let point = |angle: f64| {
                    normalize(std::array::from_fn(|i| {
                        center[i] * angle.cos() + side[i] * sign * angle.sin()
                    }))
                };
                let before = world.sample(point(bank_angle - offset));
                let after = world.sample(point(bank_angle + offset));
                max_jump = max_jump.max((before.height_m - after.height_m).abs());
                assert!((before.biome_mix.iter().sum::<f32>() - 1.0).abs() < 1e-5);
                checked += 1;
            }
        }
        assert!(checked > 100);
        assert!(
            max_jump < 1.0,
            "fine river bank jumped {max_jump}m over 20cm"
        );
    }
}
