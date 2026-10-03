//! Deterministic prototype planet compiler: global spherical drainage, persistent
//! atlas and continuous spatial detail. This is a reduced model, not a climate GCM.

mod coast;
pub mod erosion;
pub mod geometry;
pub mod hydrology;
mod lake_geometry;
pub mod landforms;
pub mod local_water;
mod regions;
pub mod river_geometry;
mod sampler;
pub mod tile_wire;
pub use regions::RegionPatch;

use anyhow::{bail, Context, Result};
use geometry::{cell_area, cross, direction, dot, normalize};
pub use geometry::{CubeFace, TileKey};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, VecDeque},
    sync::OnceLock,
};

pub const GENERATOR_VERSION: &str = "prototype-0.9.0";

/// Optional native diagnostic; it never changes the generated data.
#[derive(Default)]
struct GenerationProfile {
    #[cfg(not(target_arch = "wasm32"))]
    last: Option<std::time::Instant>,
}

impl GenerationProfile {
    fn new() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            return Self {
                last: std::env::var_os("WB_PROFILE_GENERATION").map(|_| std::time::Instant::now()),
            };
        }
        #[cfg(target_arch = "wasm32")]
        Self::default()
    }

    fn mark(&mut self, phase: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(last) = self.last.as_mut() {
            let elapsed = last.elapsed().as_secs_f64() * 1000.0;
            eprintln!("generation_phase {phase} {elapsed:.3} ms");
            *last = std::time::Instant::now();
        }
        #[cfg(target_arch = "wasm32")]
        let _ = phase;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WorldConfig {
    pub seed: u64,
    pub radius_m: f64,
    pub resolution: u32,
    pub ocean_fraction: f64,
    pub relief_m: f64,
    pub detail_m: f64,
    /// Strength of the invertible spherical coastline twists, independent of detail_m.
    pub coast_detail_ratio: f64,
    /// Smallest wavelength used to displace the canonical coastline, in metres.
    pub coast_reference_spacing_m: f64,
    /// Intensity of seeded branched glacial inlets in high-latitude provinces.
    pub fjord_strength: f64,
    /// Density of seeded coastal island chains and smaller satellite islands.
    pub archipelago_density: f64,
    /// Density of small fixed basins and wetlands in glaciated shield provinces.
    pub regional_lake_density: f64,
    /// Depressions shallower than this are drained before reservoirs are built.
    pub lake_min_basin_depth_m: f64,
    pub axial_tilt_deg: f64,
    pub gravity_m_s2: f64,
    /// Alternative to ocean_fraction: total ocean water volume in cubic metres.
    pub water_volume_m3: Option<f64>,
    pub erosion_steps: u32,
    pub geological_years: f64,
    pub uplift_m_per_year: f64,
    /// K for m=0.5,n=1 with drainage area in m²: units 1/year.
    pub stream_power_k_per_year: f64,
    pub hillslope_diffusivity_m2_per_year: f64,
    pub lake_fill_years: f64,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            seed: 42,
            radius_m: 1_737_000.0,
            resolution: 128,
            ocean_fraction: 0.58,
            relief_m: 6_000.0,
            detail_m: 140.0,
            coast_detail_ratio: 0.30,
            coast_reference_spacing_m: 0.25,
            fjord_strength: 0.65,
            archipelago_density: 0.60,
            regional_lake_density: 0.55,
            lake_min_basin_depth_m: 8.0,
            axial_tilt_deg: 23.4,
            gravity_m_s2: 3.0,
            water_volume_m3: None,
            erosion_steps: 8,
            geological_years: 350_000.0,
            uplift_m_per_year: 0.00015,
            stream_power_k_per_year: 0.000008,
            hillslope_diffusivity_m2_per_year: 0.8,
            lake_fill_years: 25.0,
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
        for (name, value, min, max) in [
            ("coast_detail_ratio", self.coast_detail_ratio, 0.0, 1.0),
            ("fjord_strength", self.fjord_strength, 0.0, 1.0),
            ("archipelago_density", self.archipelago_density, 0.0, 1.0),
            (
                "regional_lake_density",
                self.regional_lake_density,
                0.0,
                1.0,
            ),
            (
                "coast_reference_spacing_m",
                self.coast_reference_spacing_m,
                0.25,
                10_000.0,
            ),
            (
                "lake_min_basin_depth_m",
                self.lake_min_basin_depth_m,
                0.0,
                500.0,
            ),
        ] {
            if !value.is_finite() || value < min || value > max {
                bail!("{name} must be finite and in {min}..={max}");
            }
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
        if self.erosion_steps > 256 {
            bail!("erosion_steps must be between 0 and 256");
        }
        for (name, value, min, max) in [
            (
                "geological_years",
                self.geological_years,
                1.0,
                100_000_000.0,
            ),
            ("uplift_m_per_year", self.uplift_m_per_year, 0.0, 0.01),
            (
                "stream_power_k_per_year",
                self.stream_power_k_per_year,
                0.0,
                0.01,
            ),
            (
                "hillslope_diffusivity_m2_per_year",
                self.hillslope_diffusivity_m2_per_year,
                0.0,
                1000.0,
            ),
            ("lake_fill_years", self.lake_fill_years, 0.01, 10000.0),
        ] {
            if !value.is_finite() || value < min || value > max {
                bail!("{name} must be finite and in {min}..={max}");
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
    #[serde(default)]
    pub lake_id: Option<u64>,
    /// Potential spill surface used for analysis; never the rendered water surface.
    #[serde(default)]
    pub routing_height_m: f32,
    #[serde(default)]
    pub bedrock_m: f32,
    #[serde(default)]
    pub sediment_m: f32,
    #[serde(default)]
    pub uplift_m_per_year: f32,
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
    #[serde(default)]
    pub lake_count: u64,
    #[serde(default)]
    pub lake_storage_m3: f64,
    #[serde(default)]
    pub lake_evaporation_m3_s: f64,
    #[serde(default)]
    pub lake_infiltration_m3_s: f64,
    #[serde(default)]
    pub lake_storage_change_m3_s: f64,
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
    #[serde(default)]
    pub lakes: Vec<hydrology::Lake>,
    #[serde(default)]
    pub erosion: erosion::ErosionReport,
    /// Reconstructible geometry for the frozen atlas. Mutators must invalidate it.
    #[serde(skip)]
    pub river_cache: OnceLock<Vec<Option<Box<river_geometry::RiverReach>>>>,
    #[serde(skip)]
    pub(crate) hydraulic_cache: OnceLock<std::sync::Arc<lake_geometry::LakeDomains>>,
    #[serde(skip)]
    pub(crate) landform_cache: OnceLock<std::sync::Arc<landforms::Landforms>>,
    #[serde(skip)]
    pub(crate) local_water_cache: OnceLock<std::sync::Arc<local_water::LocalWaterAtlas>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub direction: [f64; 3],
    pub height_m: f32,
    pub water_m: f32,
    /// Canonical hydraulic level, also supplied on dry banks for shoreline clipping.
    #[serde(default)]
    pub water_level_m: Option<f32>,
    /// 0 = ocean, low IDs = lakes, bit 52 = river reaches (exact in JSON).
    #[serde(default)]
    pub water_body_id: Option<u64>,
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

fn atlas_fbm(seed: u64, p: [f64; 3], frequency: f64, octaves: u32, resolution: u32) -> f64 {
    let cutoff = f64::from(resolution) * 0.35;
    let mut value = 0.0;
    let mut amplitude = 1.0;
    let mut frequency = frequency;
    for octave in 0..octaves {
        let weight = (1.0 - (frequency / cutoff).powi(4)).max(0.0);
        value +=
            amplitude * weight * noise(seed.wrapping_add(u64::from(octave) * 97), p, frequency);
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
    // The global atlas must not sample detail below its Nyquist scale: doing so
    // created isolated one-cell pits that were subsequently mistaken for lakes.
    let texture = atlas_fbm(config.seed ^ 0x9832, p, 18.0, 4, config.resolution);
    let ridge = (1.0 - noise(config.seed ^ 0x3729, p, 23.0).abs()).powi(3);
    let mountain = convergence.max(0.0) * boundary * (0.30 + 0.70 * ridge);
    let trench = convergence.min(0.0) * boundary * 0.25;
    config.relief_m * (continent + 0.65 * mountain + trench + 0.10 * texture)
}

fn tectonic_uplift(config: &WorldConfig, plates: &[Plate], p: [f64; 3]) -> f32 {
    let mut nearest = [(f64::NEG_INFINITY, 0usize); 2];
    for (i, plate) in plates.iter().enumerate() {
        let score = dot(p, plate.site);
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
    let normal = normalize([
        b.site[0] - a.site[0],
        b.site[1] - a.site[1],
        b.site[2] - a.site[2],
    ]);
    let convergence = dot([va[0] - vb[0], va[1] - vb[1], va[2] - vb[2]], normal).max(0.0);
    let boundary = (-((nearest[0].0 - nearest[1].0) / 0.065).powi(2)).exp();
    (config.uplift_m_per_year * (0.12 + 3.0 * convergence * boundary)) as f32
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
        for next in hydrology::neighbours8(index, config.resolution) {
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

/// Latitude, tilt and seeded precipitation stay fixed while erosion changes
/// altitude. Keep the unrounded temperature baseline so each application is
/// identical to evaluating the full climate expression at the new height.
pub(crate) struct ClimateField {
    sea_level_temperature_c: Vec<f64>,
}

impl ClimateField {
    fn compile(cells: &mut [Cell], config: &WorldConfig) -> Self {
        let tilt = config.axial_tilt_deg.to_radians();
        let equator_flux = annual_insolation(0.0, tilt);
        let sea_level_temperature_c = cells
            .par_iter_mut()
            .map(|cell| {
                let latitude = cell.position[1].asin().abs();
                let latitude_deg = latitude.to_degrees();
                let temperature = 30.0 + 300.0 * (annual_insolation(latitude, tilt) - equator_flux);
                // Latitude circulation bands plus spatial variability; not a GCM.
                let equatorial = (-((latitude_deg - 6.0) / 18.0).powi(2)).exp();
                let temperate = (-((latitude_deg - 52.0) / 17.0).powi(2)).exp();
                let dryness = (-((latitude_deg - 28.0) / 10.0).powi(2)).exp();
                let precipitation = 0.35 + 0.60 * equatorial + 0.35 * temperate - 0.34 * dryness
                    + 0.22 * fbm(config.seed ^ 0x8142, cell.position, 5.0, 3);
                cell.moisture = precipitation.clamp(0.01, 1.0) as f32;
                temperature
            })
            .collect();
        Self {
            sea_level_temperature_c,
        }
    }

    pub(crate) fn apply(&self, cells: &mut [Cell]) {
        cells
            .par_iter_mut()
            .zip(&self.sea_level_temperature_c)
            .for_each(|(cell, &temperature)| {
                cell.temperature_c =
                    (temperature - 0.0065 * f64::from(cell.height_m.max(0.0))) as f32;
            });
    }
}

fn climate(cells: &mut [Cell], config: &WorldConfig) {
    ClimateField::compile(cells, config).apply(cells);
}

/// Geography uses annual forcing even when a presentation clone contains
/// seasonal climate. Ordinary worlds retain their stored, authoritative values.
pub(crate) fn annual_cell_climate(world: &World, cell: &Cell) -> (f64, f64) {
    if world.seasonal_phase.is_none() {
        return (f64::from(cell.temperature_c), f64::from(cell.moisture));
    }
    let tilt = world.config.axial_tilt_deg.to_radians();
    let latitude = cell.position[1].asin().abs();
    let latitude_deg = latitude.to_degrees();
    let temperature = 30.0
        + 300.0 * (annual_insolation(latitude, tilt) - annual_insolation(0.0, tilt))
        - 0.0065 * f64::from(cell.height_m.max(0.0));
    let equatorial = (-((latitude_deg - 6.0) / 18.0).powi(2)).exp();
    let temperate = (-((latitude_deg - 52.0) / 17.0).powi(2)).exp();
    let dryness = (-((latitude_deg - 28.0) / 10.0).powi(2)).exp();
    let moisture = 0.35 + 0.60 * equatorial + 0.35 * temperate - 0.34 * dryness
        + 0.22 * fbm(world.config.seed ^ 0x8142, cell.position, 5.0, 3);
    (
        f64::from(temperature as f32),
        f64::from(moisture.clamp(0.01, 1.0) as f32),
    )
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
    pub fn invalidate_geometry(&mut self) {
        self.river_cache.take();
        self.hydraulic_cache.take();
        self.landform_cache.take();
        self.local_water_cache.take();
    }

    pub fn river_reach(&self, index: usize) -> Option<&river_geometry::RiverReach> {
        self.river_cache
            .get_or_init(|| river_geometry::compile(self))
            .get(index)?
            .as_deref()
    }

    #[cfg(test)]
    fn channel_width(&self, discharge: f32) -> f64 {
        river_geometry::width(self, f64::from(discharge))
    }

    pub fn rivers(&self) -> Vec<RiverSegment> {
        self.river_segments(usize::MAX)
    }

    /// Orbital display is bounded independently from the complete physical graph.
    /// Detailed tile sampling always uses every reach in the canonical catalogue.
    pub fn river_segments(&self, max_reaches: usize) -> Vec<RiverSegment> {
        let catalog = self
            .river_cache
            .get_or_init(|| river_geometry::compile(self));
        let mut selected: Vec<_> = catalog.iter().flatten().collect();
        if selected.len() > max_reaches {
            selected.sort_by(|a, b| {
                b.discharge_m3_s
                    .total_cmp(&a.discharge_m3_s)
                    .then(a.from.cmp(&b.from))
            });
            selected.truncate(max_reaches);
        }
        selected
            .into_iter()
            .flat_map(|reach| {
                (0..16).map(move |step| {
                    let t0 = step as f64 / 16.0;
                    let t1 = (step + 1) as f64 / 16.0;
                    RiverSegment {
                        id: format!("{}-river-{}-{step}", self.id, reach.from),
                        from: reach.from,
                        to: reach.to,
                        start: reach.position(t0),
                        end: reach.position(t1),
                        start_height_m: reach.level(t0) as f32,
                        end_height_m: reach.level(t1) as f32,
                        discharge_m3_s: reach.discharge_m3_s as f32,
                        width_m: reach.width((t0 + t1) * 0.5) as f32,
                    }
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
        let mut profile = GenerationProfile::new();
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
                    lake_id: None,
                    routing_height_m: height,
                    bedrock_m: height,
                    sediment_m: 0.0,
                    uplift_m_per_year: tectonic_uplift(&config, &plate_sites, p),
                }
            })
            .collect();
        profile.mark("initial_terrain");
        let mut sea_level_m = choose_sea_level(&cells, &config);
        for cell in &mut cells {
            cell.height_m -= sea_level_m;
            cell.bedrock_m = cell.height_m;
        }
        profile.mark("initial_sea_level");
        let climate_field = ClimateField::compile(&mut cells, &config);
        profile.mark("initial_climate");
        let mut erosion = erosion::evolve(&mut cells, &config, &climate_field);
        profile.mark("erosion");
        let datum_shift = choose_sea_level(&cells, &config);
        sea_level_m += datum_shift;
        for cell in &mut cells {
            cell.height_m -= datum_shift;
            cell.bedrock_m -= datum_shift;
        }
        profile.mark("final_sea_level");
        erosion::finish_minor_basins(&mut cells, &config, &mut erosion);
        profile.mark("finish_minor_basins");
        climate_field.apply(&mut cells);
        profile.mark("final_climate");
        let (lakes, total_runoff, outlet_discharge) =
            hydrology::physical_water(&mut cells, &config);
        profile.mark("physical_water");
        let lake_evaporation: f64 = lakes.iter().map(|l| l.evaporation_m3_s).sum();
        let lake_infiltration: f64 = lakes.iter().map(|l| l.infiltration_m3_s).sum();
        let lake_storage_change: f64 = lakes.iter().map(|l| l.storage_change_m3_s).sum();
        for cell in &mut cells {
            cell.biome = classify(cell);
            if cell.sediment_m > 2.0 {
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
            water_balance_relative_error: (total_runoff
                - outlet_discharge
                - lake_evaporation
                - lake_infiltration
                - lake_storage_change)
                .abs()
                / total_runoff.max(1.0),
            lake_count: lakes.len() as u64,
            lake_storage_m3: lakes.iter().map(|l| l.volume_m3).sum(),
            lake_evaporation_m3_s: lake_evaporation,
            lake_infiltration_m3_s: lake_infiltration,
            lake_storage_change_m3_s: lake_storage_change,
        };
        let world = Self {
            config,
            id,
            cells,
            sea_level_m,
            stats,
            seasonal_phase: None,
            lakes,
            erosion,
            river_cache: OnceLock::new(),
            hydraulic_cache: OnceLock::new(),
            landform_cache: OnceLock::new(),
            local_water_cache: OnceLock::new(),
        };
        world.validate()?;
        profile.mark("stats_validation");
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
        let mut losses = vec![0.0f64; expected];
        for lake in &self.lakes {
            if lake.control_cell >= expected {
                bail!("invalid lake control");
            }
            let residual = lake.inflow_m3_s
                - lake.outflow_m3_s
                - lake.evaporation_m3_s
                - lake.infiltration_m3_s
                - lake.storage_change_m3_s;
            if residual.abs() > 1e-6 * lake.inflow_m3_s.max(1.0) {
                bail!("lake {} water budget did not close", lake.id);
            }
            losses[lake.control_cell] +=
                lake.evaporation_m3_s + lake.infiltration_m3_s + lake.storage_change_m3_s;
        }
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
                if !hydrology::neighbours8(i, self.config.resolution).contains(&receiver) {
                    bail!("nonadjacent receiver at cell {i}");
                }
                if self.cells[receiver].routing_height_m > c.routing_height_m + 0.002 {
                    bail!("uphill potential routing at cell {i}");
                }
                indegrees[receiver] += 1;
            } else if !c.ocean
                && !self
                    .lakes
                    .iter()
                    .any(|l| l.control_cell == i && l.outlet.is_none())
            {
                bail!("land cell {i} has no outlet");
            }
            if c.lake {
                let Some(lake) = self.lakes.iter().find(|l| Some(l.id) == c.lake_id) else {
                    bail!("missing lake identity at {i}");
                };
                if (c.water_m - lake.level_m).abs() > 0.002 || c.water_m < c.height_m {
                    bail!("lake surface is not its shared level at {i}");
                }
            } else if !c.ocean && c.water_m != c.height_m {
                bail!("dry cell has artificial spill water at {i}");
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
            recomputed[i] = (recomputed[i] - losses[i]).max(0.0);
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
        let sediment: f64 = self
            .cells
            .iter()
            .map(|c| f64::from(c.sediment_m) * c.area_m2 * 1700.0)
            .sum();
        if (sediment - self.erosion.sediment_stored_kg).abs()
            / self.erosion.sediment_stored_kg.max(1.0)
            > 1e-6
            || self.erosion.mass_balance_relative_error > 1e-9
        {
            bail!("sediment mass budget did not close");
        }
        Ok(())
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
    fn cached_climate_keeps_exact_analytic_values_after_height_changes() {
        let config = WorldConfig {
            resolution: 8,
            erosion_steps: 0,
            ..Default::default()
        };
        let mut cells = World::generate(config.clone()).unwrap().cells;
        let field = ClimateField::compile(&mut cells, &config);
        let moisture: Vec<_> = cells.iter().map(|c| c.moisture.to_bits()).collect();
        let tilt = config.axial_tilt_deg.to_radians();
        let equator_flux = annual_insolation(0.0, tilt);
        for height_offset in [-20_000.0, 350.25, 10_000.0] {
            for (i, cell) in cells.iter_mut().enumerate() {
                cell.height_m = height_offset + i as f32 * 0.25;
            }
            field.apply(&mut cells);
            for (i, cell) in cells.iter().enumerate() {
                let latitude = cell.position[1].asin().abs();
                let expected = (30.0 + 300.0 * (annual_insolation(latitude, tilt) - equator_flux)
                    - 0.0065 * f64::from(cell.height_m.max(0.0)))
                    as f32;
                assert_eq!(cell.temperature_c.to_bits(), expected.to_bits());
                assert_eq!(cell.moisture.to_bits(), moisture[i]);
            }
        }
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
        let mut max_case = None;
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
                assert!((before.biome_mix.iter().sum::<f32>() - 1.0).abs() < 1e-5);
                let jump = (before.height_m - after.height_m).abs();
                if jump > max_jump {
                    max_jump = jump;
                    max_case = Some((river.from, river.to, point(bank_angle), before, after));
                }
                checked += 1;
            }
        }
        assert!(checked > 100);
        if max_jump >= 1.0 {
            let (_, _, center, before, after) = max_case.as_ref().unwrap();
            let tangent = normalize(std::array::from_fn(|i| {
                after.direction[i] - before.direction[i]
            }));
            let mut natural = world.clone();
            for cell in &mut natural.cells {
                cell.receiver = None;
            }
            natural.invalidate_geometry();
            for span in [0.2, 0.02, 0.002, 0.0002] {
                let offset = span * 0.5 / world.config.radius_m;
                let left = normalize(std::array::from_fn(|i| center[i] - tangent[i] * offset));
                let right = normalize(std::array::from_fn(|i| center[i] + tangent[i] * offset));
                eprintln!(
                    "bank_span {span}m carved_difference {}m natural_difference {}m",
                    (world.sample(left).height_m - world.sample(right).height_m).abs(),
                    (natural.sample(left).height_m - natural.sample(right).height_m).abs()
                );
            }
        }
        assert!(
            max_jump < 1.0,
            "fine river bank jumped {max_jump}m over 20cm: {max_case:?}"
        );
    }

    #[test]
    fn geological_evolution_changes_terrain_and_conserves_solid_mass() {
        let config = WorldConfig {
            resolution: 32,
            erosion_steps: 16,
            ..Default::default()
        };
        let evolved = World::generate(config.clone()).unwrap();
        let prior = World::generate(WorldConfig {
            erosion_steps: 0,
            ..config
        })
        .unwrap();
        assert!(evolved.erosion.rms_height_change_m > 10.0);
        assert!(evolved.erosion.bedrock_eroded_kg > 1e12);
        assert!(evolved.erosion.incised_cells > 100);
        assert!(evolved.erosion.mass_balance_relative_error < 1e-10);
        let independently_stored: f64 = evolved
            .cells
            .iter()
            .map(|c| f64::from(c.sediment_m) * c.area_m2 * 1700.0)
            .sum();
        assert!(
            (independently_stored + evolved.erosion.sediment_exported_kg
                - evolved.erosion.bedrock_eroded_kg)
                .abs()
                / evolved.erosion.bedrock_eroded_kg
                < 1e-6
        );
        assert_eq!(prior.erosion.bedrock_eroded_kg, 0.0);
        assert_ne!(prior.id, evolved.id);
        assert!(evolved
            .cells
            .iter()
            .filter(|c| !c.ocean && !c.lake)
            .all(|c| c.water_m == c.height_m));
    }
}
