//! Browser explorer: all geometry, precision handling, camera and rendering are Rust.
use bytemuck::{Pod, Zeroable};
use glam::{DVec3, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
mod lod;

const BASES: [[[i32; 3]; 3]; 6] = [
    [[1, 0, 0], [0, 0, -1], [0, 1, 0]],
    [[-1, 0, 0], [0, 0, 1], [0, 1, 0]],
    [[0, 1, 0], [1, 0, 0], [0, 0, -1]],
    [[0, -1, 0], [1, 0, 0], [0, 0, 1]],
    [[0, 0, 1], [1, 0, 0], [0, 1, 0]],
    [[0, 0, -1], [-1, 0, 0], [0, 1, 0]],
];

const RIVER_BODY: u64 = 1 << 52;
const MAX_TILE_LEVEL: u8 = 24;
const STREAM_SEGMENTS: usize = 64;
const DEEP_STREAM_SEGMENTS: usize = 32;
// Halving a tile's grid requires twice the tile frequency for the same
// physical vertex spacing, hence up to four times as many visible leaves.
const MAX_VISIBLE_TILES: usize = 1024;
// Keep room for an atomic learned-field replacement, its old visible cut,
// and canonical root support inside the existing 768 MiB CPU/GPU budgets.
const MAX_PLANNED_LEAVES: usize = 576;
const MAX_PREFETCH_TILES: usize = 64;
// The same value is injected into WGSL. Short, smooth transitions keep a
// twenty-level descent from spending seconds waiting on completed geometry.
const MORPH_DURATION_SECONDS: f64 = 0.08;
const BIOME_DETAIL_SCALES_M: [f64; 8] = [64_000.0, 8_000.0, 1_000.0, 125.0, 16.0, 2.0, 0.5, 0.2];

const TERRAIN_MEMORY_LIMIT: usize = 768 * 1024 * 1024;

// Field revisions stay immutable while the incoming registry changes.
#[derive(Default)]
struct HybridSnapshots {
    selected: Option<String>,
    fields: HashMap<String, std::rc::Rc<world_core::hybrid::HybridField>>,
}
impl HybridSnapshots {
    fn key(field: &world_core::hybrid::HybridField) -> String {
        format!(
            "{}/{}/{}",
            field.world_id,
            field.revision,
            field.radius_m().to_bits()
        )
    }
    fn prepare(
        &mut self,
        field: world_core::hybrid::HybridField,
    ) -> std::rc::Rc<world_core::hybrid::HybridField> {
        let key = Self::key(&field);
        if let Some(existing) = self.fields.get(&key) {
            return existing.clone();
        }
        self.fields
            .retain(|candidate, _| Some(candidate) == self.selected.as_ref());
        let field = std::rc::Rc::new(field);
        self.fields.insert(key.clone(), field.clone());
        self.selected = Some(key);
        field
    }
    fn select(&mut self, key: &str) -> Option<std::rc::Rc<world_core::hybrid::HybridField>> {
        let field = self.fields.get(key)?.clone();
        self.selected = Some(key.to_owned());
        self.fields.retain(|candidate, _| candidate == key);
        Some(field)
    }
    fn procedural(&mut self) {
        self.selected = None;
        self.fields.clear();
    }
}
fn surface_refresh_keys(active: &HashSet<TileKey>, changed: &HashSet<TileKey>) -> HashSet<TileKey> {
    let mut keys: HashSet<_> = changed.intersection(active).copied().collect();
    for key in keys.iter().copied().collect::<Vec<_>>() {
        keys.extend(tile_neighbors(key, active));
    }
    // Keep the complete canonical orbital return path, including shared normals.
    if changed.iter().any(|key| key.level == 0) {
        keys.extend(lod::initial_leaves());
    }
    keys
}
fn within_surface_budget(
    current: (usize, usize),
    reserved: (usize, usize),
    limits: (usize, usize),
) -> bool {
    current
        .0
        .checked_add(reserved.0)
        .is_some_and(|n| n <= limits.0)
        && current
            .1
            .checked_add(reserved.1)
            .is_some_and(|n| n <= limits.1)
}

fn transition_needs_child_uploads(coarsening: bool, instant: bool) -> bool {
    // Instant refinement still introduces four new resident GPU meshes.
    // Only an invisible coarsening can omit the departing children's jobs.
    !coarsening || !instant
}

fn surface_cpu_reservation(base: usize, mesh: usize, gpu_cpu: usize) -> usize {
    // The decoded mesh is released immediately after upload. Only one upload
    // can temporarily overlap both representations; that scratch is global.
    base + mesh.max(gpu_cpu)
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct NoiseBand {
    cell: [i32; 4],
    phase_scale: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct NoiseFrame {
    anchor_high: [f32; 4],
    anchor_low: [f32; 4],
    bands: [NoiseBand; 8],
}

fn noise_axes(point: DVec3) -> DVec3 {
    DVec3::new(
        point.dot(DVec3::new(0.36, 0.48, 0.8)),
        point.dot(DVec3::new(-0.8, 0.6, 0.0)),
        point.dot(DVec3::new(-0.48, -0.64, 0.6)),
    )
}

fn noise_frame(anchor: DVec3, radius_m: f64) -> NoiseFrame {
    let inverse_length = 1.0 / anchor.length();
    let direction = anchor * inverse_length;
    let high = anchor.as_vec3();
    let low = (anchor - high.as_dvec3()).as_vec3();
    let world_point = noise_axes(direction) * radius_m;
    NoiseFrame {
        anchor_high: high.extend(inverse_length as f32).to_array(),
        anchor_low: low.extend(0.0).to_array(),
        bands: BIOME_DETAIL_SCALES_M.map(|scale| {
            // Integer cell IDs retain the world phase. The floating-point
            // shader only handles small differences around this f64 anchor.
            let q = world_point / scale;
            let cell = q.floor();
            NoiseBand {
                cell: [cell.x as i32, cell.y as i32, cell.z as i32, 0],
                phase_scale: [
                    (q.x - cell.x) as f32,
                    (q.y - cell.y) as f32,
                    (q.z - cell.z) as f32,
                    scale as f32,
                ],
            }
        }),
    }
}

fn shader_source() -> String {
    format!(
        "const MORPH_DURATION_SECONDS: f32 = {MORPH_DURATION_SECONDS};\n{}",
        include_str!("shader.wgsl")
    )
}

fn canonical_tile_vertex_count(key: TileKey, root_segments: usize) -> usize {
    let segments = if key.level == 0 {
        root_segments
    } else if key.level <= 2 {
        STREAM_SEGMENTS
    } else {
        DEEP_STREAM_SEGMENTS
    };
    (segments + 1).pow(2)
}

fn valid_tile_vertex_count(key: TileKey, count: usize, root_segments: usize) -> bool {
    count == canonical_tile_vertex_count(key,root_segments)
        // Legacy contracts hash the segment count into their field. Retain
        // their standard 64 grid rather than silently requesting a new field.
        || key.level>=3 && count == (STREAM_SEGMENTS+1).pow(2)
}

fn cut_prepare_budget(motion_age: f64, render_ms: f64, prepare_ms: f64) -> f64 {
    let target_ms: f64 = if motion_age < 0.15 { 4.0 } else { 8.0 };
    let other_ms = (render_ms - prepare_ms).max(0.0);
    target_ms.min((16.0 - other_ms - 2.0).max(4.0)) * 0.001
}

#[derive(Clone, Deserialize)]
struct Config {
    #[serde(default)]
    seed: u64,
    radius_m: f64,
    resolution: usize,
}

#[derive(Clone, Deserialize)]
#[serde(default)]
struct Sample {
    #[serde(alias = "position")]
    direction: [f64; 3],
    #[serde(deserialize_with = "native_f32")]
    height_m: f64,
    #[serde(deserialize_with = "native_f32")]
    water_m: f64,
    #[serde(deserialize_with = "native_optional_f32")]
    water_level_m: Option<f64>,
    #[serde(alias = "lake_id")]
    water_body_id: Option<u64>,
    #[serde(deserialize_with = "native_f32")]
    temperature_c: f64,
    #[serde(deserialize_with = "native_f32")]
    moisture: f64,
    #[serde(deserialize_with = "native_f32")]
    discharge: f64,
    #[serde(deserialize_with = "native_f32")]
    river: f64,
    biome: String,
    #[serde(deserialize_with = "native_mix")]
    biome_mix: [f64; 10],
    ocean: bool,
    lake: bool,
    #[serde(deserialize_with = "native_f32")]
    hybrid_weight: f64,
}

impl Default for Sample {
    fn default() -> Self {
        Self {
            direction: [0.0; 3],
            height_m: 0.0,
            water_m: 0.0,
            water_level_m: None,
            water_body_id: None,
            temperature_c: 0.0,
            moisture: 0.0,
            discharge: 0.0,
            river: 0.0,
            biome: String::new(),
            biome_mix: [0.0; 10],
            ocean: false,
            lake: false,
            hybrid_weight: 1.0,
        }
    }
}

fn native_f32<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    f32::deserialize(d).map(f64::from)
}
fn native_optional_f32<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Option::<f32>::deserialize(d).map(|v| v.map(f64::from))
}
fn native_mix<'de, D: serde::Deserializer<'de>>(d: D) -> Result<[f64; 10], D::Error> {
    <[f32; 10]>::deserialize(d).map(|v| v.map(f64::from))
}

#[derive(Deserialize)]
struct World {
    config: Config,
    #[serde(default)]
    render_resolution: Option<usize>,
    #[serde(default)]
    render_grid: Option<String>,
    cells: Vec<Sample>,
}

impl World {
    fn mesh_resolution(&self) -> usize {
        self.render_resolution.unwrap_or(self.config.resolution)
    }
}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
struct TileKey {
    face: u8,
    level: u8,
    x: u32,
    y: u32,
}

#[derive(Default, Deserialize)]
struct Tile {
    #[serde(default)]
    key: TileKey,
    vertices: Vec<Sample>,
    indices: Vec<u32>,
    #[serde(skip)]
    morph_offsets: Vec<[f64; 3]>,
    #[serde(skip)]
    morph_start: f64,
}

fn from_core_tile(tile: world_core::Tile) -> Tile {
    Tile {
        key: TileKey {
            face: tile.key.face,
            level: tile.key.level,
            x: tile.key.x,
            y: tile.key.y,
        },
        vertices: tile
            .vertices
            .into_iter()
            .map(|s| Sample {
                direction: s.direction,
                height_m: f64::from(s.height_m),
                water_m: f64::from(s.water_m),
                water_level_m: s.water_level_m.map(f64::from),
                water_body_id: s.water_body_id,
                temperature_c: f64::from(s.temperature_c),
                moisture: f64::from(s.moisture),
                discharge: f64::from(s.discharge),
                river: f64::from(s.river),
                biome: format!("{:?}", s.biome),
                biome_mix: s.biome_mix.map(f64::from),
                ocean: s.ocean,
                lake: s.lake,
                hybrid_weight: f64::from(s.hybrid_weight),
            })
            .collect(),
        indices: tile.indices,
        ..Default::default()
    }
}

fn decode_json_tile(json: &str) -> Result<Tile, serde_json::Error> {
    // Restore native f32 field values before promoting them. Direct f64 parsing of
    // decimal f32 JSON can shift intersections along nearly horizontal shores.
    serde_json::from_str::<world_core::Tile>(json)
        .map(from_core_tile)
        .or_else(|_| serde_json::from_str(json))
}

#[derive(Default, Deserialize)]
struct RiverSegment {
    start: [f64; 3],
    end: [f64; 3],
    start_height_m: f64,
    end_height_m: f64,
    width_m: f64,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RiverLayer {
    segments: Vec<RiverSegment>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct HumanLayer {
    settlements: Vec<Settlement>,
    roads: Vec<Road>,
}

#[derive(Deserialize)]
struct Settlement {
    direction: [f64; 3],
    altitude_m: f64,
    population: u64,
}

#[derive(Deserialize)]
struct Road {
    points: Vec<[f64; 3]>,
    altitudes_m: Vec<f64>,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    position_low: [f32; 3],
    normal_height: [f32; 4],
    color: [f32; 4],
    fields: [f32; 4],
    biome_a: [f32; 4],
    biome_b: [f32; 4],
    direction_low: [f32; 3],
    morph: [f32; 4],
    shading_normal: [f32; 3],
    parent_normal: [f32; 3],
}

#[derive(Default)]
struct Mesh {
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
}

const BIOMES: [&str; 10] = [
    "Ocean",
    "Lake",
    "Ice",
    "Tundra",
    "Desert",
    "Grassland",
    "Forest",
    "Rainforest",
    "Wetland",
    "Rock",
];

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PreparedHeader {
    magic: [u8; 4],
    face: u8,
    level: u8,
    reserved: [u8; 2],
    x: u32,
    y: u32,
    radius: f64,
    exaggeration: f64,
    counts: [u32; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LegacyPackedSample {
    direction: [f64; 3],
    height: f64,
    water: f64,
    level: f64,
    body: u64,
    fields: [f32; 4],
    mix: [f32; 10],
    biome: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PackedSample {
    direction: [f64; 3],
    height: f64,
    water: f64,
    level: f64,
    body: u64,
    fields: [f32; 4],
    mix: [f32; 10],
    biome: u32,
    flags: u32,
    hybrid_weight: f32,
    reserved: u32,
}
impl From<LegacyPackedSample> for PackedSample {
    fn from(sample: LegacyPackedSample) -> Self {
        Self {
            direction: sample.direction,
            height: sample.height,
            water: sample.water,
            level: sample.level,
            body: sample.body,
            fields: sample.fields,
            mix: sample.mix,
            biome: sample.biome,
            flags: sample.flags,
            hybrid_weight: 1.0,
            reserved: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BorderNormal {
    index: u32,
    reserved: u32,
    sum: [f64; 3],
}

struct PreparedTile {
    // Immutable procedural heights: every field revision starts from these.
    base_heights: Vec<f64>,
    tile: Tile,
    mesh: Mesh,
    borders: Vec<BorderNormal>,
    radius: f64,
    exaggeration: f64,
}

fn mesh_point(vertex: &Vertex) -> DVec3 {
    DVec3::from_array(vertex.position.map(f64::from))
        + DVec3::from_array(vertex.position_low.map(f64::from))
}

fn mesh_bounds(mesh: &Mesh) -> (DVec3, f64) {
    let (mut low, mut high) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for vertex in &mesh.vertices {
        let point = mesh_point(vertex);
        low = low.min(point);
        high = high.max(point);
    }
    ((low + high) * 0.5, (high - low).length() * 0.5)
}

fn local_normal_sums(mesh: &Mesh, parent: bool) -> Vec<DVec3> {
    let mut sums = vec![DVec3::ZERO; mesh.vertices.len()];
    let position = |i: usize| {
        let v = &mesh.vertices[i];
        let p = mesh_point(v);
        if parent {
            p + DVec3::new(
                v.normal_height[0] as f64,
                v.normal_height[1] as f64,
                v.normal_height[2] as f64,
            ) * v.morph[0] as f64
        } else {
            p
        }
    };
    for triangle in mesh.indices.chunks_exact(3) {
        if mesh.vertices[triangle[0] as usize].color[3].round() as u32 & 1 != 0 {
            continue;
        }
        let [ia, ib, ic] = [
            triangle[0] as usize,
            triangle[1] as usize,
            triangle[2] as usize,
        ];
        let [a, b, c] = [position(ia), position(ib), position(ic)];
        let mut normal = (b - a).cross(c - a);
        if normal.dot(a + b + c) < 0.0 {
            normal = -normal;
        }
        for i in [ia, ib, ic] {
            sums[i] += normal;
        }
    }
    sums
}

fn local_mesh_normals(mesh: &mut Mesh) {
    for parent in [false, true] {
        let sums = local_normal_sums(mesh, parent);
        for (v, sum) in mesh.vertices.iter_mut().zip(sums) {
            let normal = if sum.length_squared() > 1e-60 {
                sum.normalize().as_vec3().to_array()
            } else {
                v.normal_height[..3].try_into().unwrap()
            };
            if parent {
                v.parent_normal = normal;
            } else {
                v.shading_normal = normal;
            }
        }
    }
}

fn compact_mesh(mesh: &mut Mesh, ground_vertices: usize) {
    let mut used = vec![false; mesh.vertices.len()];
    used[..ground_vertices].fill(true);
    for &index in &mesh.indices {
        used[index as usize] = true;
    }
    let mut mapping = vec![0u32; mesh.vertices.len()];
    let mut compact = Vec::with_capacity(used.iter().filter(|v| **v).count());
    for (i, &vertex) in mesh.vertices.iter().enumerate() {
        if used[i] {
            mapping[i] = compact.len() as u32;
            compact.push(vertex);
        }
    }
    for index in &mut mesh.indices {
        *index = mapping[*index as usize];
    }
    mesh.vertices = compact;
}

fn prepare_tile(tile: Tile, radius: f64, exaggeration: f64) -> Result<Vec<u8>, String> {
    if !radius.is_finite()
        || radius <= 0.0
        || !exaggeration.is_finite()
        || !(1.0..=30.0).contains(&exaggeration)
    {
        return Err("Paramètres de préparation invalides".into());
    }
    let mut mesh = tile_mesh(&tile, radius, exaggeration)?;
    compact_mesh(&mut mesh, tile.vertices.len());
    let side = (tile.vertices.len() as f64).sqrt() as usize;
    if side < 2 || side * side != tile.vertices.len() {
        return Err("Grille de tuile invalide".into());
    }
    let sums = local_normal_sums(&mesh, false);
    let borders: Vec<_> = (0..tile.vertices.len())
        .filter(|i| i % side == 0 || i % side == side - 1 || *i < side || *i >= side * (side - 1))
        .map(|i| BorderNormal {
            index: i as u32,
            reserved: 0,
            sum: sums[i].to_array(),
        })
        .collect();
    let samples: Vec<_> = tile
        .vertices
        .iter()
        .map(|v| PackedSample {
            direction: v.direction,
            height: v.height_m,
            water: v.water_m,
            level: v.water_level_m.unwrap_or(f64::NAN),
            body: v.water_body_id.unwrap_or(u64::MAX),
            fields: [
                v.temperature_c as f32,
                v.moisture as f32,
                v.discharge as f32,
                v.river as f32,
            ],
            mix: v.biome_mix.map(|x| x as f32),
            biome: BIOMES.iter().position(|name| *name == v.biome).unwrap_or(9) as u32,
            flags: u32::from(v.ocean) | u32::from(v.lake) << 1,
            hybrid_weight: v.hybrid_weight as f32,
            reserved: 0,
        })
        .collect();
    let header = PreparedHeader {
        magic: *b"WVP4",
        face: tile.key.face,
        level: tile.key.level,
        reserved: [0; 2],
        x: tile.key.x,
        y: tile.key.y,
        radius,
        exaggeration,
        counts: [
            samples.len() as u32,
            mesh.vertices.len() as u32,
            mesh.indices.len() as u32,
            borders.len() as u32,
            tile.indices.len() as u32,
            0,
        ],
    };
    let mut bytes = Vec::new();
    bytes.extend_from_slice(bytemuck::bytes_of(&header));
    bytes.extend_from_slice(bytemuck::cast_slice(&samples));
    bytes.extend_from_slice(bytemuck::cast_slice(&mesh.vertices));
    bytes.extend_from_slice(bytemuck::cast_slice(&mesh.indices));
    while bytes.len() % 8 != 0 {
        bytes.push(0);
    }
    bytes.extend_from_slice(bytemuck::cast_slice(&borders));
    Ok(bytes)
}

/// Apply the continuous field once, before geometry preparation. The raw tile
/// and its procedural detail remain the immutable source for later revisions.
fn hybrid_delta(field: &world_core::hybrid::HybridField, sample: &Sample, base_height: f64) -> f64 {
    field.protected_delta(
        sample.direction,
        base_height,
        sample.water_level_m,
        sample.water_body_id,
        sample.river,
        sample.ocean,
        sample.lake,
    ) * if sample.hybrid_weight.is_finite() {
        sample.hybrid_weight.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn prepare_hybrid_tile(
    mut tile: Tile,
    radius: f64,
    exaggeration: f64,
    field: Option<&world_core::hybrid::HybridField>,
) -> Result<Vec<u8>, String> {
    let base_heights: Vec<_> = tile.vertices.iter().map(|sample| sample.height_m).collect();
    if let Some(field) = field {
        for sample in &mut tile.vertices {
            sample.height_m += hybrid_delta(field, sample, sample.height_m);
            if let Some(level) = sample.water_level_m {
                sample.water_m = (level - sample.height_m).max(0.0);
            }
        }
    }
    let mut packet = prepare_tile(tile, radius, exaggeration)?;
    let size = std::mem::size_of::<PreparedHeader>();
    let mut header: PreparedHeader = bytemuck::pod_read_unaligned(&packet[..size]);
    header.magic = *b"WVP4";
    header.counts[5] = base_heights.len() as u32;
    packet[..size].copy_from_slice(bytemuck::bytes_of(&header));
    packet.extend_from_slice(bytemuck::cast_slice(&base_heights));
    Ok(packet)
}

fn decode_prepared_tile(bytes: &[u8]) -> Result<PreparedTile, String> {
    let hsize = std::mem::size_of::<PreparedHeader>();
    if bytes.len() < hsize {
        return Err("Préparation tronquée".into());
    }
    let header: PreparedHeader = bytemuck::pod_read_unaligned(&bytes[..hsize]);
    if !matches!(&header.magic, b"WVP2" | b"WVP3" | b"WVP4")
        || (header.magic == *b"WVP3" && header.counts[5] != header.counts[0])
        || (header.magic == *b"WVP2" && header.counts[5] != 0)
        || (header.magic == *b"WVP4"
            && header.counts[5] != 0
            && header.counts[5] != header.counts[0])
        || header.face >= 6
        || header.level > MAX_TILE_LEVEL
        || header.counts[0] > 263169
        || header.counts[1] > 2_000_000
        || header.counts[2] > 12_000_000
        || header.counts[3] > 4096
        || header.counts[4] > header.counts[2]
    {
        return Err("Préparation invalide".into());
    }
    let mut offset = hsize;
    fn read<T: Pod>(bytes: &[u8], offset: &mut usize, count: usize) -> Result<Vec<T>, String> {
        let size = count
            .checked_mul(std::mem::size_of::<T>())
            .ok_or("Préparation trop grande")?;
        let end = offset
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .ok_or("Préparation tronquée")?;
        let out = bytes[*offset..end]
            .chunks_exact(std::mem::size_of::<T>())
            .map(bytemuck::pod_read_unaligned)
            .collect();
        *offset = end;
        Ok(out)
    }
    let samples: Vec<PackedSample> = if header.magic == *b"WVP4" {
        read(bytes, &mut offset, header.counts[0] as usize)?
    } else {
        read::<LegacyPackedSample>(bytes, &mut offset, header.counts[0] as usize)?
            .into_iter()
            .map(PackedSample::from)
            .collect()
    };
    if samples.iter().any(|sample| {
        !sample.hybrid_weight.is_finite() || !(0.0..=1.0).contains(&sample.hybrid_weight)
    }) {
        return Err("Poids de protection hybride invalide".into());
    }
    let vertices: Vec<Vertex> = read(bytes, &mut offset, header.counts[1] as usize)?;
    let indices: Vec<u32> = read(bytes, &mut offset, header.counts[2] as usize)?;
    offset = (offset + 7) & !7;
    let borders = read(bytes, &mut offset, header.counts[3] as usize)?;
    let base_heights = if header.counts[5] > 0 {
        let heights: Vec<f64> = read(bytes, &mut offset, header.counts[5] as usize)?;
        if heights.iter().any(|height| !height.is_finite()) {
            return Err("Altitudes de référence invalides".into());
        }
        heights
    } else {
        samples.iter().map(|sample| sample.height).collect()
    };
    if offset != bytes.len() || indices.iter().any(|i| *i as usize >= vertices.len()) {
        return Err("Indices de préparation invalides".into());
    }
    let terrain_indices = indices[..header.counts[4] as usize].to_vec();
    let tile = Tile {
        key: TileKey {
            face: header.face,
            level: header.level,
            x: header.x,
            y: header.y,
        },
        indices: terrain_indices,
        vertices: samples
            .into_iter()
            .map(|v| Sample {
                direction: v.direction,
                height_m: v.height,
                water_m: v.water,
                water_level_m: v.level.is_finite().then_some(v.level),
                water_body_id: (v.body != u64::MAX).then_some(v.body),
                temperature_c: v.fields[0] as f64,
                moisture: v.fields[1] as f64,
                discharge: v.fields[2] as f64,
                river: v.fields[3] as f64,
                biome: BIOMES.get(v.biome as usize).unwrap_or(&"Rock").to_string(),
                biome_mix: v.mix.map(f64::from),
                ocean: v.flags & 1 != 0,
                lake: v.flags & 2 != 0,
                hybrid_weight: f64::from(v.hybrid_weight),
            })
            .collect(),
        ..Default::default()
    };
    Ok(PreparedTile {
        base_heights,
        tile,
        mesh: Mesh { vertices, indices },
        borders,
        radius: header.radius,
        exaggeration: header.exaggeration,
    })
}

fn biome_color(biome: &str) -> [f32; 3] {
    match biome {
        // Linear RGB: restrained inks, warm rock/sand and cool ivory ice.
        "Ocean" => [0.035, 0.16, 0.25],
        "Lake" => [0.06, 0.28, 0.30],
        "Ice" => [0.78, 0.83, 0.80],
        "Tundra" => [0.32, 0.37, 0.24],
        "Desert" => [0.65, 0.47, 0.25],
        "Grassland" => [0.32, 0.43, 0.19],
        "Forest" => [0.10, 0.24, 0.115],
        "Rainforest" => [0.055, 0.17, 0.085],
        "Wetland" => [0.18, 0.31, 0.22],
        "Rock" => [0.40, 0.35, 0.28],
        _ => [0.32, 0.40, 0.22],
    }
}

#[cfg(test)]
fn mix_color(a: [f32; 3], b: [f32; 3], weight: f64) -> [f32; 3] {
    std::array::from_fn(|i| (f64::from(a[i]) * (1.0 - weight) + f64::from(b[i]) * weight) as f32)
}

fn terrestrial_mix(sample: &Sample) -> [f32; 8] {
    let mut mix: [f32; 8] = std::array::from_fn(|i| {
        let weight = sample.biome_mix[i + 2];
        if weight.is_finite() {
            weight.max(0.0) as f32
        } else {
            0.0
        }
    });
    let total: f32 = mix.iter().sum();
    if total > 0.0 {
        for weight in &mut mix {
            *weight /= total;
        }
    } else {
        // Water-classified macro samples also participate in the bed mesh.
        // Their land ink follows climate, rather than inventing a wetland halo.
        let fallback = BIOMES[2..]
            .iter()
            .position(|name| *name == sample.biome)
            .unwrap_or_else(|| {
                if sample.temperature_c < -8.0 {
                    0
                } else if sample.temperature_c < 3.0 {
                    1
                } else if sample.moisture < 0.22 {
                    2
                } else if sample.temperature_c > 19.0 && sample.moisture > 0.70 {
                    5
                } else if sample.moisture > 0.46 {
                    4
                } else {
                    3
                }
            });
        mix[fallback] = 1.0;
    }
    mix
}

fn cartographic_land_color(sample: &Sample) -> [f32; 3] {
    let mut color = [0.0; 3];
    for (name, amount) in BIOMES[2..].iter().zip(terrestrial_mix(sample)) {
        let ink = biome_color(name);
        for i in 0..3 {
            color[i] += ink[i] * amount;
        }
    }
    color
}

fn water_style(sample: &Sample) -> f32 {
    match water_body(sample) {
        Some((id, _)) if id >= RIVER_BODY => -2.0,
        _ => sample.river as f32,
    }
}

fn cartography_seed(seed: u64) -> [u32; 4] {
    [seed as u32, (seed >> 32) as u32, 0, 0]
}

fn physical_slope_degrees(
    normal: Vec3,
    radial: DVec3,
    height: f64,
    radius: f64,
    exaggeration: f64,
) -> f64 {
    let normal = normal.as_dvec3().normalize_or_zero();
    let cosine = normal.dot(radial.normalize()).clamp(0.0001, 1.0);
    let rendered_tangent = (1.0 - cosine * cosine).max(0.0).sqrt() / cosine;
    let radial_correction =
        (radius + exaggeration * height).max(1e-6) / (radius + height).max(1e-6);
    (rendered_tangent * radial_correction / exaggeration.max(1.0))
        .atan()
        .to_degrees()
}

fn slope_cover(slope_degrees: f64) -> [f64; 3] {
    let smooth = |low: f64, high: f64| {
        let t = ((slope_degrees - low) / (high - low)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    [
        (1.0 - smooth(20.0, 55.0)).powi(2),
        (1.0 - smooth(25.0, 60.0)).powi(2),
        smooth(40.0, 65.0),
    ]
}

fn vertex(sample: &Sample, radius: f64, exaggeration: f64, water: bool) -> Vertex {
    let direction = DVec3::from_array(sample.direction).normalize();
    let direction_high = direction.as_vec3();
    let direction_low = (direction - direction_high.as_dvec3()).as_vec3();
    let land_mix = terrestrial_mix(sample);
    let height = if water {
        water_height(sample)
    } else {
        sample.height_m
    };
    let position = direction * (1.0 + height * exaggeration / radius);
    let high = position.as_vec3();
    let low = (position - high.as_dvec3()).as_vec3();
    let color = if water {
        if water_body(sample).is_some_and(|(id, _)| id == 0) {
            biome_color("Ocean")
        } else {
            biome_color("Lake")
        }
    } else {
        cartographic_land_color(sample)
    };
    let river = if sample.river > 0.0 {
        sample.river
    } else if sample.discharge > 1.0 && !sample.ocean && !sample.lake {
        (sample.discharge.ln() / 12.0).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Vertex {
        position: high.to_array(),
        position_low: low.to_array(),
        normal_height: [
            direction.x as f32,
            direction.y as f32,
            direction.z as f32,
            sample.height_m as f32 / 1000.0,
        ],
        color: [color[0], color[1], color[2], if water { 1.0 } else { 0.0 }],
        fields: [
            sample.temperature_c as f32,
            sample.moisture as f32,
            if water {
                water_style(sample)
            } else {
                river as f32
            },
            (water_height(sample) - sample.height_m) as f32,
        ],
        biome_a: land_mix[..4].try_into().unwrap(),
        biome_b: land_mix[4..].try_into().unwrap(),
        direction_low: direction_low.to_array(),
        morph: [
            0.0,
            0.0,
            (water_height(sample) - sample.height_m) as f32,
            0.0,
        ],
        shading_normal: direction.as_vec3().to_array(),
        parent_normal: direction.as_vec3().to_array(),
    }
}

fn cube_key(face: usize, x: usize, y: usize, resolution: usize) -> [i32; 3] {
    let [n, u, v] = BASES[face];
    let a = 2 * x as i32 - resolution as i32;
    let b = 2 * y as i32 - resolution as i32;
    std::array::from_fn(|i| n[i] * resolution as i32 + u[i] * a + v[i] * b)
}

/// A hydraulic level is independent of the bed, including the dry bank samples.
fn water_body(sample: &Sample) -> Option<(u64, f64)> {
    if let Some(level) = sample.water_level_m.filter(|level| level.is_finite()) {
        return Some((
            sample.water_body_id.unwrap_or(u64::from(!sample.ocean)),
            level,
        ));
    }
    if sample.ocean {
        Some((0, 0.0))
    } else if sample.lake {
        Some((sample.water_body_id.unwrap_or(1), sample.water_m))
    } else {
        None
    }
}

fn water_height(sample: &Sample) -> f64 {
    water_body(sample).map_or(sample.height_m, |(_, level)| level)
}

/// Sample the preview atlas at cube vertices without deriving a mesh from its cell polygons.
fn atlas_sample(world: &World, direction: DVec3) -> Sample {
    let n = world.mesh_resolution();
    let mut sample = Sample {
        direction: direction.to_array(),
        hybrid_weight: 0.0,
        ..Default::default()
    };
    let mut candidates = Vec::with_capacity(12);
    let mut total = 0.0;
    for (face, basis) in BASES.iter().enumerate() {
        let vec = |a: [i32; 3]| DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
        let plane = direction.dot(vec(basis[0]));
        if plane <= 0.0 {
            continue;
        }
        let u = direction.dot(vec(basis[1])) / plane;
        let v = direction.dot(vec(basis[2])) / plane;
        let support = (1.0 + 1.0 / n as f64 - u.abs().max(v.abs())).max(0.0);
        if support == 0.0 {
            continue;
        }
        let x = ((u + 1.0) * 0.5 * n as f64 - 0.5).clamp(0.0, (n - 1) as f64);
        let y = ((v + 1.0) * 0.5 * n as f64 - 0.5).clamp(0.0, (n - 1) as f64);
        let ix = x.floor() as usize;
        let iy = y.floor() as usize;
        let tx = x - ix as f64;
        let ty = y - iy as f64;
        for (dx, dy, w) in [
            (0, 0, (1.0 - tx) * (1.0 - ty)),
            (1, 0, tx * (1.0 - ty)),
            (0, 1, (1.0 - tx) * ty),
            (1, 1, tx * ty),
        ] {
            let i = face * n * n + (iy + dy).min(n - 1) * n + (ix + dx).min(n - 1);
            candidates.push((i, w * support));
            total += w * support;
        }
    }
    let mut bodies: HashMap<u64, (f64, f64)> = HashMap::new();
    let mut selected = (0.0, 0usize);
    let names = [
        "Ocean",
        "Lake",
        "Ice",
        "Tundra",
        "Desert",
        "Grassland",
        "Forest",
        "Rainforest",
        "Wetland",
        "Rock",
    ];
    for (i, raw) in candidates {
        let w = raw / total;
        let cell = &world.cells[i];
        sample.height_m += w * cell.height_m;
        sample.temperature_c += w * cell.temperature_c;
        sample.moisture += w * cell.moisture;
        sample.discharge += w * cell.discharge;
        sample.river += w * cell.river;
        sample.hybrid_weight += w * cell.hybrid_weight;
        if cell.biome_mix.iter().sum::<f64>() > 0.0 {
            for j in 0..10 {
                sample.biome_mix[j] += w * cell.biome_mix[j];
            }
        } else if let Some(j) = names.iter().position(|name| *name == cell.biome) {
            sample.biome_mix[j] += w;
        }
        if let Some((id, level)) = water_body(cell) {
            // A subcell river profile cannot be expanded into a planetary water plane.
            // Its real channel geometry is supplied by the streamed tiles.
            if id < RIVER_BODY {
                let entry = bodies.entry(id).or_default();
                entry.0 += w;
                entry.1 += w * level;
            }
        }
        if w > selected.0 {
            selected = (w, i);
        }
    }
    sample.biome = world.cells[selected.1].biome.clone();
    if let Some((id, (weight, levels))) =
        bodies.into_iter().max_by(|a, b| a.1 .0.total_cmp(&b.1 .0))
    {
        let level = levels / weight;
        sample.water_level_m = Some(level);
        sample.water_body_id = Some(id);
        sample.ocean = id == 0 && sample.height_m < level;
        sample.lake = id != 0 && sample.height_m < level;
        sample.water_m = sample.height_m.max(level);
    } else {
        sample.water_m = sample.height_m;
    }
    sample
}

fn interpolate_water_vertex(
    a: Vertex,
    b: Vertex,
    t: f64,
    level: f64,
    radius: f64,
    exaggeration: f64,
) -> Vertex {
    let mut out = a;
    let mix = |x: f32, y: f32| (x as f64 * (1.0 - t) + y as f64 * t) as f32;
    for i in 0..4 {
        out.normal_height[i] = mix(a.normal_height[i], b.normal_height[i]);
        out.color[i] = mix(a.color[i], b.color[i]);
        out.fields[i] = mix(a.fields[i], b.fields[i]);
        out.morph[i] = mix(a.morph[i], b.morph[i]);
        out.biome_a[i] = mix(a.biome_a[i], b.biome_a[i]);
        out.biome_b[i] = mix(a.biome_b[i], b.biome_b[i]);
    }
    let direction_of = |v: Vertex| {
        (DVec3::from_array(v.position.map(f64::from))
            + DVec3::from_array(v.position_low.map(f64::from)))
        .normalize()
    };
    let da = direction_of(a);
    let db = direction_of(b);
    let direction = da.lerp(db, t).normalize();
    let position = direction * (1.0 + level * exaggeration / radius);
    let high = position.as_vec3();
    out.position = high.to_array();
    out.position_low = (position - high.as_dvec3()).as_vec3().to_array();
    for i in 0..3 {
        out.normal_height[i] = direction[i] as f32;
    }
    out.direction_low = (direction - direction.as_vec3().as_dvec3())
        .as_vec3()
        .to_array();
    out.shading_normal = direction.as_vec3().to_array();
    out.parent_normal = out.shading_normal;
    out
}

/// March each terrain triangle against an independent water plane. Dry portions have no
/// water triangles at all; lake elevations never average in the surrounding dry bed.
fn append_water_geometry(
    mesh: &mut Mesh,
    samples: &[Sample],
    terrain_indices: &[u32],
    radius: f64,
    exaggeration: f64,
    skip_ocean: bool,
) {
    let water_start = samples.len() as u32;
    for triangle in terrain_indices.chunks_exact(3) {
        let mut bodies: Vec<(u64, f64)> = Vec::new();
        for &i in triangle {
            let s = &samples[i as usize];
            if let Some((id, level)) = water_body(s) {
                let id = if id >= RIVER_BODY { RIVER_BODY } else { id };
                if !(skip_ocean && id == 0)
                    && level - s.height_m > 0.005
                    && (id < RIVER_BODY || s.river > 0.0)
                    && !bodies.iter().any(|(other, _)| *other == id)
                {
                    bodies.push((id, level));
                }
            }
        }
        for (id, level) in bodies {
            let flowing = id >= RIVER_BODY;
            let mut polygon: Vec<(Vertex, f64, f64)> = triangle
                .iter()
                .map(|&i| {
                    let s = &samples[i as usize];
                    let body = water_body(s);
                    let local_level = if flowing {
                        body.filter(|(other, _)| *other >= RIVER_BODY)
                            .map_or(level, |(_, h)| h)
                    } else {
                        level
                    };
                    let mut depth = local_level - s.height_m;
                    if (flowing && (body.is_none() || s.river <= 0.0))
                        || body.is_some_and(|(other, _)| {
                            if flowing {
                                other < RIVER_BODY
                            } else {
                                other != id
                            }
                        })
                    {
                        depth = -depth.abs() - 0.01;
                    }
                    let mut v = mesh.vertices[(water_start + i) as usize];
                    v = interpolate_water_vertex(v, v, 0.0, local_level, radius, exaggeration);
                    v.morph[2] += depth as f32 - v.fields[3];
                    v.fields[3] = depth as f32;
                    if flowing {
                        v.fields[2] = -2.0;
                    }
                    v.color[..3].copy_from_slice(&biome_color(if id == 0 {
                        "Ocean"
                    } else {
                        "Lake"
                    }));
                    (v, depth, local_level)
                })
                .collect();
            if polygon.iter().all(|(_, depth, _)| *depth > 0.005)
                && triangle.iter().all(|&i| {
                    if flowing {
                        water_body(&samples[i as usize])
                            .is_some_and(|(other, _)| other >= RIVER_BODY)
                    } else {
                        (water_height(&samples[i as usize]) - level).abs() < 1e-5
                    }
                })
            {
                if flowing {
                    for &i in triangle {
                        mesh.vertices[(water_start + i) as usize].fields[2] = -2.0;
                    }
                }
                mesh.indices
                    .extend(triangle.iter().map(|&i| water_start + i));
                continue;
            }
            let original = std::mem::take(&mut polygon);
            for i in 0..original.len() {
                let (a, da, la) = original[i];
                let (b, db, lb) = original[(i + 1) % original.len()];
                if da > 0.005 {
                    polygon.push((a, da, la));
                }
                if (da > 0.005) != (db > 0.005) {
                    let t = (da / (da - db)).clamp(0.0, 1.0);
                    let local_level = la * (1.0 - t) + lb * t;
                    let mut v =
                        interpolate_water_vertex(a, b, t, local_level, radius, exaggeration);
                    v.fields[3] = 0.0;
                    polygon.push((v, 0.0, local_level));
                }
            }
            if polygon.len() < 3 {
                continue;
            }
            let start = mesh.vertices.len() as u32;
            mesh.vertices.extend(polygon.iter().map(|(v, _, _)| *v));
            for i in 1..polygon.len() - 1 {
                mesh.indices
                    .extend([start, start + i as u32, start + i as u32 + 1]);
            }
        }
    }
}

fn tile_borders(tile: &Tile, mesh: &Mesh) -> Vec<BorderNormal> {
    let side = (tile.vertices.len() as f64).sqrt() as usize;
    let sums = local_normal_sums(mesh, false);
    (0..tile.vertices.len())
        .filter(|i| i % side == 0 || i % side == side - 1 || *i < side || *i >= side * (side - 1))
        .map(|i| BorderNormal {
            index: i as u32,
            reserved: 0,
            sum: sums[i].to_array(),
        })
        .collect()
}
fn cube_direction(face: u8, u: f64, v: f64) -> DVec3 {
    let b = BASES[face as usize];
    let vec = |x: [i32; 3]| DVec3::new(x[0] as f64, x[1] as f64, x[2] as f64);
    (vec(b[0]) + vec(b[1]) * u + vec(b[2]) * v).normalize()
}
fn tile_center(key: TileKey) -> DVec3 {
    let count = (1u32 << key.level) as f64;
    cube_direction(
        key.face,
        2.0 * (key.x as f64 + 0.5) / count - 1.0,
        2.0 * (key.y as f64 + 0.5) / count - 1.0,
    )
}
fn tile_neighbors(key: TileKey, cut: &HashSet<TileKey>) -> HashSet<TileKey> {
    let count = (1u32 << key.level) as f64;
    let mut out = HashSet::new();
    for (x, y) in [
        (-1e-6, 0.25),
        (-1e-6, 0.5),
        (-1e-6, 0.75),
        (1.000001, 0.25),
        (1.000001, 0.5),
        (1.000001, 0.75),
        (0.25, -1e-6),
        (0.5, -1e-6),
        (0.75, -1e-6),
        (0.25, 1.000001),
        (0.5, 1.000001),
        (0.75, 1.000001),
        (-1e-6, -1e-6),
        (-1e-6, 1.000001),
        (1.000001, -1e-6),
        (1.000001, 1.000001),
    ] {
        let d = cube_direction(
            key.face,
            2.0 * (key.x as f64 + x) / count - 1.0,
            2.0 * (key.y as f64 + y) / count - 1.0,
        );
        let (face, u, v) = face_uv(d);
        if let Some(neighbor) = lod::covering_key(cut, face as u8, u, v) {
            if neighbor != key {
                out.insert(neighbor);
            }
        }
    }
    out
}

fn same_level_neighbors(key: TileKey) -> HashSet<TileKey> {
    let count = (1u32 << key.level) as f64;
    let mut neighbors = HashSet::new();
    for dy in -1..=1 {
        for dx in -1..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            let direction = cube_direction(
                key.face,
                2.0 * (key.x as f64 + dx as f64 + 0.5) / count - 1.0,
                2.0 * (key.y as f64 + dy as f64 + 0.5) / count - 1.0,
            );
            let (face, u, v) = face_uv(direction);
            neighbors.insert(TileKey {
                face: face as u8,
                level: key.level,
                x: (((u + 1.0) * 0.5 * count) as u32).min(count as u32 - 1),
                y: (((v + 1.0) * 0.5 * count) as u32).min(count as u32 - 1),
            });
        }
    }
    neighbors.remove(&key);
    neighbors
}

/// Close the requested split parents under 2:1 support. Splitting a parent at
/// L requires its neighboring regions to reach L, hence their L-1 parents.
/// Looking from the actual fine cell avoids refining a whole ancestor's ring.
fn graduate_targets(targets: &[TileKey], wanted: u8, direction: DVec3) -> Vec<TileKey> {
    let mut parents = HashSet::new();
    for target in targets {
        for level in (0..wanted.min(target.level.saturating_add(1))).rev() {
            if !parents.insert(lod::ancestor(*target, level).unwrap()) {
                // Every stored split parent already includes its ancestors.
                break;
            }
        }
    }
    let mut pending: Vec<_> = parents.iter().copied().collect();
    while let Some(parent) = pending.pop() {
        if parent.level == 0 {
            continue;
        }
        for neighbor in same_level_neighbors(parent) {
            for level in (0..parent.level).rev() {
                let support = lod::ancestor(neighbor, level).unwrap();
                if parents.insert(support) {
                    pending.push(support);
                } else {
                    break;
                }
            }
        }
    }
    parents.extend(targets.iter().copied());
    let mut keys: Vec<_> = parents.into_iter().collect();
    keys.sort_by(|a, b| {
        tile_center(*b)
            .dot(direction)
            .total_cmp(&tile_center(*a).dot(direction))
            .then(a.cmp(b))
    });
    keys
}

fn needs_refinement(key: TileKey, targets: &[TileKey], wanted: u8) -> bool {
    key.level < wanted && targets.iter().any(|target| lod::is_ancestor(key, *target))
}

fn graduated_leaf_count(targets: &[TileKey], wanted: u8) -> usize {
    // graduate_targets includes all unique split parents and their ancestors.
    6 + 3 * targets.iter().filter(|key| key.level < wanted).count()
}

fn bounded_footprint_level(
    direction: DVec3,
    mut level: u8,
    aspect: f64,
    altitude: f64,
    tilt: f64,
) -> u8 {
    while level > 0 {
        let raw = footprint_keys(direction, level, aspect, altitude, tilt);
        if raw.len() <= MAX_VISIBLE_TILES {
            let targets = graduate_targets(&raw, level, direction);
            if graduated_leaf_count(&targets, level) <= MAX_PLANNED_LEAVES {
                break;
            }
        }
        level -= 1;
    }
    level
}

/// Keep the original bounded overview, spending remaining leaves on source
/// resolution near the inspection centre. Periphery stays coarser and2:1.
fn inspection_targets(
    direction: DVec3,
    wanted: u8,
    mut base: u8,
    aspect: f64,
    altitude: f64,
    tilt: f64,
) -> Vec<TileKey> {
    loop {
        let raw = footprint_keys(direction, base, aspect, altitude, tilt);
        let mut parents: Vec<_> = raw.into_iter().filter_map(lod::parent).collect();
        let (face, u, v) = face_uv(direction);
        let count = 1u32 << wanted.saturating_sub(1);
        let centre = TileKey {
            face: face as u8,
            level: wanted.saturating_sub(1),
            x: (((u + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
            y: (((v + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
        };
        parents.push(centre);
        let mut targets = graduate_targets(&parents, wanted, direction);
        if graduated_leaf_count(&targets, wanted) <= MAX_PLANNED_LEAVES || base == 0 {
            let mut neighbors: Vec<_> = same_level_neighbors(centre).into_iter().collect();
            neighbors.sort_by(|a, b| {
                tile_center(*b)
                    .dot(direction)
                    .total_cmp(&tile_center(*a).dot(direction))
            });
            for neighbor in neighbors {
                parents.push(neighbor);
                let candidate = graduate_targets(&parents, wanted, direction);
                if graduated_leaf_count(&candidate, wanted) <= MAX_PLANNED_LEAVES {
                    targets = candidate;
                } else {
                    parents.pop();
                }
            }
            return targets;
        }
        base -= 1;
    }
}

fn split_preserves_grading(key: TileKey, cut: &HashSet<TileKey>) -> bool {
    cut.contains(&key)
        && tile_neighbors(key, cut)
            .iter()
            .all(|neighbor| neighbor.level >= key.level)
}

fn coarsen_preserves_grading(parent: TileKey, cut: &HashSet<TileKey>) -> bool {
    lod::children(parent).iter().all(|key| cut.contains(key))
        && transition_neighbors(parent, cut, true)
            .iter()
            .all(|neighbor| neighbor.level <= parent.level + 1)
}
fn border_neighbors(
    key: TileKey,
    index: usize,
    side: usize,
    cut: &HashSet<TileKey>,
) -> HashSet<TileKey> {
    let x = index % side;
    let y = index / side;
    let mut offsets = Vec::new();
    if x == 0 {
        offsets.push((-1e-6, 0.0));
    }
    if x == side - 1 {
        offsets.push((1e-6, 0.0));
    }
    if y == 0 {
        offsets.push((0.0, -1e-6));
    }
    if y == side - 1 {
        offsets.push((0.0, 1e-6));
    }
    if offsets.len() == 2 {
        offsets.push((offsets[0].0 + offsets[1].0, offsets[0].1 + offsets[1].1));
    }
    let count = (1u32 << key.level) as f64;
    let mut out = HashSet::new();
    for (dx, dy) in offsets {
        let u = 2.0 * (key.x as f64 + x as f64 / (side - 1) as f64 + dx) / count - 1.0;
        let v = 2.0 * (key.y as f64 + y as f64 / (side - 1) as f64 + dy) / count - 1.0;
        let (face, u, v) = face_uv(cube_direction(key.face, u, v));
        if let Some(neighbor) = lod::covering_key(cut, face as u8, u, v) {
            if neighbor != key {
                out.insert(neighbor);
            }
        }
    }
    out
}

/// Every finer leaf whose boundary changes when a parent is split or merged.
/// Probe from the fine side: a coarse edge can meet more than eight fine leaves.
fn transition_neighbors(parent: TileKey, cut: &HashSet<TileKey>, coarsening: bool) -> Vec<TileKey> {
    let children = lod::children(parent);
    let mut neighbors: Vec<_> = cut
        .iter()
        .copied()
        .filter(|key| {
            *key != parent
                && !children.contains(key)
                && (key.level > parent.level || coarsening && key.level == parent.level)
                && tile_neighbors(*key, cut).iter().any(|neighbor| {
                    if coarsening {
                        children.contains(neighbor)
                    } else {
                        *neighbor == parent
                    }
                })
        })
        .collect();
    neighbors.sort();
    neighbors
}

/// On a 2:1 cut, at most two fine leaves meet each source edge. Quarter
/// probes enumerate both; corner probes include the diagonal and cube seams.
fn graded_transition_neighbors(
    parent: TileKey,
    cut: &HashSet<TileKey>,
    coarsening: bool,
) -> Vec<TileKey> {
    let children = lod::children(parent);
    let sources = if coarsening {
        children.to_vec()
    } else {
        vec![parent]
    };
    let mut neighbors = HashSet::new();
    for source in sources {
        let count = (1u32 << source.level) as f64;
        for (x, y) in [
            (-1e-6, 0.25),
            (-1e-6, 0.75),
            (1.000001, 0.25),
            (1.000001, 0.75),
            (0.25, -1e-6),
            (0.75, -1e-6),
            (0.25, 1.000001),
            (0.75, 1.000001),
            (-1e-6, -1e-6),
            (-1e-6, 1.000001),
            (1.000001, -1e-6),
            (1.000001, 1.000001),
        ] {
            let direction = cube_direction(
                source.face,
                2.0 * (source.x as f64 + x) / count - 1.0,
                2.0 * (source.y as f64 + y) / count - 1.0,
            );
            let (face, u, v) = face_uv(direction);
            if let Some(key) = lod::covering_key(cut, face as u8, u, v) {
                if key != parent
                    && !children.contains(&key)
                    && (key.level > parent.level || coarsening && key.level == parent.level)
                {
                    neighbors.insert(key);
                }
            }
        }
    }
    let mut neighbors: Vec<_> = neighbors.into_iter().collect();
    neighbors.sort();
    neighbors
}

/// Include every source and edge job in the exclusion region of a transition.
/// Disjoint regions can share a morph epoch without rewriting a neighbor twice.
fn transition_footprint(
    parent: TileKey,
    cut: &HashSet<TileKey>,
    coarsening: bool,
    affected: &[TileKey],
) -> HashSet<TileKey> {
    let children = lod::children(parent);
    let mut keys = HashSet::from([parent]);
    keys.extend(children);
    keys.extend(affected.iter().copied());
    for source in if coarsening {
        children.to_vec()
    } else {
        vec![parent]
    } {
        keys.extend(tile_neighbors(source, cut));
    }
    keys
}

/// Request the next usable partition change, never the history of a target leaf.
fn frontier_keys(
    cut: &HashSet<TileKey>,
    targets: &[TileKey],
    level: u8,
    direction: DVec3,
    include_coarsen: bool,
) -> Vec<TileKey> {
    let mut parents: Vec<_> = cut
        .iter()
        .copied()
        .filter(|key| needs_refinement(*key, targets, level))
        .collect();
    parents.sort_by(|a, b| {
        a.level.cmp(&b.level).then_with(|| {
            tile_center(*b)
                .dot(direction)
                .total_cmp(&tile_center(*a).dot(direction))
        })
    });
    let mut out = Vec::new();
    if include_coarsen {
        let candidates: HashSet<_> = cut.iter().filter_map(|key| lod::parent(*key)).collect();
        let mut coarse: Vec<_> = candidates
            .into_iter()
            .filter(|parent| {
                !needs_refinement(*parent, targets, level)
                    && lod::children(*parent)
                        .iter()
                        .all(|child| cut.contains(child))
            })
            .collect();
        coarse.sort_by_key(|key| std::cmp::Reverse(key.level));
        out.extend(coarse);
    }
    out.extend(parents.into_iter().flat_map(lod::children));
    out
}

/// Warm one more frontier while the immediate quartets are being received.
/// Projection uses complete requested families, never mutates the displayed cut,
/// and runs only once even when the forecast target is many levels deeper.
fn prefetch_keys(
    cut: &HashSet<TileKey>,
    targets: &[TileKey],
    level: u8,
    direction: DVec3,
) -> Vec<TileKey> {
    let immediate = frontier_keys(cut, targets, level, direction, false);
    let mut projected = cut.clone();
    for quartet in immediate.chunks_exact(4) {
        let parent = lod::parent(quartet[0]).expect("frontier children have a parent");
        if quartet == lod::children(parent) && split_preserves_grading(parent, &projected) {
            lod::split(&mut projected, parent);
        }
    }
    let following = frontier_keys(&projected, targets, level, direction, false);
    let (face, u, v) = face_uv(direction);
    let centers = [
        lod::covering_key(cut, face as u8, u, v),
        lod::covering_key(&projected, face as u8, u, v),
    ];
    let mut out = Vec::with_capacity(immediate.len() + following.len());
    let mut seen = HashSet::new();
    // Only speculative ordering changes: keep the central dependency and then
    // its next quartet before forecast fringes can consume the bounded budget.
    for center in centers.into_iter().flatten() {
        if let Some(quartet) = immediate
            .chunks_exact(4)
            .chain(following.chunks_exact(4))
            .find(|quartet| lod::parent(quartet[0]) == Some(center))
        {
            out.extend(quartet.iter().copied().filter(|key| seen.insert(*key)));
        }
    }
    out.extend(
        immediate
            .into_iter()
            .chain(following)
            .filter(|key| seen.insert(*key)),
    );
    out
}

/// Deduplicate before budgeting so the existing real frontier stays intact and
/// every admitted forecast quartet is complete across the combined request list.
fn append_prefetch(keys: &mut Vec<TileKey>, future: &[TileKey]) {
    let mut seen: HashSet<_> = keys.iter().copied().collect();
    let mut remaining = MAX_PREFETCH_TILES;
    for quartet in future.chunks_exact(4) {
        let missing = quartet.iter().filter(|key| !seen.contains(key)).count();
        if missing > remaining {
            continue;
        }
        remaining -= missing;
        keys.extend(quartet.iter().copied().filter(|key| seen.insert(*key)));
    }
}

/// Under pressure, only the reserved quartet can survive until its siblings
/// arrive. Fetching later quartets would immediately evict and request them again.
fn pressure_frontier(
    keys: Vec<TileKey>,
    cut: &HashSet<TileKey>,
    staged: Option<TileKey>,
) -> Vec<TileKey> {
    keys.into_iter()
        .filter(|key| {
            staged.is_some_and(|parent| lod::parent(*key) == Some(parent))
                || lod::children(*key).iter().all(|child| cut.contains(child))
        })
        .collect()
}

fn root_tiles(world: &World) -> Result<Vec<Tile>, String> {
    let source = world.mesh_resolution();
    if source < 2 || source > 128 {
        return Err("Résolution visuelle invalide".into());
    }
    let exact = world.render_grid.as_deref() == Some("vertices");
    let n = source;
    let size = (n + 1) * (n + 1);
    if world.cells.len() != 6 * if exact { size } else { n * n } {
        return Err("Atlas visuel invalide".into());
    }
    let mut roots = Vec::new();
    for face in 0..6 {
        let mut vertices = Vec::with_capacity(size);
        let mut indices = Vec::with_capacity(n * n * 6);
        for y in 0..=n {
            for x in 0..=n {
                if exact {
                    vertices.push(world.cells[face * size + y * (n + 1) + x].clone());
                } else {
                    let key = cube_key(face, x, y, n);
                    let direction =
                        DVec3::new(key[0] as f64, key[1] as f64, key[2] as f64).normalize();
                    vertices.push(atlas_sample(world, direction));
                }
            }
        }
        for y in 0..n {
            for x in 0..n {
                let a = (y * (n + 1) + x) as u32;
                let b = a + 1;
                let c = a + (n + 1) as u32;
                let d = c + 1;
                indices.extend([a, b, d, a, d, c]);
            }
        }
        roots.push(Tile {
            key: TileKey {
                face: face as u8,
                ..Default::default()
            },
            vertices,
            indices,
            ..Default::default()
        });
    }
    Ok(roots)
}
/// Orbital geometry and streamed roots use the exact same samples and diagonals.
fn world_mesh(world: &World, exaggeration: f64) -> Result<Mesh, String> {
    let roots = root_tiles(world)?;
    let mut samples = Vec::new();
    let mut indices = Vec::new();
    for root in roots {
        let start = samples.len() as u32;
        samples.extend(root.vertices);
        indices.extend(root.indices.iter().map(|i| i + start));
    }
    let mut mesh = mesh_from_samples_inner(
        &samples,
        &indices,
        0,
        None,
        world.config.radius_m,
        exaggeration,
        true,
    )?;
    smooth_mesh_normals(std::slice::from_mut(&mut mesh));
    Ok(mesh)
}
fn append_ocean_shell(
    mesh: &mut Mesh,
    samples: &[Sample],
    indices: &[u32],
    tag: u8,
    radius: f64,
    exaggeration: f64,
) {
    let start = mesh.vertices.len() as u32;
    for sample in samples {
        let mut ocean = sample.clone();
        ocean.ocean = true;
        ocean.water_level_m = Some(0.0);
        ocean.water_body_id = Some(0);
        ocean.water_m = 0.0;
        let mut v = vertex(&ocean, radius, exaggeration, true);
        v.color[3] += 2.0 * tag as f32;
        let depth = if water_body(sample).is_some_and(|(id, _)| id == 0) {
            -sample.height_m
        } else {
            -sample.height_m.abs() - 0.01
        };
        v.fields[2] = -1.0;
        v.fields[3] = depth as f32;
        v.morph[2] = depth as f32;
        v.morph[3] = depth as f32;
        mesh.vertices.push(v);
    }
    mesh.indices.extend(indices.iter().map(|i| i + start));
}

fn mesh_from_samples(
    samples: &[Sample],
    indices: &[u32],
    tag: u8,
    morph: Option<(&[[f64; 3]], f64)>,
    radius: f64,
    exaggeration: f64,
) -> Result<Mesh, String> {
    mesh_from_samples_inner(samples, indices, tag, morph, radius, exaggeration, false)
}

fn mesh_from_samples_inner(
    samples: &[Sample],
    indices: &[u32],
    tag: u8,
    morph: Option<(&[[f64; 3]], f64)>,
    radius: f64,
    exaggeration: f64,
    ocean_shell: bool,
) -> Result<Mesh, String> {
    if samples.is_empty() || indices.iter().any(|&i| i as usize >= samples.len()) {
        return Err("Indices de tuile invalides".into());
    }
    let mut mesh = Mesh {
        indices: indices.to_vec(),
        ..Default::default()
    };
    for water in [false, true] {
        mesh.vertices
            .extend(samples.iter().enumerate().map(|(index, s)| {
                let mut v = vertex(s, radius, exaggeration, water);
                v.fields[2] = if water {
                    water_style(s)
                } else {
                    s.river as f32
                };
                v.color[3] += 2.0 * f32::from(tag);
                if let Some((offsets, start)) = morph {
                    if let Some(offset) = offsets.get(index) {
                        v.morph = [
                            if water { 0.0 } else { offset[0] as f32 },
                            start as f32,
                            if water {
                                v.fields[3] - (offset[0] * radius / exaggeration) as f32
                            } else {
                                offset[2] as f32
                            },
                            0.0,
                        ];
                    }
                }
                v
            }));
    }
    append_water_geometry(
        &mut mesh,
        samples,
        indices,
        radius,
        exaggeration,
        ocean_shell,
    );
    if ocean_shell {
        append_ocean_shell(&mut mesh, samples, indices, tag, radius, exaggeration);
    }
    local_mesh_normals(&mut mesh);
    Ok(mesh)
}

fn tile_mesh(tile: &Tile, radius: f64, exaggeration: f64) -> Result<Mesh, String> {
    mesh_from_samples_inner(
        &tile.vertices,
        &tile.indices,
        tile.key.level + 1,
        Some((&tile.morph_offsets, tile.morph_start)),
        radius,
        exaggeration,
        true,
    )
}

fn face_uv(direction: DVec3) -> (usize, f64, f64) {
    let mut best = (0, f64::NEG_INFINITY);
    for (face, basis) in BASES.iter().enumerate() {
        let n = DVec3::new(basis[0][0] as f64, basis[0][1] as f64, basis[0][2] as f64);
        let dot = direction.dot(n);
        if dot > best.1 {
            best = (face, dot);
        }
    }
    let b = BASES[best.0];
    let vector = |v: [i32; 3]| DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64);
    (
        best.0,
        direction.dot(vector(b[1])) / best.1,
        direction.dot(vector(b[2])) / best.1,
    )
}

fn morph_progress(time: f64, start: f64) -> f64 {
    let t = ((time - start) / MORPH_DURATION_SECONDS).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[derive(Clone, Copy, Default)]
struct MorphEpoch {
    id: u32,
    start: f32,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DynamicVertex {
    morph: [f32; 4],
    shading_normal: [f32; 3],
    parent_normal: [f32; 3],
    water_depth: f32,
}
impl DynamicVertex {
    fn settled(normal: [f32; 3], depth: f32) -> Self {
        Self {
            morph: [0.0, 0.0, depth, 0.0],
            shading_normal: normal,
            parent_normal: normal,
            water_depth: depth,
        }
    }
}
fn next_morph_epoch(last: u32, time: f64, epochs: &[MorphEpoch; 32]) -> Option<u32> {
    let id = last.wrapping_add(1).max(1);
    let previous = epochs[id as usize % 32];
    (time.is_finite()
        && (previous.id == 0
            || previous.start.is_finite()
                && time >= f64::from(previous.start) + MORPH_DURATION_SECONDS))
        .then_some(id)
}
fn epoch_progress(raw: f32, time: f64, epochs: &[MorphEpoch; 32]) -> f64 {
    let start = if raw < 0.0 {
        let id = (-raw) as u32;
        let epoch = epochs[id as usize % 32];
        if epoch.id == id {
            epoch.start as f64
        } else {
            -1e9
        }
    } else {
        raw as f64
    };
    morph_progress(time, start)
}

fn camera_level(altitude: f64) -> u8 {
    let level =
        ((1.0 / altitude).log2().floor() as i32 + 3).clamp(0, i32::from(MAX_TILE_LEVEL)) as u8;
    if level > 2 {
        level.saturating_add(1).min(MAX_TILE_LEVEL)
    } else {
        level
    }
}

/// Close inspection uses a light at 45° to the local surface, including the
/// planetary night side. Only rendering changes; climate keeps its own forcing.
fn inspection_light(direction: DVec3, altitude: f64) -> Vec3 {
    let radial = direction.normalize();
    let axis = if radial.y.abs() < 0.95 {
        DVec3::Y
    } else {
        DVec3::Z
    };
    let north = (axis - radial * axis.dot(radial)).normalize();
    let east = north.cross(radial).normalize();
    let tangent = north * 0.8 + east * 0.6;
    let local = (radial + tangent).normalize();
    let orbital = DVec3::new(-0.45, 0.80, 0.55).normalize();
    let t = ((altitude - 0.02) / 0.18).clamp(0.0, 1.0);
    let weight = t * t * (3.0 - 2.0 * t);
    let blend = local.lerp(orbital, weight);
    if blend.length_squared() > 1e-12 {
        blend.normalize().as_vec3()
    } else {
        local.as_vec3()
    }
}

fn smooth_mesh_normals(meshes: &mut [Mesh]) {
    let position = |vertex: &Vertex, parent: bool| {
        let point = DVec3::from_array(vertex.position.map(f64::from))
            + DVec3::from_array(vertex.position_low.map(f64::from));
        if parent {
            point
                + DVec3::new(
                    vertex.normal_height[0] as f64,
                    vertex.normal_height[1] as f64,
                    vertex.normal_height[2] as f64,
                ) * vertex.morph[0] as f64
        } else {
            point
        }
    };
    let key = |vertex: &Vertex| {
        let d = position(vertex, false).normalize();
        (
            (vertex.color[3].round() as u32 & 1) != 0,
            [
                (d.x * 1e12).round() as i64,
                (d.y * 1e12).round() as i64,
                (d.z * 1e12).round() as i64,
            ],
        )
    };
    for parent in [false, true] {
        let mut shared: HashMap<(bool, [i64; 3]), DVec3> = HashMap::new();
        for mesh in meshes.iter() {
            for triangle in mesh.indices.chunks_exact(3) {
                if mesh.vertices[triangle[0] as usize].color[3].round() as u32 & 1 != 0 {
                    continue;
                }
                let a = position(&mesh.vertices[triangle[0] as usize], parent);
                let b = position(&mesh.vertices[triangle[1] as usize], parent);
                let c = position(&mesh.vertices[triangle[2] as usize], parent);
                let mut normal = (b - a).cross(c - a);
                if normal.dot(a + b + c) < 0.0 {
                    normal = -normal;
                }
                for &index in triangle {
                    *shared
                        .entry(key(&mesh.vertices[index as usize]))
                        .or_insert(DVec3::ZERO) += normal;
                }
            }
        }
        for mesh in meshes.iter_mut() {
            for vertex in &mut mesh.vertices {
                if vertex.color[3].round() as u32 & 1 != 0 {
                    let normal = [
                        vertex.normal_height[0],
                        vertex.normal_height[1],
                        vertex.normal_height[2],
                    ];
                    vertex.shading_normal = normal;
                    vertex.parent_normal = normal;
                    continue;
                }
                let normal = shared.get(&key(vertex)).copied().unwrap_or(DVec3::ZERO);
                let normal = if normal.length_squared() > 1e-60 {
                    normal.normalize().as_vec3().to_array()
                } else {
                    [
                        vertex.normal_height[0],
                        vertex.normal_height[1],
                        vertex.normal_height[2],
                    ]
                };
                if parent {
                    vertex.parent_normal = normal;
                } else {
                    vertex.shading_normal = normal;
                }
            }
        }
    }
}

fn ray_triangle(direction: DVec3, a: DVec3, b: DVec3, c: DVec3) -> Option<f64> {
    let e1 = b - a;
    let e2 = c - a;
    let h = direction.cross(e2);
    let determinant = e1.dot(h);
    if determinant.abs() < 1e-24 {
        return None;
    }
    let inverse = determinant.recip();
    let s = -a;
    let u = inverse * s.dot(h);
    let q = s.cross(e1);
    let v = inverse * direction.dot(q);
    if u < -1e-7 || v < -1e-7 || u + v > 1.0000001 {
        return None;
    }
    let t = inverse * e2.dot(q);
    (t > 0.0).then_some(t)
}

fn ray_triangle_detail(direction: DVec3, a: DVec3, b: DVec3, c: DVec3) -> Option<(f64, f64, f64)> {
    let e1 = b - a;
    let e2 = c - a;
    let h = direction.cross(e2);
    let determinant = e1.dot(h);
    if determinant.abs() < 1e-24 {
        return None;
    }
    let inverse = determinant.recip();
    let q = (-a).cross(e1);
    let u = inverse * (-a).dot(h);
    let v = inverse * direction.dot(q);
    if u < -1e-7 || v < -1e-7 || u + v > 1.0000001 {
        return None;
    }
    let t = inverse * e2.dot(q);
    (t > 0.0).then_some((t, u, v))
}
fn surface_query(
    direction: DVec3,
    key: TileKey,
    side: usize,
    at: impl Fn(usize) -> (DVec3, [f32; 3]),
) -> Option<(f64, [f32; 3])> {
    if side < 2 {
        return None;
    }
    let b = BASES[key.face as usize];
    let vec = |v: [i32; 3]| DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64);
    let d = direction.dot(vec(b[0]));
    if d <= 0.0 {
        return None;
    }
    let count = f64::from(1u32 << key.level);
    let x = (direction.dot(vec(b[1])) / d + 1.0) * 0.5 * count - key.x as f64;
    let y = (direction.dot(vec(b[2])) / d + 1.0) * 0.5 * count - key.y as f64;
    if !(-1e-9..=1.000000001).contains(&x) || !(-1e-9..=1.000000001).contains(&y) {
        return None;
    }
    let i = ((x * (side - 1) as f64).floor() as usize).min(side - 2);
    let j = ((y * (side - 1) as f64).floor() as usize).min(side - 2);
    let a = j * side + i;
    let b = a + 1;
    let c = a + side;
    let d = c + 1;
    for indices in [[a, b, d], [a, d, c]] {
        let [(pa, na), (pb, nb), (pc, nc)] = indices.map(&at);
        if let Some((radius, u, v)) = ray_triangle_detail(direction, pa, pb, pc) {
            let normal = Vec3::from_array(na) * (1.0 - u - v) as f32
                + Vec3::from_array(nb) * u as f32
                + Vec3::from_array(nc) * v as f32;
            return Some((radius, normal.normalize_or_zero().to_array()));
        }
    }
    None
}

fn surface_scalar(
    direction: DVec3,
    key: TileKey,
    side: usize,
    at: impl Fn(usize) -> (DVec3, f64),
) -> Option<(f64, f64)> {
    if side < 2 {
        return None;
    }
    let b = BASES[key.face as usize];
    let vec = |v: [i32; 3]| DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64);
    let d = direction.dot(vec(b[0]));
    if d <= 0.0 {
        return None;
    }
    let count = (1u32 << key.level) as f64;
    let x = (direction.dot(vec(b[1])) / d + 1.0) * 0.5 * count - key.x as f64;
    let y = (direction.dot(vec(b[2])) / d + 1.0) * 0.5 * count - key.y as f64;
    if !(-1e-9..=1.000000001).contains(&x) || !(-1e-9..=1.000000001).contains(&y) {
        return None;
    }
    let i = ((x * (side - 1) as f64).floor() as usize).min(side - 2);
    let j = ((y * (side - 1) as f64).floor() as usize).min(side - 2);
    let a = j * side + i;
    let b = a + 1;
    let c = a + side;
    let d = c + 1;
    for indices in [[a, b, d], [a, d, c]] {
        let [(pa, va), (pb, vb), (pc, vc)] = indices.map(&at);
        if let Some((r, u, v)) = ray_triangle_detail(direction, pa, pb, pc) {
            return Some((r, va * (1.0 - u - v) + vb * u + vc * v));
        }
    }
    None
}

/// Nested even grids put every added vertex on one parent edge or diagonal.
/// Intersect the radial ray with that actual 3D chord, rather than averaging heights.
fn nested_parent_source(
    child: TileKey,
    index: usize,
    side: usize,
    direction: DVec3,
    at: impl Fn(usize) -> (DVec3, [f32; 3], f64),
) -> Option<(f64, [f32; 3], f64)> {
    if side < 3 || (side - 1) % 2 != 0 || index >= side * side {
        return None;
    }
    let x = index % side;
    let y = index / side;
    let i = (child.x as usize % 2) * ((side - 1) / 2) + x / 2;
    let j = (child.y as usize % 2) * ((side - 1) / 2) + y / 2;
    let a = j * side + i;
    let b = a + x % 2 + (y % 2) * side;
    let (pa, na, da) = at(a);
    if a == b {
        return Some((pa.length(), na, da));
    }
    let (pb, nb, db) = at(b);
    let edge = pb - pa;
    let plane = edge.cross(pa.cross(pb));
    let denominator = plane.dot(direction);
    if denominator.abs() < 1e-30 {
        return None;
    }
    let radius = plane.dot(pa) / denominator;
    let t = (direction * radius - pa).dot(edge) / edge.length_squared();
    if !radius.is_finite() || !(-1e-5..=1.00001).contains(&t) {
        return None;
    }
    let normal = Vec3::from_array(na)
        .lerp(Vec3::from_array(nb), t as f32)
        .normalize_or_zero();
    Some((radius, normal.to_array(), da * (1.0 - t) + db * t))
}

fn sphere_in_frustum(
    center: DVec3,
    radius: f64,
    frame: (DVec3, DVec3, DVec3, DVec3),
    aspect: f64,
) -> bool {
    let (eye, forward, right, up) = frame;
    let p = center - eye;
    let tangent = 22.5f64.to_radians().tan();
    p.dot(forward) >= -radius
        && [
            forward * tangent * aspect + right,
            forward * tangent * aspect - right,
            forward * tangent + up,
            forward * tangent - up,
        ]
        .iter()
        .all(|n| p.dot(*n) >= -radius * n.length())
}

fn projected_diameter_pixels(
    center: DVec3,
    radius: f64,
    frame: (DVec3, DVec3, DVec3, DVec3),
    height: u32,
) -> f64 {
    let (eye, forward, right, up) = frame;
    let p = center - eye;
    let depth = p.dot(forward);
    if depth <= radius {
        return f64::INFINITY;
    }
    let lateral = (p.dot(right) / depth).powi(2) + (p.dot(up) / depth).powi(2);
    let focal = height as f64 / (2.0 * 22.5f64.to_radians().tan());
    // The numerator bounds the x/z,y/z variation, including off-axis depth
    // motion. depth-radius bounds the denominator of every sphere point.
    2.0 * focal * radius * (1.0 + lateral).sqrt() / (depth - radius)
}

fn instant_coarsen_bounds(
    bounds: &[(DVec3, f64)],
    frame: (DVec3, DVec3, DVec3, DVec3),
    aspect: f64,
    height: u32,
) -> bool {
    if ![frame.0, frame.1, frame.2, frame.3]
        .iter()
        .all(|v| v.is_finite())
        || !aspect.is_finite()
        || aspect <= 0.0
        || height == 0
        || bounds.is_empty()
        || bounds
            .iter()
            .any(|(center, radius)| !center.is_finite() || !radius.is_finite() || *radius < 0.0)
    {
        return false;
    }
    if bounds
        .iter()
        .all(|(center, radius)| !sphere_in_frustum(*center, *radius, frame, aspect))
    {
        return true;
    }
    let (mut low, mut high) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for (center, radius) in bounds {
        low = low.min(*center - DVec3::splat(*radius));
        high = high.max(*center + DVec3::splat(*radius));
    }
    projected_diameter_pixels(
        (low + high) * 0.5,
        (high - low).length() * 0.5,
        frame,
        height,
    ) <= 0.5
}

fn touches_camera_support(
    keys: &[TileKey],
    cut: &HashSet<TileKey>,
    direction: DVec3,
    eye: DVec3,
) -> bool {
    [direction, eye.normalize()].into_iter().any(|direction| {
        let (face, u, v) = face_uv(direction);
        lod::covering_key(cut, face as u8, u, v)
            .is_none_or(|support| keys.iter().any(|key| lod::is_ancestor(*key, support)))
    })
}

fn normal_key(direction: DVec3) -> [i64; 3] {
    direction
        .normalize()
        .to_array()
        .map(|v| (v * 1e12).round() as i64)
}

fn surface_radius(
    direction: DVec3,
    key: TileKey,
    side: usize,
    at: impl Fn(usize) -> DVec3,
) -> Option<f64> {
    if side < 2 {
        return None;
    }
    let basis = BASES[usize::from(key.face)];
    let vec = |a: [i32; 3]| DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
    let denominator = direction.dot(vec(basis[0]));
    if denominator <= 0.0 {
        return None;
    }
    let u = direction.dot(vec(basis[1])) / denominator;
    let v = direction.dot(vec(basis[2])) / denominator;
    let count = f64::from(1u32 << key.level);
    let x = (u + 1.0) * 0.5 * count - f64::from(key.x);
    let y = (v + 1.0) * 0.5 * count - f64::from(key.y);
    if x < -1e-9 || y < -1e-9 || x > 1.000000001 || y > 1.000000001 {
        return None;
    }
    let i = ((x * (side - 1) as f64).floor() as usize).min(side - 2);
    let j = ((y * (side - 1) as f64).floor() as usize).min(side - 2);
    let a = at(j * side + i);
    let b = at(j * side + i + 1);
    let c = at((j + 1) * side + i);
    let d = at((j + 1) * side + i + 1);
    ray_triangle(direction, a, b, d).or_else(|| ray_triangle(direction, a, d, c))
}

fn mesh_surfaces(mesh: &Mesh, resolution: usize) -> [Vec<DVec3>; 2] {
    let half = 6 * (resolution.min(128) + 1).pow(2);
    std::array::from_fn(|water| {
        mesh.vertices[water * half..(water + 1) * half]
            .iter()
            .map(|vertex| {
                DVec3::from_array(vertex.position.map(f64::from))
                    + DVec3::from_array(vertex.position_low.map(f64::from))
            })
            .collect()
    })
}

fn camera_axes(forward: DVec3) -> (DVec3, DVec3) {
    let axis = if forward.cross(DVec3::Y).length_squared() < 1e-12 {
        DVec3::Z
    } else {
        DVec3::Y
    };
    let right = forward.cross(axis).normalize();
    (right, right.cross(forward).normalize())
}

fn camera_frame(
    direction: DVec3,
    altitude: f64,
    tilt: f64,
    radius: f64,
) -> (DVec3, DVec3, DVec3, DVec3) {
    let north = (DVec3::Y - direction * direction.y).normalize_or_zero();
    let eye = direction * (radius + altitude) - north * altitude * tilt.tan();
    let forward = (direction * radius - eye).normalize();
    let (right, up) = camera_axes(forward);
    (eye, forward, right, up)
}

/// Project the viewport on the local sphere, then include a one-tile guard band.
/// Sampling intervals are smaller than a tile; the guard also covers seam slivers
/// and moderate elevation differences, without requesting a large unused square.
fn footprint_keys(
    direction: DVec3,
    level: u8,
    aspect: f64,
    altitude: f64,
    tilt: f64,
) -> Vec<TileKey> {
    let count = 1u32 << level;
    let (eye, forward, right, up) = camera_frame(direction, altitude, tilt, 1.0);
    let tangent = 22.5f64.to_radians().tan();
    let mut hits = HashSet::new();
    let key_at = |p: DVec3| {
        let (face, u, v) = face_uv(p);
        TileKey {
            face: face as u8,
            level,
            x: (((u + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
            y: (((v + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
        }
    };
    for y in 0..=12 {
        for x in 0..=12 {
            let ray = (forward
                + right * ((x as f64 / 6.0 - 1.0) * tangent * aspect)
                + up * ((y as f64 / 6.0 - 1.0) * tangent))
                .normalize();
            let b = eye.dot(ray);
            let discriminant = b * b - eye.length_squared() + 1.0;
            if discriminant < 0.0 {
                continue;
            }
            let t = -b - discriminant.sqrt();
            if t > 0.0 {
                hits.insert(key_at((eye + ray * t).normalize()));
            }
        }
    }
    hits.insert(key_at(direction));
    let mut guarded = hits.clone();
    for key in hits {
        let b = BASES[key.face as usize];
        let vec = |a: [i32; 3]| DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
        for dy in -1..=1 {
            for dx in -1..=1 {
                let u = 2.0 * (key.x as f64 + dx as f64 + 0.5) / f64::from(count) - 1.0;
                let v = 2.0 * (key.y as f64 + dy as f64 + 0.5) / f64::from(count) - 1.0;
                guarded.insert(key_at(
                    (vec(b[0]) + vec(b[1]) * u + vec(b[2]) * v).normalize(),
                ));
            }
        }
    }
    let mut keys: Vec<_> = guarded.into_iter().collect();
    let distance = |key: &TileKey| {
        let b = BASES[key.face as usize];
        let vec = |a: [i32; 3]| DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
        let u = 2.0 * (key.x as f64 + 0.5) / f64::from(count) - 1.0;
        let v = 2.0 * (key.y as f64 + 0.5) / f64::from(count) - 1.0;
        -direction.dot((vec(b[0]) + vec(b[1]) * u + vec(b[2]) * v).normalize())
    };
    keys.sort_by(|a, b| distance(a).total_cmp(&distance(b)).then(a.cmp(b)));
    keys
}

fn footprint_level(direction: DVec3, altitude: f64, aspect: f64, tilt: f64) -> u8 {
    bounded_footprint_level(direction, camera_level(altitude), aspect, altitude, tilt)
}

fn append_ribbon(
    mesh: &mut Mesh,
    start: [f64; 3],
    end: [f64; 3],
    heights: [f64; 2],
    width: f64,
    radius: f64,
    exaggeration: f64,
    color: [f32; 3],
    tag: f32,
) {
    let start = DVec3::from_array(start).normalize();
    let end = DVec3::from_array(end).normalize();
    let tangent = end - start;
    if tangent.length_squared() < 1e-20 {
        return;
    }
    // River descriptors already contain curved, resampled chords. Subdividing
    // every short chord eight more times multiplies orbital geometry needlessly.
    let steps = (start.angle_between(end) * radius / 2000.0)
        .ceil()
        .clamp(1.0, 8.0) as u32;
    let first = mesh.vertices.len() as u32;
    for i in 0..=steps {
        let t = f64::from(i) / f64::from(steps);
        let direction = start.lerp(end, t).normalize();
        let side = direction.cross(tangent).normalize() * (width * 0.5 / radius);
        let sample = Sample {
            direction: direction.to_array(),
            height_m: heights[0] * (1.0 - t) + heights[1] * t + 1.0,
            ..Default::default()
        };
        for sign in [-1.0, 1.0] {
            let mut v = vertex(&sample, radius, exaggeration, false);
            let p = DVec3::from_array(v.position.map(f64::from))
                + DVec3::from_array(v.position_low.map(f64::from))
                + sign * side;
            let high = p.as_vec3();
            v.position = high.to_array();
            v.position_low = (p - high.as_dvec3()).as_vec3().to_array();
            v.color = [color[0], color[1], color[2], tag];
            mesh.vertices.push(v);
        }
        if i < steps {
            let a = first + i * 2;
            mesh.indices.extend([a, a + 1, a + 2, a + 1, a + 3, a + 2]);
        }
    }
}

fn object_mesh(rivers: &RiverLayer, human: &HumanLayer, radius: f64, exaggeration: f64) -> Mesh {
    let mut mesh = Mesh::default();
    // This layer is an orbital locator, not the fine physical water mesh.
    // Bound untrusted/legacy responses; local channels remain in terrain tiles.
    let mut selected: Vec<_> = rivers.segments.iter().collect();
    const ORBITAL_RIVER_CHORDS: usize = 32_768;
    if selected.len() > ORBITAL_RIVER_CHORDS {
        selected
            .select_nth_unstable_by(ORBITAL_RIVER_CHORDS, |a, b| b.width_m.total_cmp(&a.width_m));
        selected.truncate(ORBITAL_RIVER_CHORDS);
    }
    for river in selected {
        append_ribbon(
            &mut mesh,
            river.start,
            river.end,
            [river.start_height_m, river.end_height_m],
            river.width_m.max(3.0),
            radius,
            exaggeration,
            [0.07, 0.39, 0.51],
            100.0,
        );
    }
    for road in &human.roads {
        for i in 1..road.points.len() {
            append_ribbon(
                &mut mesh,
                road.points[i - 1],
                road.points[i],
                [
                    road.altitudes_m.get(i - 1).copied().unwrap_or(0.0),
                    road.altitudes_m.get(i).copied().unwrap_or(0.0),
                ],
                8.0,
                radius,
                exaggeration,
                [0.86, 0.62, 0.30],
                102.0,
            );
        }
    }
    for town in &human.settlements {
        let direction = DVec3::from_array(town.direction).normalize();
        let axis = if direction.y.abs() > 0.9 {
            DVec3::X
        } else {
            DVec3::Y
        };
        let u = direction.cross(axis).normalize();
        let v = direction.cross(u).normalize();
        let width = (town.population as f64).sqrt().clamp(15.0, 200.0) / radius;
        let base = direction * (1.0 + (town.altitude_m + 2.0) * exaggeration / radius);
        let first = mesh.vertices.len() as u32;
        for offset in [-u - v, u - v, u + v, -u + v, direction * 2.5] {
            let p = base + offset * width;
            let sample = Sample {
                direction: p.normalize().to_array(),
                height_m: (p.length() - 1.0) * radius / exaggeration,
                ..Default::default()
            };
            let mut vert = vertex(&sample, radius, exaggeration, false);
            vert.color = [0.94, 0.74, 0.42, 104.0];
            mesh.vertices.push(vert);
        }
        for i in 0..4 {
            mesh.indices
                .extend([first + i, first + (i + 1) % 4, first + 4]);
        }
    }
    mesh
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::*;
    use glam::Mat4;
    use std::cell::RefCell;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use wgpu::util::DeviceExt;

    thread_local! {
        static RENDERER: RefCell<Option<Renderer>> = const { RefCell::new(None) };
        // A worker has its own WASM instance. Parsing a field once avoids
        // repeatedly decoding megabytes of residuals for each procedural tile.
        static WORKER_FIELD: RefCell<Option<std::rc::Rc<world_core::hybrid::HybridField>>> = const { RefCell::new(None) };
        static WORKER_SNAPSHOTS: RefCell<HybridSnapshots> = RefCell::new(HybridSnapshots::default());
        static VIEW_SNAPSHOT: RefCell<String> = RefCell::new("{}".into());
        static WORKER_WORLD: RefCell<Option<World>> = const { RefCell::new(None) };
        static FIELD_PATCHES: RefCell<HashMap<String, world_core::hybrid::HybridPatch>> = RefCell::new(HashMap::new());
    }

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct Uniforms {
        view_projection: [[f32; 4]; 4],
        eye: [f32; 4],
        eye_low: [f32; 4],
        light: [f32; 4],
        options: [f32; 4],
        cartography: [u32; 4],
        surface: [f32; 4],
        noise: NoiseFrame,
        clips: [TileClip; 32],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct TileClip {
        bounds: [f32; 4],
        face_level: [f32; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct StaticVertex {
        position: [f32; 3],
        position_low: [f32; 3],
        normal_height: [f32; 4],
        color: [f32; 4],
        fields: [f32; 4],
        biome_a: [f32; 4],
        biome_b: [f32; 4],
        direction_low: [f32; 3],
    }
    #[derive(Clone, Copy)]
    struct SurfaceVertex {
        direction: DVec3,
        radius: f64,
        depth: f32,
        water: bool,
        ocean: bool,
        normal: [f32; 3],
    }
    struct GpuMesh {
        vertices: wgpu::Buffer,
        dynamic_buffer: wgpu::Buffer,
        indices: wgpu::Buffer,
        dynamic: Vec<DynamicVertex>,
        geometry: Vec<SurfaceVertex>,
        count: u32,
        bytes: usize,
        ocean_start: Option<usize>,
        bound_center: DVec3,
        bound_radius: f64,
        morph_pad: f64,
    }
    impl Drop for GpuMesh {
        fn drop(&mut self) {
            // wgpu's web handle Drop leaves GPU allocation to JavaScript GC.
            // Explicit destruction makes eviction release its GPU resources.
            self.vertices.destroy();
            self.dynamic_buffer.destroy();
            self.indices.destroy();
        }
    }

    impl GpuMesh {
        fn upload(device: &wgpu::Device, queue: &wgpu::Queue, mesh: &Mesh) -> Self {
            let dynamic = mesh.vertices.iter().map(dynamic_vertex).collect();
            Self::upload_dynamic(device, queue, mesh, dynamic)
        }
        fn upload_dynamic(
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            mesh: &Mesh,
            dynamic: Vec<DynamicVertex>,
        ) -> Self {
            Self::upload_with_geometry(device, queue, mesh, dynamic, None)
        }
        fn upload_with_geometry(
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            mesh: &Mesh,
            dynamic: Vec<DynamicVertex>,
            geometry: Option<Vec<SurfaceVertex>>,
        ) -> Self {
            let static_vertices: Vec<_> = mesh
                .vertices
                .iter()
                .map(|v| StaticVertex {
                    position: v.position,
                    position_low: v.position_low,
                    normal_height: v.normal_height,
                    color: v.color,
                    fields: v.fields,
                    biome_a: v.biome_a,
                    biome_b: v.biome_b,
                    direction_low: v.direction_low,
                })
                .collect();
            let vbytes = static_vertices.len() * std::mem::size_of::<StaticVertex>();
            let ibytes = mesh.indices.len() * 4;
            let dbytes = dynamic.len() * std::mem::size_of::<DynamicVertex>();
            let buffer = |label: &'static str, size: usize, usage| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: size.max(4) as u64,
                    usage: usage | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            };
            let vertices = buffer("terrain vertices", vbytes, wgpu::BufferUsages::VERTEX);
            let indices = buffer("terrain indices", ibytes, wgpu::BufferUsages::INDEX);
            let dynamic_buffer = buffer(
                "terrain morph and normals",
                dbytes,
                wgpu::BufferUsages::VERTEX,
            );
            if vbytes > 0 {
                queue.write_buffer(&vertices, 0, bytemuck::cast_slice(&static_vertices));
            }
            if ibytes > 0 {
                queue.write_buffer(&indices, 0, bytemuck::cast_slice(&mesh.indices));
            }
            if dbytes > 0 {
                queue.write_buffer(&dynamic_buffer, 0, bytemuck::cast_slice(&dynamic));
            }
            let geometry =
                geometry.unwrap_or_else(|| mesh.vertices.iter().map(surface_vertex).collect());
            let ocean_start = geometry.iter().position(|g| g.ocean);
            let (bound_center, bound_radius) = mesh_bounds(mesh);
            let morph_pad = dynamic
                .iter()
                .map(|d| d.morph[0].abs().max(d.morph[3].abs()) as f64)
                .fold(0.0, f64::max);
            Self {
                vertices,
                dynamic_buffer,
                indices,
                dynamic,
                geometry,
                count: mesh.indices.len() as u32,
                bytes: vbytes + ibytes + dbytes,
                ocean_start,
                bound_center,
                bound_radius,
                morph_pad,
            }
        }
        fn update_dynamic(&mut self, queue: &wgpu::Queue, dynamic: Vec<DynamicVertex>) {
            queue.write_buffer(&self.dynamic_buffer, 0, bytemuck::cast_slice(&dynamic));
            self.morph_pad = dynamic
                .iter()
                .map(|d| d.morph[0].abs().max(d.morph[3].abs()) as f64)
                .fold(0.0, f64::max);
            self.dynamic = dynamic;
        }
        fn update_edges(
            &mut self,
            queue: &wgpu::Queue,
            indices: &[usize],
            dynamic: Vec<DynamicVertex>,
        ) {
            for (&index, vertex) in indices.iter().zip(dynamic) {
                self.morph_pad = self
                    .morph_pad
                    .max(f64::from(vertex.morph[0].abs().max(vertex.morph[3].abs())));
                self.dynamic[index] = vertex;
            }
            // One upload avoids hundreds of strided WebGPU calls along the two
            // vertical edges, while CPU work retains only the changed vertices.
            queue.write_buffer(&self.dynamic_buffer, 0, bytemuck::cast_slice(&self.dynamic));
        }
        fn cpu_bytes(&self) -> usize {
            self.dynamic.capacity() * std::mem::size_of::<DynamicVertex>()
                + self.geometry.capacity() * std::mem::size_of::<SurfaceVertex>()
        }
    }
    struct CachedTile {
        key: TileKey,
        base_heights: Vec<f64>,
        surface_version: u32,
        tile: Tile,
        prepared: Option<Mesh>,
        borders: Vec<BorderNormal>,
        gpu: Option<GpuMesh>,
        last_used: u64,
        cpu_base: usize,
        bounds: (DVec3, f64),
        morph_source: u64,
        transition_start: f64,
    }
    impl CachedTile {
        fn cpu_bytes(&self) -> usize {
            std::mem::size_of::<Self>()
                + self.cpu_base
                + self.base_heights.capacity() * 8
                + self.tile.morph_offsets.capacity() * 24
                + self.prepared.as_ref().map_or(0, |m| {
                    m.vertices.capacity() * std::mem::size_of::<Vertex>() + m.indices.capacity() * 4
                })
                + self.gpu.as_ref().map_or(0, GpuMesh::cpu_bytes)
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum WorkKind {
        Refine,
        Parent,
        Coarsen,
        Edge,
    }
    struct CutWork {
        key: TileKey,
        mesh: Option<Mesh>,
        dynamic: Vec<DynamicVertex>,
        geometry: Option<Vec<SurfaceVertex>>,
        ocean_start: Option<usize>,
        indices: Option<Vec<usize>>,
        kind: WorkKind,
        cursor: usize,
    }
    struct PendingCut {
        parents: Vec<TileKey>,
        children: Vec<TileKey>,
        proposed: HashSet<TileKey>,
        jobs: Vec<(TileKey, WorkKind, TileKey)>,
        cursor: usize,
        work: Option<CutWork>,
        shared: HashMap<(u8, [i64; 3]), [f32; 3]>,
        coarsening: bool,
        instant: bool,
        epoch: u32,
        started: Option<f64>,
    }
    #[derive(Default)]
    struct RenderMetrics {
        uploads: u64,
        upload_bytes: u64,
        mesh_builds: u64,
        cut_commits: u64,
        instant_coarsen_commits: u64,
        instant_refine_commits: u64,
        reused_tiles: u64,
        max_prepare_ms: f64,
        last_prepare_ms: f64,
        last_render_ms: f64,
        last_frame_time: f32,
        max_render_ms: f64,
        surface_fallbacks: u64,
    }

    fn dynamic_vertex(v: &Vertex) -> DynamicVertex {
        DynamicVertex {
            morph: [v.morph[0], v.morph[1], v.morph[2], 0.0],
            shading_normal: v.shading_normal,
            parent_normal: v.parent_normal,
            water_depth: v.fields[3],
        }
    }
    fn surface_vertex(v: &Vertex) -> SurfaceVertex {
        let p = mesh_point(v);
        let water = v.color[3].round() as u32 & 1 != 0;
        SurfaceVertex {
            direction: p.normalize(),
            radius: p.length(),
            depth: v.fields[3],
            water,
            ocean: water && v.fields[2] == -1.0,
            normal: v.shading_normal,
        }
    }
    fn tile_cpu_base(tile: &Tile, borders: &Vec<BorderNormal>) -> usize {
        tile.vertices.capacity() * std::mem::size_of::<Sample>()
            + tile
                .vertices
                .iter()
                .map(|s| s.biome.capacity())
                .sum::<usize>()
            + tile.indices.capacity() * 4
            + borders.capacity() * std::mem::size_of::<BorderNormal>()
    }

    struct SurfaceUpdate {
        version: u32,
        field: Option<world_core::hybrid::HybridField>,
        keys: Vec<TileKey>,
        packets: HashMap<TileKey, PreparedTile>,
        staged: HashMap<TileKey, (PreparedTile, GpuMesh)>,
        shared: HashMap<(u8, [i64; 3]), [f32; 3]>,
        epoch: u32,
        invalidated: HashSet<TileKey>,
        reservations: HashMap<TileKey, (usize, usize)>,
    }

    struct Renderer {
        surface: wgpu::Surface<'static>,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: wgpu::SurfaceConfiguration,
        pipeline: wgpu::RenderPipeline,
        uniform: wgpu::Buffer,
        binding: wgpu::BindGroup,
        depth: wgpu::TextureView,
        world: Option<World>,
        globe: Option<GpuMesh>,
        base_surfaces: [Vec<DVec3>; 2],
        tiles: Vec<CachedTile>,
        pending: Option<PendingCut>,
        surface_update: Option<SurfaceUpdate>,
        surface_field: Option<world_core::hybrid::HybridField>,
        surface_version: u32,
        cut_dirty: bool,
        surface_priority_until: f64,
        cut_visible: bool,
        signature: Option<[u64; 5]>,
        last_camera_change: f64,
        cut_serial: u64,
        morph_start: f64,
        epochs: [MorphEpoch; 32],
        next_epoch: u32,
        neighbor_serial: u64,
        neighbor_cache: HashMap<(TileKey, bool), Vec<TileKey>>,
        desired: Vec<TileKey>,
        wanted: u8,
        draw_keys: Vec<TileKey>,
        tick: u64,
        metrics: RenderMetrics,
        tile_lookup: HashMap<TileKey, usize>,
        active_keys: Vec<TileKey>,
        active_set: HashSet<TileKey>,
        rivers: RiverLayer,
        human: HumanLayer,
        objects: Option<GpuMesh>,
        show_rivers: bool,
        show_human: bool,
        yaw: f64,
        pitch: f64,
        view_tilt: f64,
        altitude: f64,
        exaggeration: f64,
        mode: f32,
    }

    fn depth_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("depth"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default())
    }

    impl Renderer {
        fn clear_cut(&mut self) {
            self.active_keys.clear();
            self.active_set.clear();
            self.cut_visible = false;
            self.draw_keys.clear();
            self.cancel_pending();
        }
        fn rebuild_lookup(&mut self) {
            self.tile_lookup = self
                .tiles
                .iter()
                .enumerate()
                .map(|(i, tile)| (tile.key, i))
                .collect();
        }
        fn aspect(&self) -> f64 {
            f64::from(self.config.width) / f64::from(self.config.height.max(1))
        }

        fn requested_level_for(&self, altitude: f64) -> u8 {
            let step = ((self.config.height.max(1) as f64
                / (DEEP_STREAM_SEGMENTS as f64 * altitude * 0.41421356237 * 2.0))
                .log2()
                .ceil() as i32)
                .clamp(0, i32::from(MAX_TILE_LEVEL)) as u8;
            camera_level(altitude).max(step)
        }
        fn level_for(&self, direction: DVec3, altitude: f64) -> u8 {
            let requested = self.requested_level_for(altitude);
            let base = bounded_footprint_level(
                direction,
                requested,
                self.aspect(),
                altitude,
                self.view_tilt,
            );
            if let Some(field) = &self.surface_field {
                if field.kind == world_core::hybrid::SurfaceKind::Height {
                    if let Some(spacing) = field.source_spacing(direction.to_array()) {
                        let radius = self
                            .world
                            .as_ref()
                            .map_or(1_737_000.0, |w| w.config.radius_m);
                        let native = (2.0 * radius / (DEEP_STREAM_SEGMENTS as f64 * spacing))
                            .log2()
                            .ceil()
                            .clamp(0.0, f64::from(MAX_TILE_LEVEL))
                            as u8;
                        return base.max(native.min(requested.saturating_add(1)));
                    }
                }
            }
            base
        }
        fn detail_level(&self) -> u8 {
            self.level_for(self.direction(), self.altitude)
        }
        fn footprint(&self, level: u8) -> Vec<TileKey> {
            let direction = self.direction();
            let base = bounded_footprint_level(
                direction,
                self.requested_level_for(self.altitude),
                self.aspect(),
                self.altitude,
                self.view_tilt,
            );
            if level > base {
                return inspection_targets(
                    direction,
                    level,
                    base,
                    self.aspect(),
                    self.altitude,
                    self.view_tilt,
                );
            }
            graduate_targets(
                &footprint_keys(
                    direction,
                    level,
                    self.aspect(),
                    self.altitude,
                    self.view_tilt,
                ),
                level,
                direction,
            )
        }
        fn requests_at(&self, direction: DVec3, altitude: f64) -> Vec<TileKey> {
            let (cpu, gpu) = self.cache_bytes();
            if cpu >= 768 * 1024 * 1024 * 4 / 5 || gpu >= 768 * 1024 * 1024 * 4 / 5 {
                return Vec::new();
            }
            let level = self.level_for(direction, altitude);
            let targets = graduate_targets(
                &footprint_keys(direction, level, self.aspect(), altitude, self.view_tilt),
                level,
                direction,
            );
            prefetch_keys(&self.active_set, &targets, level, direction)
        }
        fn staged_parent(&self) -> Option<TileKey> {
            self.active_set
                .iter()
                .copied()
                .filter(|key| needs_refinement(*key, &self.desired, self.wanted))
                .filter(|key| split_preserves_grading(*key, &self.active_set))
                .min_by(|a, b| {
                    a.level.cmp(&b.level).then_with(|| {
                        tile_center(*b)
                            .dot(self.direction())
                            .total_cmp(&tile_center(*a).dot(self.direction()))
                    })
                })
        }
        fn requests(&self) -> (Vec<TileKey>, usize) {
            let mut keys = frontier_keys(
                &self.active_set,
                &self.desired,
                self.wanted,
                self.direction(),
                true,
            );
            let (cpu, gpu) = self.cache_bytes();
            let prefetch = cpu < 768 * 1024 * 1024 * 4 / 5 && gpu < 768 * 1024 * 1024 * 4 / 5;
            if !prefetch {
                keys = pressure_frontier(keys, &self.active_set, self.staged_parent());
            }
            let essential_count = keys.len();
            if prefetch {
                let future = self.requests_at(self.direction(), self.altitude * 0.5);
                append_prefetch(&mut keys, &future);
            }
            (keys, essential_count)
        }
        fn direction(&self) -> DVec3 {
            DVec3::new(
                self.pitch.cos() * self.yaw.sin(),
                self.pitch.sin(),
                self.pitch.cos() * self.yaw.cos(),
            )
        }

        fn dynamic_delta(&self, d: &DynamicVertex, time: f64, final_geometry: bool) -> f64 {
            let p = if final_geometry {
                1.0
            } else {
                epoch_progress(d.morph[1], time, &self.epochs)
            };
            d.morph[0] as f64 * (1.0 - p) + d.morph[3] as f64 * p
        }
        fn cached_surface(
            &self,
            key: TileKey,
            direction: DVec3,
            time: f64,
            final_geometry: bool,
        ) -> Option<(f64, [f32; 3], f64, f64)> {
            let tile = &self.tiles[*self.tile_lookup.get(&key)?];
            let gpu = tile.gpu.as_ref()?;
            let side = (tile.tile.vertices.len() as f64).sqrt() as usize;
            let (ground, normal) = surface_query(direction, key, side, |i| {
                let g = gpu.geometry[i];
                let d = &gpu.dynamic[i];
                let p = if final_geometry {
                    1.0
                } else {
                    epoch_progress(d.morph[1], time, &self.epochs)
                };
                let normal = Vec3::from_array(d.parent_normal)
                    .lerp(Vec3::from_array(d.shading_normal), p as f32)
                    .normalize_or_zero()
                    .to_array();
                (
                    g.direction * (g.radius + self.dynamic_delta(d, time, final_geometry)),
                    normal,
                )
            })?;
            let ocean = gpu.ocean_start.and_then(|start| {
                surface_scalar(direction, key, side, |i| {
                    let g = gpu.geometry[start + i];
                    let d = &gpu.dynamic[start + i];
                    let p = if final_geometry {
                        1.0
                    } else {
                        epoch_progress(d.morph[1], time, &self.epochs)
                    };
                    (
                        g.direction * (g.radius + self.dynamic_delta(d, time, final_geometry)),
                        d.morph[2] as f64 * (1.0 - p) + d.water_depth as f64 * p,
                    )
                })
            });
            let (sea, depth) = ocean.unwrap_or((1.0, -1e9));
            Some((ground, normal, sea, depth))
        }
        fn parent_source(
            &self,
            parent: TileKey,
            child: TileKey,
            index: usize,
            side: usize,
            direction: DVec3,
            ocean: bool,
            time: f64,
        ) -> Option<(f64, [f32; 3], f64)> {
            let tile = &self.tiles[*self.tile_lookup.get(&parent)?];
            let gpu = tile.gpu.as_ref()?;
            if tile.tile.vertices.len() != side * side {
                return self.cached_surface(parent, direction, time, true).map(|s| {
                    if ocean {
                        (s.2, direction.as_vec3().to_array(), s.3)
                    } else {
                        (s.0, s.1, 0.0)
                    }
                });
            }
            let offset = if ocean { gpu.ocean_start? } else { 0 };
            nested_parent_source(child, index, side, direction, |i| {
                let g = gpu.geometry[offset + i];
                let d = &gpu.dynamic[offset + i];
                (
                    g.direction * (g.radius + d.morph[3] as f64),
                    d.shading_normal,
                    d.water_depth as f64,
                )
            })
        }
        fn view_frame(&self, time: f64) -> (DVec3, DVec3, DVec3, DVec3) {
            let direction = self.direction();
            let radius = 1.0 + self.ground_height(time);
            let (eye, _, _, _) = camera_frame(direction, self.altitude, self.view_tilt, radius);
            let eye_direction = eye.normalize();
            let ground = self
                .displayed_radius(eye_direction, false, None, time)
                .max(self.displayed_radius(eye_direction, true, None, time));
            let eye = eye_direction * eye.length().max(ground + self.altitude * 0.25);
            let forward = (direction * radius - eye).normalize();
            let (right, up) = camera_axes(forward);
            (eye, forward, right, up)
        }
        fn can_coarsen_instantly(
            &self,
            keys: impl IntoIterator<Item = TileKey>,
            frame: (DVec3, DVec3, DVec3, DVec3),
        ) -> bool {
            let keys: Vec<_> = keys.into_iter().collect();
            // Both samples influence the rendered camera, even if the nadir
            // behind an inclined eye lies outside its visible frustum.
            if touches_camera_support(&keys, &self.active_set, self.direction(), frame.0) {
                return false;
            }
            let bounds: Option<Vec<_>> = keys
                .into_iter()
                .map(|key| {
                    self.tile_lookup.get(&key).map(|&i| {
                        let tile = &self.tiles[i];
                        (
                            tile.bounds.0,
                            tile.bounds.1
                                + tile.gpu.as_ref().map_or(0.0, |gpu| gpu.morph_pad)
                                + 1e-6,
                        )
                    })
                })
                .collect();
            bounds.is_some_and(|bounds| {
                instant_coarsen_bounds(&bounds, frame, self.aspect(), self.config.height)
            })
        }
        fn displayed_radius(
            &self,
            direction: DVec3,
            water: bool,
            _below: Option<u8>,
            time: f64,
        ) -> f64 {
            let (face, u, v) = face_uv(direction);
            let Some(key) = lod::covering_key(&self.active_set, face as u8, u, v) else {
                return 1.0;
            };
            let Some((ground, _, sea, depth)) = self.cached_surface(key, direction, time, false)
            else {
                return 1.0;
            };
            if !water {
                return ground;
            }
            let tile = &self.tiles[self.tile_lookup[&key]];
            let side = (tile.tile.vertices.len() as f64).sqrt() as usize;
            let radius = self.world.as_ref().map_or(1.0, |w| w.config.radius_m);
            let inland = surface_scalar(direction, key, side, |i| {
                let sample = &tile.tile.vertices[i];
                let valid = water_body(sample)
                    .is_some_and(|(id, level)| id != 0 && level > sample.height_m);
                (
                    DVec3::from_array(sample.direction).normalize()
                        * (1.0 + water_height(sample) * self.exaggeration / radius),
                    if valid { 1.0 } else { -1.0 },
                )
            })
            .filter(|(_, wet)| *wet > 0.0)
            .map_or(ground, |v| v.0);
            inland.max(if depth > 0.01 { sea } else { ground })
        }
        fn ground_height(&self, time: f64) -> f64 {
            let d = self.direction();
            self.displayed_radius(d, false, None, time)
                .max(self.displayed_radius(d, true, None, time))
                - 1.0
        }
        fn cancel_pending(&mut self) {
            if let Some(mut pending) = self.pending.take() {
                if let Some(work) = pending.work.take() {
                    if let Some(mesh) = work.mesh {
                        if let Some(&i) = self.tile_lookup.get(&work.key) {
                            self.tiles[i].prepared = Some(mesh);
                        }
                    }
                }
                // A cancelled preparation must not leave its reserved slot at
                // the future sentinel and block the epoch ring on its next lap.
                self.epochs[pending.epoch as usize % 32].start = -1e9;
            }
        }
        fn cache_bytes(&self) -> (usize, usize) {
            let patch_bytes = |field: &world_core::hybrid::HybridField| {
                field
                    .patches()
                    .iter()
                    .map(|patch| patch.values.capacity() * 4)
                    .sum::<usize>()
            };
            let packet_bytes = |packet: &PreparedTile| {
                tile_cpu_base(&packet.tile, &packet.borders)
                    + packet.base_heights.capacity() * 8
                    + packet.mesh.vertices.capacity() * std::mem::size_of::<Vertex>()
                    + packet.mesh.indices.capacity() * 4
            };
            let mut cpu = self.tiles.iter().map(CachedTile::cpu_bytes).sum::<usize>()
                + self.surface_field.as_ref().map_or(0, patch_bytes);
            let mut gpu = self
                .tiles
                .iter()
                .filter_map(|tile| tile.gpu.as_ref())
                .map(|mesh| mesh.bytes)
                .sum::<usize>();
            if let Some(update) = &self.surface_update {
                cpu += update.field.as_ref().map_or(0, patch_bytes);
                cpu += update.packets.values().map(packet_bytes).sum::<usize>();
                cpu += update
                    .staged
                    .values()
                    .map(|(packet, mesh)| packet_bytes(packet) + mesh.cpu_bytes())
                    .sum::<usize>();
                gpu += update
                    .staged
                    .values()
                    .map(|(_, mesh)| mesh.bytes)
                    .sum::<usize>();
            }
            (cpu, gpu)
        }
        fn evict_cache(&mut self) {
            self.evict_for_capacity((0, 0), &HashSet::new());
        }
        fn evict_for_capacity(&mut self, reserve: (usize, usize), additional: &HashSet<TileKey>) {
            let limits = (TERRAIN_MEMORY_LIMIT, TERRAIN_MEMORY_LIMIT);
            if within_surface_budget(self.cache_bytes(), reserve, limits) {
                return;
            }
            let mut pinned = self.active_set.clone();
            pinned.extend(additional.iter().copied());
            if let Some(update) = &self.surface_update {
                pinned.extend(update.keys.iter().copied());
            }
            if reserve == (0, 0) {
                pinned.extend(self.active_set.iter().filter_map(|leaf| lod::parent(*leaf)));
            }
            let (face, u, v) = face_uv(self.direction());
            if let Some(center) =
                lod::covering_key(&self.active_set, face as u8, u, v).filter(|_| reserve == (0, 0))
            {
                // Keep the central return path resident without retaining every
                // historical ancestor of the entire visible partition.
                pinned.extend((0..center.level).filter_map(|level| lod::ancestor(center, level)));
            }
            // Reserve a complete sibling quartet before its fourth packet arrives.
            // Without this reservation, pressure can repeatedly evict the first three.
            if let Some(parent) = self.staged_parent().filter(|_| reserve == (0, 0)) {
                pinned.extend(lod::children(parent));
            }
            if let Some(p) = &self.pending {
                pinned.extend(p.parents.iter().copied());
                pinned.extend(p.children.iter().copied());
                pinned.extend(p.jobs.iter().map(|v| v.0));
            }
            let wanted: HashSet<_> = self.requests().0.into_iter().collect();
            loop {
                if within_surface_budget(self.cache_bytes(), reserve, limits) {
                    break;
                }
                let victim = self
                    .tiles
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.key.level != 0 && !pinned.contains(&t.key))
                    .min_by_key(|(_, t)| (wanted.contains(&t.key), t.last_used))
                    .map(|(i, _)| i);
                let Some(i) = victim else {
                    break;
                };
                self.tiles.swap_remove(i);
                self.rebuild_lookup();
            }
        }

        fn cancel_surface_update(&mut self) {
            if let Some(update) = self.surface_update.take() {
                self.epochs[update.epoch as usize % 32].start = -1e9;
                // Ordinary requests prepared with the uncommitted worker field
                // must never enter a later procedural cut after cancellation.
                self.tiles.retain(|tile| {
                    self.active_set.contains(&tile.key)
                        || tile.surface_version == self.surface_version
                });
                self.rebuild_lookup();
                self.cut_dirty = true;
            }
        }

        fn surface_target(
            &self,
            update: &SurfaceUpdate,
            key: TileKey,
            direction: DVec3,
        ) -> Option<(f64, [f32; 3], f64, f64)> {
            let Some((packet, gpu)) = update.staged.get(&key) else {
                return self.cached_surface(key, direction, js_now(), true);
            };
            let side = (packet.tile.vertices.len() as f64).sqrt() as usize;
            let (height, normal) = surface_query(direction, key, side, |i| {
                let g = gpu.geometry[i];
                let dynamic = gpu.dynamic[i];
                (
                    g.direction * (g.radius + dynamic.morph[3] as f64),
                    dynamic.shading_normal,
                )
            })?;
            let (water, depth) = gpu
                .ocean_start
                .and_then(|start| {
                    surface_scalar(direction, key, side, |i| {
                        let g = gpu.geometry[start + i];
                        let dynamic = gpu.dynamic[start + i];
                        (
                            g.direction * (g.radius + dynamic.morph[3] as f64),
                            dynamic.water_depth as f64,
                        )
                    })
                })
                .unwrap_or((height, -1e9));
            Some((height, normal, water, depth))
        }

        fn process_surface_update(&mut self) {
            let Some(mut update) = self.surface_update.take() else {
                return;
            };
            if update.packets.len() + update.staged.len() != update.keys.len() {
                self.surface_update = Some(update);
                return;
            }
            if update.shared.is_empty() {
                // Include untouched neighbors, as in normal LOD transitions.
                let mut sums: HashMap<(u8, [i64; 3]), DVec3> = HashMap::new();
                let normal_keys: HashSet<_> = self
                    .active_keys
                    .iter()
                    .chain(update.keys.iter())
                    .copied()
                    .collect();
                for key in &normal_keys {
                    let (tile, borders) = if let Some(packet) = update.packets.get(key) {
                        (&packet.tile, &packet.borders)
                    } else {
                        let tile = &self.tiles[self.tile_lookup[key]];
                        (&tile.tile, &tile.borders)
                    };
                    for border in borders {
                        *sums
                            .entry((
                                key.level,
                                normal_key(DVec3::from_array(
                                    tile.vertices[border.index as usize].direction,
                                )),
                            ))
                            .or_default() += DVec3::from_array(border.sum);
                    }
                }
                update.shared = sums
                    .into_iter()
                    .map(|(key, value)| (key, value.normalize_or_zero().as_vec3().to_array()))
                    .collect();
            }
            // Batch small uploads within a short frame budget. The old cut
            // remains visible until the complete replacement is ready.
            let upload_deadline = js_now() + 0.005;
            let mut uploaded = 0;
            while let Some(key) = update
                .keys
                .iter()
                .copied()
                .find(|key| !update.staged.contains_key(key))
            {
                let mut packet = update.packets.remove(&key).unwrap();
                let old = &self.tiles[self.tile_lookup[&key]];
                let ground_count = packet.tile.vertices.len();
                let side = (ground_count as f64).sqrt() as usize;
                let ocean_start = packet
                    .mesh
                    .vertices
                    .iter()
                    .position(|vertex| surface_vertex(vertex).ocean);
                let mut dynamic = Vec::with_capacity(packet.mesh.vertices.len());
                let time = js_now();
                for (i, vertex) in packet.mesh.vertices.iter().enumerate() {
                    let g = surface_vertex(vertex);
                    let original_index = if i < ground_count {
                        Some(i)
                    } else {
                        ocean_start.filter(|_| g.ocean).map(|start| i - start)
                    };
                    let border = original_index.filter(|i| {
                        i % side == 0
                            || i % side == side - 1
                            || *i < side
                            || *i >= ground_count - side
                    });
                    let mut target = (g.radius, g.normal, g.radius, g.depth as f64);
                    if let Some(index) = border {
                        if let Some(coarse) = border_neighbors(key, index, side, &self.active_set)
                            .into_iter()
                            .filter(|neighbor| neighbor.level < key.level)
                            .min_by_key(|neighbor| neighbor.level)
                        {
                            if let Some(surface) = self.surface_target(&update, coarse, g.direction)
                            {
                                target = surface;
                            }
                        } else if !g.water {
                            if let Some(normal) = update.shared.get(&(
                                key.level,
                                normal_key(DVec3::from_array(
                                    packet.tile.vertices[index].direction,
                                )),
                            )) {
                                target.1 = *normal;
                            }
                        }
                    }
                    let source = if let Some(index) = original_index.filter(|_| !g.ocean) {
                        let gpu = old.gpu.as_ref().unwrap();
                        let point = gpu.geometry[index];
                        let old_dynamic = gpu.dynamic[index];
                        let progress = epoch_progress(old_dynamic.morph[1], time, &self.epochs);
                        let normal = Vec3::from_array(old_dynamic.parent_normal)
                            .lerp(
                                Vec3::from_array(old_dynamic.shading_normal),
                                progress as f32,
                            )
                            .normalize_or_zero()
                            .to_array();
                        (
                            point.radius + self.dynamic_delta(&old_dynamic, time, false),
                            normal,
                            0.0,
                        )
                    } else if g.ocean {
                        self.cached_surface(key, g.direction, time, false)
                            .map(|surface| (surface.2, g.normal, surface.3))
                            .unwrap_or((g.radius, g.normal, g.depth as f64))
                    } else {
                        (g.radius, g.normal, g.depth as f64)
                    };
                    let (initial, final_radius, final_normal, initial_depth, final_depth) =
                        if g.ocean {
                            (source.0, target.2, g.normal, source.2, target.3)
                        } else if g.water {
                            (g.radius, g.radius, g.normal, g.depth as f64, g.depth as f64)
                        } else {
                            (source.0, target.0, target.1, g.depth as f64, g.depth as f64)
                        };
                    dynamic.push(DynamicVertex {
                        morph: [
                            (initial - g.radius) as f32,
                            -(update.epoch as f32),
                            initial_depth as f32,
                            (final_radius - g.radius) as f32,
                        ],
                        shading_normal: final_normal,
                        parent_normal: source.1,
                        water_depth: final_depth as f32,
                    });
                }
                let gpu = GpuMesh::upload_dynamic(&self.device, &self.queue, &packet.mesh, dynamic);
                self.metrics.uploads += 1;
                self.metrics.upload_bytes += gpu.bytes as u64;
                // Geometry now lives in GpuMesh; keep samples/borders for
                // normals and collision, but free the full decoded Mesh.
                packet.mesh = Mesh::default();
                update.staged.insert(key, (packet, gpu));
                uploaded += 1;
                if uploaded >= 16 || js_now() >= upload_deadline {
                    break;
                }
            }
            if update.staged.len() != update.keys.len() {
                self.surface_update = Some(update);
                return;
            }
            let start = f64::from(js_now() as f32);
            self.epochs[update.epoch as usize % 32].start = start as f32;
            for (key, (packet, gpu)) in update.staged {
                let index = self.tile_lookup[&key];
                let bounds = (gpu.bound_center, gpu.bound_radius);
                let cpu_base = tile_cpu_base(&packet.tile, &packet.borders);
                self.tiles[index] = CachedTile {
                    key,
                    base_heights: packet.base_heights,
                    surface_version: update.version,
                    tile: packet.tile,
                    prepared: None,
                    borders: packet.borders,
                    gpu: Some(gpu),
                    last_used: self.tick,
                    cpu_base,
                    bounds,
                    morph_source: self.cut_serial,
                    transition_start: start,
                };
            }
            self.tiles.retain(|tile| {
                tile.key.level == 0
                    || self.active_set.contains(&tile.key)
                    || !update.invalidated.contains(&tile.key)
            });
            for tile in &mut self.tiles {
                tile.surface_version = update.version;
            }
            self.rebuild_lookup();
            self.surface_field = update.field;
            self.surface_version = update.version;
            self.signature = None;
            self.morph_start = start;
            self.cut_dirty = true;
            self.evict_cache();
        }

        fn install_roots(&mut self) -> Result<(), String> {
            self.cancel_surface_update();
            self.cancel_pending();
            self.neighbor_cache.clear();
            self.tiles.clear();
            self.tile_lookup.clear();
            self.active_set.clear();
            self.active_keys.clear();
            self.draw_keys.clear();
            let world = self.world.as_ref().ok_or("Monde absent")?;
            let radius = world.config.radius_m;
            let roots = root_tiles(world)?;
            let mut compiled = Vec::new();
            for tile in roots {
                let packet = prepare_hybrid_tile(
                    tile,
                    radius,
                    self.exaggeration,
                    self.surface_field.as_ref(),
                )?;
                let prepared = decode_prepared_tile(&packet)?;
                compiled.push((
                    prepared.tile,
                    prepared.mesh,
                    prepared.borders,
                    prepared.base_heights,
                ));
            }
            let mut sums: HashMap<[i64; 3], DVec3> = HashMap::new();
            for (tile, _, borders, _) in &compiled {
                for border in borders {
                    *sums
                        .entry(normal_key(DVec3::from_array(
                            tile.vertices[border.index as usize].direction,
                        )))
                        .or_default() += DVec3::from_array(border.sum);
                }
            }
            for (tile, mut mesh, borders, base_heights) in compiled {
                for border in &borders {
                    let i = border.index as usize;
                    if let Some(sum) =
                        sums.get(&normal_key(DVec3::from_array(tile.vertices[i].direction)))
                    {
                        let normal = sum.normalize_or_zero().as_vec3().to_array();
                        mesh.vertices[i].shading_normal = normal;
                        mesh.vertices[i].parent_normal = normal;
                    }
                }
                let key = tile.key;
                let gpu = GpuMesh::upload(&self.device, &self.queue, &mesh);
                let bounds = (gpu.bound_center, gpu.bound_radius);
                let cpu_base = tile_cpu_base(&tile, &borders);
                self.tiles.push(CachedTile {
                    key,
                    base_heights,
                    surface_version: self.surface_version,
                    tile,
                    prepared: None,
                    borders,
                    gpu: Some(gpu),
                    last_used: self.tick,
                    cpu_base,
                    bounds,
                    morph_source: 0,
                    transition_start: -1e9,
                });
                self.active_set.insert(key);
            }
            self.rebuild_lookup();
            self.sync_cut();
            self.cut_visible = true;
            self.cut_dirty = true;
            self.signature = None;
            Ok(())
        }
        fn sync_cut(&mut self) {
            self.active_keys = self.active_set.iter().copied().collect();
            self.active_keys.sort();
            // A sphere footprint cannot bound valleys beyond an elevated cliff.
            // The GPU clips this complete partition; no terrain may vanish by culling.
            self.draw_keys = self.active_keys.clone();
        }
        fn stitch_target(
            &self,
            key: TileKey,
            index: usize,
            next: &HashSet<TileKey>,
            time: f64,
        ) -> Option<(f64, [f32; 3], f64, f64)> {
            let tile = &self.tiles[*self.tile_lookup.get(&key)?];
            let n = tile.tile.vertices.len();
            let side = (n as f64).sqrt() as usize;
            let direction = DVec3::from_array(tile.tile.vertices[index].direction);
            let neighbors = border_neighbors(key, index, side, next);
            let coarse = neighbors
                .into_iter()
                .filter(|k| k.level < key.level)
                .min_by_key(|k| k.level)?;
            self.cached_surface(coarse, direction, time, true)
        }
        fn plan_transitions(
            &mut self,
            candidates: impl IntoIterator<Item = TileKey>,
            coarsening: bool,
            now: f64,
            transitioning: bool,
            frame_deadline: f64,
        ) -> bool {
            let Some(epoch) = next_morph_epoch(self.next_epoch, now, &self.epochs) else {
                self.cut_dirty = true;
                return false;
            };
            let frame = self.view_frame(now);
            let mut parents = Vec::new();
            let mut children = Vec::new();
            let mut neighbors = Vec::new();
            let mut reserved = HashSet::new();
            let mut instant = false;
            let mut proposed = self.active_set.clone();
            let plan_begin = js_now();
            for parent in candidates {
                if js_now() >= frame_deadline.min(plan_begin + 0.002) {
                    // Rejected candidates can cost as much as accepted ones.
                    // Continue planning next frame instead of scanning the
                    // entire partition while its neighboring morphs settle.
                    self.cut_dirty = true;
                    break;
                }
                if transitioning && !self.neighbors_settled(parent, coarsening, now) {
                    continue;
                }
                // Loaded central children cannot outrun their supporting ring.
                // Check the next partition, including earlier accepted groups.
                if !coarsening && !split_preserves_grading(parent, &proposed) {
                    continue;
                }
                let affected = self.affected_neighbors(parent, coarsening);
                if if coarsening {
                    // Earlier accepted groups only coarsen their neighbors.
                    // The cached fine-side bound stays conservative for every
                    // proposed merge; the complete child quartet is checked.
                    !lod::children(parent)
                        .iter()
                        .all(|key| proposed.contains(key))
                        || affected.iter().any(|key| key.level > parent.level + 1)
                } else {
                    false
                } {
                    continue;
                }
                let quartet = lod::children(parent);
                let candidate_instant = self.can_coarsen_instantly(
                    std::iter::once(parent)
                        .chain(quartet)
                        .chain(affected.iter().copied()),
                    frame,
                );
                if !parents.is_empty() && candidate_instant != instant {
                    continue;
                }
                let footprint =
                    transition_footprint(parent, &self.active_set, coarsening, &affected);
                if !reserved.is_disjoint(&footprint) {
                    continue;
                }
                let changed = if coarsening {
                    lod::coarsen(&mut proposed, parent)
                } else {
                    lod::split(&mut proposed, parent)
                };
                if !changed {
                    continue;
                }
                instant = candidate_instant;
                parents.push(parent);
                children.extend(quartet);
                neighbors.extend(affected.into_iter().map(|key| (key, parent)));
                reserved.extend(footprint);
                if parents.len() == 8 || js_now() - plan_begin >= 0.002 {
                    break;
                }
            }
            if parents.is_empty() {
                return false;
            }
            self.next_epoch = epoch;
            self.epochs[epoch as usize % 32] = MorphEpoch {
                id: epoch,
                start: 1e9,
            };
            let mut shared_keys: HashSet<_> = children.iter().copied().collect();
            shared_keys.extend(parents.iter().copied());
            shared_keys.extend(neighbors.iter().map(|(key, _)| *key));
            let same_level_sources: Vec<_> = shared_keys.iter().copied().collect();
            for key in same_level_sources {
                for neighbor in tile_neighbors(key, &proposed) {
                    if neighbor.level == key.level {
                        shared_keys.insert(neighbor);
                    }
                }
            }
            let mut sums: HashMap<(u8, [i64; 3]), DVec3> = HashMap::new();
            for key in shared_keys {
                if let Some(&i) = self.tile_lookup.get(&key) {
                    let tile = &self.tiles[i];
                    for border in &tile.borders {
                        *sums
                            .entry((
                                key.level,
                                normal_key(DVec3::from_array(
                                    tile.tile.vertices[border.index as usize].direction,
                                )),
                            ))
                            .or_default() += DVec3::from_array(border.sum);
                    }
                }
            }
            let shared = sums
                .into_iter()
                .map(|(k, v)| (k, v.normalize_or_zero().as_vec3().to_array()))
                .collect();
            let mut jobs = Vec::new();
            if coarsening {
                jobs.extend(
                    parents
                        .iter()
                        .map(|&parent| (parent, WorkKind::Parent, parent)),
                );
            }
            if transition_needs_child_uploads(coarsening, instant) {
                jobs.extend(children.iter().copied().map(|k| {
                    (
                        k,
                        if coarsening {
                            WorkKind::Coarsen
                        } else {
                            WorkKind::Refine
                        },
                        lod::parent(k).unwrap(),
                    )
                }));
            }
            jobs.extend(
                neighbors
                    .into_iter()
                    .map(|(key, parent)| (key, WorkKind::Edge, parent)),
            );
            self.pending = Some(PendingCut {
                parents,
                children,
                proposed,
                jobs,
                cursor: 0,
                work: None,
                shared,
                coarsening,
                instant,
                epoch,
                started: None,
            });
            true
        }
        fn affected_neighbors(&mut self, parent: TileKey, coarsening: bool) -> Vec<TileKey> {
            if self.neighbor_serial != self.cut_serial {
                self.neighbor_cache.clear();
                self.neighbor_serial = self.cut_serial;
            }
            self.neighbor_cache
                .entry((parent, coarsening))
                .or_insert_with(|| {
                    graded_transition_neighbors(parent, &self.active_set, coarsening)
                })
                .clone()
        }
        fn neighbors_settled(&mut self, parent: TileKey, coarsening: bool, time: f64) -> bool {
            let source = if coarsening {
                lod::children(parent).to_vec()
            } else {
                vec![parent]
            };
            if source
                .into_iter()
                .flat_map(|key| tile_neighbors(key, &self.active_set))
                .any(|key| {
                    self.tiles[self.tile_lookup[&key]].transition_start + MORPH_DURATION_SECONDS
                        > time
                })
            {
                return false;
            }
            self.affected_neighbors(parent, coarsening)
                .iter()
                .all(|key| {
                    self.tiles[self.tile_lookup[key]].transition_start + MORPH_DURATION_SECONDS
                        <= time
                })
        }
        fn refresh_cut(&mut self) {
            self.tick = self.tick.wrapping_add(1);
            if self.world.is_none() {
                return;
            }
            let begin = js_now();
            let deadline = begin
                + cut_prepare_budget(
                    begin - self.last_camera_change,
                    self.metrics.last_render_ms,
                    self.metrics.last_prepare_ms,
                );
            for key in &self.active_keys {
                if let Some(&index) = self.tile_lookup.get(key) {
                    self.tiles[index].last_used = self.tick;
                }
            }
            let signature = [
                self.yaw.to_bits(),
                self.pitch.to_bits(),
                self.altitude.to_bits(),
                self.aspect().to_bits(),
                self.view_tilt.to_bits(),
            ];
            if self.signature != Some(signature) {
                self.signature = Some(signature);
                self.last_camera_change = js_now();
                self.cut_dirty = true;
                self.wanted = self.detail_level();
                self.desired = self.footprint(self.wanted);
                self.sync_cut();
            }
            if self.surface_update.is_some() {
                // Input and the rendered camera keep moving. Only partition
                // changes wait for this short, atomic geometry transaction.
                self.process_surface_update();
                return;
            }
            // In-flight local work is never cancelled merely because the camera moved.
            if self.pending.is_some() {
                self.process_cut(deadline);
                return;
            }
            // Let a ready neural field take the next idle slot instead of
            // starting another LOD batch every frame and starving its commit.
            if js_now() < self.surface_priority_until {
                return;
            }
            if !self.cut_dirty {
                return;
            }
            self.cut_dirty = false;
            let now = js_now();
            let transitioning = self.active_set.iter().any(|key| {
                self.tiles[self.tile_lookup[key]].transition_start + MORPH_DURATION_SECONDS > now
            });
            let mut parents: HashSet<_> = self
                .active_set
                .iter()
                .filter_map(|k| lod::parent(*k))
                .collect();
            let mut coarsen: Vec<_> = parents
                .drain()
                .filter(|parent| {
                    lod::children(*parent)
                        .iter()
                        .all(|k| self.active_set.contains(k))
                        && self.tile_lookup.contains_key(parent)
                        && !needs_refinement(*parent, &self.desired, self.wanted)
                        && lod::children(*parent).iter().all(|k| {
                            self.tiles[self.tile_lookup[k]].transition_start
                                + MORPH_DURATION_SECONDS
                                <= now
                        })
                })
                .collect();
            coarsen.sort_by(|a, b| b.level.cmp(&a.level));
            let (cpu, gpu) = self.cache_bytes();
            // Off-screen old cuts must coarsen before new visible refinement
            // consumes their inverse parents and the last cache headroom.
            if cpu >= TERRAIN_MEMORY_LIMIT * 4 / 5 || gpu >= TERRAIN_MEMORY_LIMIT * 4 / 5 {
                let outside: Vec<_> = coarsen
                    .iter()
                    .copied()
                    .filter(|parent| !needs_refinement(*parent, &self.desired, self.wanted))
                    .collect();
                if self.plan_transitions(outside, true, now, transitioning, deadline) {
                    self.process_cut(deadline);
                    return;
                }
            }
            let mut refine: Vec<_> = self
                .active_set
                .iter()
                .copied()
                .filter(|key| {
                    needs_refinement(*key, &self.desired, self.wanted)
                        && lod::children(*key).iter().all(|k| {
                            self.tile_lookup.get(k).is_some_and(|&i| {
                                self.tiles[i].gpu.is_some() || self.tiles[i].prepared.is_some()
                            })
                        })
                        && self.tiles[self.tile_lookup[key]].transition_start
                            + MORPH_DURATION_SECONDS
                            <= now
                })
                .collect();
            refine.sort_by(|a, b| {
                a.level.cmp(&b.level).then_with(|| {
                    tile_center(*b)
                        .dot(self.direction())
                        .total_cmp(&tile_center(*a).dot(self.direction()))
                })
            });
            if self.plan_transitions(refine, false, now, transitioning, deadline) {
                self.process_cut(deadline);
                return;
            }
            if self.plan_transitions(coarsen, true, now, transitioning, deadline) {
                self.process_cut(deadline);
            } else if self.active_set.iter().any(|k| {
                self.tiles[self.tile_lookup[k]].transition_start + MORPH_DURATION_SECONDS > now
            }) {
                self.cut_dirty = true;
            }
        }
        fn process_cut(&mut self, deadline: f64) {
            let Some(mut pending) = self.pending.take() else {
                return;
            };
            let begin = js_now();
            let budget = (deadline - begin).max(0.0);
            if let Some(start) = pending.started {
                if pending.coarsening && begin < start + MORPH_DURATION_SECONDS {
                    self.pending = Some(pending);
                    return;
                }
                self.active_set = pending.proposed;
                self.cut_serial = self.cut_serial.wrapping_add(1);
                self.metrics.cut_commits += 1;
                self.cut_dirty = true;
                self.sync_cut();
                self.evict_cache();
                return;
            }
            while pending.cursor < pending.jobs.len() {
                if js_now() >= deadline {
                    break;
                }
                let (key, kind, parent) = pending.jobs[pending.cursor];
                let index = self.tile_lookup[&key];
                if pending.work.is_none() {
                    if kind != WorkKind::Edge
                        && (js_now() - begin >= budget * 0.5 || deadline - js_now() < 0.001)
                    {
                        break;
                    }
                    let tile = &mut self.tiles[index];
                    tile.last_used = self.tick;
                    let (mesh, dynamic, geometry, ocean_start, indices) = if kind == WorkKind::Edge
                    {
                        let gpu = tile.gpu.as_ref().expect("active edge mesh is resident");
                        let mut indices: Vec<_> =
                            tile.borders.iter().map(|b| b.index as usize).collect();
                        if let Some(start) = gpu.ocean_start {
                            indices.extend(tile.borders.iter().map(|b| start + b.index as usize));
                        }
                        let dynamic = indices.iter().map(|&i| gpu.dynamic[i]).collect();
                        self.metrics.reused_tiles += 1;
                        (None, dynamic, None, gpu.ocean_start, Some(indices))
                    } else if let Some(mesh) = tile.prepared.take() {
                        let dynamic = mesh.vertices.iter().map(dynamic_vertex).collect();
                        let geometry: Vec<_> = mesh.vertices.iter().map(surface_vertex).collect();
                        let ocean_start = geometry.iter().position(|g| g.ocean);
                        (Some(mesh), dynamic, Some(geometry), ocean_start, None)
                    } else if let Some(gpu) = &tile.gpu {
                        self.metrics.reused_tiles += 1;
                        (None, gpu.dynamic.clone(), None, gpu.ocean_start, None)
                    } else {
                        self.cut_dirty = true;
                        self.pending = Some(pending);
                        return;
                    };
                    pending.work = Some(CutWork {
                        key,
                        mesh,
                        dynamic,
                        geometry,
                        ocean_start,
                        indices,
                        kind,
                        cursor: 0,
                    });
                }
                let mut work = pending.work.take().unwrap();
                let length = work.indices.as_ref().map_or(work.dynamic.len(), Vec::len);
                let ground_count = self.tiles[index].tile.vertices.len();
                let side = (ground_count as f64).sqrt() as usize;
                let ocean_start = work.ocean_start;
                while work.cursor < length {
                    let i = work
                        .indices
                        .as_ref()
                        .map_or(work.cursor, |indices| indices[work.cursor]);
                    let g = work.geometry.as_ref().map_or_else(
                        || self.tiles[index].gpu.as_ref().unwrap().geometry[i],
                        |geometry| geometry[i],
                    );
                    let dynamic_index = if work.indices.is_some() {
                        work.cursor
                    } else {
                        i
                    };
                    let old = work.dynamic[dynamic_index];
                    let original_index = if i < ground_count {
                        Some(i)
                    } else {
                        ocean_start.filter(|_| g.ocean).map(|start| i - start)
                    };
                    let mut final_delta = 0.0;
                    let mut final_depth = g.depth as f64;
                    let mut final_normal = g.normal;
                    if let Some(sample_index) = original_index.filter(|s| {
                        s % side == 0
                            || s % side == side - 1
                            || s / side == 0
                            || s / side == side - 1
                    }) {
                        if let Some(target) =
                            self.stitch_target(key, sample_index, &pending.proposed, begin)
                        {
                            final_delta = if g.ocean {
                                target.2 - g.radius
                            } else {
                                target.0 - g.radius
                            };
                            final_depth = if g.ocean { target.3 } else { final_depth };
                            final_normal = if g.water { g.normal } else { target.1 };
                        } else if !g.water {
                            let d = DVec3::from_array(
                                self.tiles[index].tile.vertices[sample_index].direction,
                            );
                            if let Some(&normal) = pending.shared.get(&(key.level, normal_key(d))) {
                                final_normal = normal;
                            }
                        }
                    }
                    let p = epoch_progress(old.morph[1], begin, &self.epochs);
                    let current_delta = old.morph[0] as f64 * (1.0 - p) + old.morph[3] as f64 * p;
                    let current_depth =
                        old.morph[2] as f64 * (1.0 - p) + old.water_depth as f64 * p;
                    let current_normal = if p >= 1.0 {
                        old.shading_normal
                    } else {
                        Vec3::from_array(old.parent_normal)
                            .lerp(Vec3::from_array(old.shading_normal), p as f32)
                            .normalize_or_zero()
                            .to_array()
                    };
                    let source = if matches!(work.kind, WorkKind::Refine | WorkKind::Coarsen)
                        && (g.ocean || !g.water)
                    {
                        original_index
                            .and_then(|sample_index| {
                                let d = DVec3::from_array(
                                    self.tiles[index].tile.vertices[sample_index].direction,
                                );
                                self.parent_source(
                                    parent,
                                    key,
                                    sample_index,
                                    side,
                                    d,
                                    g.ocean,
                                    begin,
                                )
                            })
                            .unwrap_or_else(|| {
                                self.metrics.surface_fallbacks += 1;
                                (g.radius + current_delta, current_normal, current_depth)
                            })
                    } else {
                        (g.radius, g.normal, g.depth as f64)
                    };
                    let (initial_delta, initial_depth, initial_normal) = match work.kind {
                        WorkKind::Refine => (
                            if g.water && !g.ocean {
                                0.0
                            } else {
                                source.0 - g.radius
                            },
                            if g.ocean { source.2 } else { g.depth as f64 },
                            if g.water { g.normal } else { source.1 },
                        ),
                        WorkKind::Parent => (final_delta, final_depth, final_normal),
                        WorkKind::Coarsen => {
                            final_delta = if g.water && !g.ocean {
                                0.0
                            } else {
                                source.0 - g.radius
                            };
                            final_depth = if g.ocean { source.2 } else { g.depth as f64 };
                            final_normal = if g.water { g.normal } else { source.1 };
                            (current_delta, current_depth, current_normal)
                        }
                        WorkKind::Edge => (current_delta, current_depth, current_normal),
                    };
                    work.dynamic[dynamic_index] = DynamicVertex {
                        morph: [
                            initial_delta as f32,
                            -(pending.epoch as f32),
                            initial_depth as f32,
                            final_delta as f32,
                        ],
                        shading_normal: final_normal,
                        parent_normal: initial_normal,
                        water_depth: final_depth as f32,
                    };
                    work.cursor += 1;
                    if work.cursor % 32 == 0 && js_now() >= deadline {
                        break;
                    }
                }
                if work.cursor < length {
                    pending.work = Some(work);
                    break;
                }
                let tile = &mut self.tiles[index];
                let bytes = if let Some(mesh) = work.mesh {
                    let gpu = GpuMesh::upload_with_geometry(
                        &self.device,
                        &self.queue,
                        &mesh,
                        work.dynamic,
                        work.geometry.take(),
                    );
                    let bytes = gpu.bytes;
                    tile.gpu = Some(gpu);
                    bytes
                } else {
                    let gpu = tile.gpu.as_mut().unwrap();
                    let bytes = gpu.dynamic.len() * std::mem::size_of::<DynamicVertex>();
                    if let Some(indices) = work.indices {
                        gpu.update_edges(&self.queue, &indices, work.dynamic);
                    } else {
                        gpu.update_dynamic(&self.queue, work.dynamic);
                    }
                    bytes
                };
                tile.morph_source = self.cut_serial;
                self.metrics.uploads += 1;
                self.metrics.upload_bytes += bytes as u64;
                pending.cursor += 1;
                // Small edge jobs should share a frame; a completed upload no
                // longer forces the scheduler to leave the remaining budget idle.
                if js_now() >= deadline {
                    break;
                }
            }
            let elapsed = (js_now() - begin) * 1000.0;
            self.metrics.last_prepare_ms = elapsed;
            self.metrics.max_prepare_ms = self.metrics.max_prepare_ms.max(elapsed);
            if pending.cursor == pending.jobs.len() {
                if pending.instant
                    && !self.can_coarsen_instantly(
                        pending
                            .parents
                            .iter()
                            .copied()
                            .chain(pending.children.iter().copied())
                            .chain(pending.jobs.iter().filter_map(|(key, kind, _)| {
                                (*kind == WorkKind::Edge).then_some(*key)
                            })),
                        self.view_frame(js_now()),
                    )
                {
                    // Camera movement can expose a prepared group. Refine
                    // children already carry their complete source/target
                    // geometry, so their normal visible morph can start now.
                    pending.instant = false;
                    if pending.coarsening {
                        pending.jobs.extend(
                            pending
                                .children
                                .iter()
                                .copied()
                                .map(|key| (key, WorkKind::Coarsen, lod::parent(key).unwrap())),
                        );
                        self.pending = Some(pending);
                        return;
                    }
                }
                let start = f64::from(js_now() as f32);
                let epoch_start = if pending.instant {
                    start - MORPH_DURATION_SECONDS
                } else {
                    start
                };
                self.epochs[pending.epoch as usize % 32].start = epoch_start as f32;
                for (key, _, _) in &pending.jobs {
                    self.tiles[self.tile_lookup[key]].transition_start = epoch_start;
                }
                pending.started = Some(start);
                self.morph_start = start;
                if !pending.coarsening || pending.instant {
                    self.active_set = pending.proposed.clone();
                    self.metrics.cut_commits += 1;
                    if pending.coarsening {
                        self.metrics.instant_coarsen_commits += 1;
                    } else if pending.instant {
                        self.metrics.instant_refine_commits += 1;
                    }
                    self.cut_serial = self.cut_serial.wrapping_add(1);
                    self.cut_dirty = true;
                    self.sync_cut();
                    self.evict_cache();
                    return;
                }
            }
            self.pending = Some(pending);
        }

        fn render(&mut self, width: u32, height: u32) -> Result<(), String> {
            if width == 0 || height == 0 {
                return Ok(());
            }
            if width != self.config.width || height != self.config.height {
                self.config.width = width;
                self.config.height = height;
                self.surface.configure(&self.device, &self.config);
                self.depth = depth_texture(&self.device, width, height);
                self.cut_dirty = true;
            }
            let frame_begin = js_now();
            self.refresh_cut();
            let dir = self.direction();
            let time = js_now() as f32;
            let frame = self.view_frame(f64::from(time));
            let (eye, look, _, _) = frame;
            if self.cut_visible {
                // These bounds enclose the actual terrain/water vertices and
                // every morph offset. Keep the complete active partition for
                // stitching; only skip drawcalls outside this exact camera.
                self.draw_keys = self
                    .active_keys
                    .iter()
                    .copied()
                    .filter(|key| {
                        self.tile_lookup
                            .get(key)
                            .and_then(|&i| self.tiles[i].gpu.as_ref())
                            .is_some_and(|gpu| {
                                sphere_in_frustum(
                                    gpu.bound_center,
                                    gpu.bound_radius + gpu.morph_pad + 1e-6,
                                    frame,
                                    self.aspect(),
                                )
                            })
                    })
                    .collect();
            }
            let high = eye.as_vec3();
            let low = (eye - high.as_dvec3()).as_vec3();
            let rotation = Mat4::look_to_rh(Vec3::ZERO, look.as_vec3(), frame.3.as_vec3());
            let projection = Mat4::perspective_infinite_reverse_rh(
                45f32.to_radians(),
                width as f32 / height as f32,
                (self.altitude as f32 * 0.008).max(0.00000001),
            );
            let clips = std::array::from_fn(|i| TileClip {
                bounds: [self.epochs[i].id as f32, self.epochs[i].start, 0.0, 0.0],
                face_level: [0.0; 4],
            });
            let uniforms = Uniforms {
                view_projection: (projection * rotation).to_cols_array_2d(),
                eye: high
                    .extend(
                        (self.altitude
                            * self
                                .world
                                .as_ref()
                                .map_or(1_737_000.0, |world| world.config.radius_m))
                            as f32,
                    )
                    .to_array(),
                eye_low: low.extend(0.0).to_array(),
                light: inspection_light(dir, self.altitude).extend(time).to_array(),
                options: [
                    self.mode,
                    self.world
                        .as_ref()
                        .map_or(1_737_000.0, |world| world.config.radius_m)
                        as f32,
                    if self.show_rivers { 1.0 } else { 0.0 },
                    if self.show_human { 1.0 } else { 0.0 },
                ],
                cartography: cartography_seed(
                    self.world.as_ref().map_or(42, |world| world.config.seed),
                ),
                surface: [self.exaggeration as f32, 0.0, 0.0, 0.0],
                noise: noise_frame(
                    eye,
                    self.world
                        .as_ref()
                        .map_or(1_737_000.0, |world| world.config.radius_m),
                ),
                clips,
            };
            self.queue
                .write_buffer(&self.uniform, 0, bytemuck::bytes_of(&uniforms));
            let output = match self.surface.get_current_texture() {
                Ok(output) => output,
                Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                    self.surface.configure(&self.device, &self.config);
                    return Ok(());
                }
                Err(wgpu::SurfaceError::Timeout) => return Ok(()),
                Err(error) => return Err(error.to_string()),
            };
            let view = output.texture.create_view(&Default::default());
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("world frame"),
                });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("planet"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.015,
                                g: 0.025,
                                b: 0.038,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &self.depth,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    occlusion_query_set: None,
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.binding, &[]);
                for mesh in self
                    .globe
                    .iter()
                    .filter(|_| !self.cut_visible)
                    .chain(
                        self.draw_keys
                            .iter()
                            .filter(|_| self.cut_visible)
                            .filter_map(|key| {
                                self.tile_lookup
                                    .get(key)
                                    .and_then(|&i| self.tiles[i].gpu.as_ref())
                            }),
                    )
                    .chain(self.objects.iter())
                {
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_vertex_buffer(1, mesh.dynamic_buffer.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.count, 0, 0..1);
                }
            }
            self.queue.submit(Some(encoder.finish()));
            output.present();
            self.metrics.last_frame_time = time;
            self.metrics.last_render_ms = (js_now() - frame_begin) * 1000.0;
            self.metrics.max_render_ms =
                self.metrics.max_render_ms.max(self.metrics.last_render_ms);
            Ok(())
        }
    }

    #[wasm_bindgen]
    pub async fn start(canvas_id: &str, force_webgl: bool) -> Result<String, JsValue> {
        console_error_panic_hook::set_once();
        let document = web_sys::window()
            .ok_or("Window absent")?
            .document()
            .ok_or("Document absent")?;
        let canvas: web_sys::HtmlCanvasElement = document
            .get_element_by_id(canvas_id)
            .ok_or("Canvas absent")?
            .dyn_into()?;
        let has_webgpu = !force_webgl && js_sys_gpu_exists();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: if has_webgpu {
                wgpu::Backends::BROWSER_WEBGPU
            } else {
                wgpu::Backends::GL
            },
            ..Default::default()
        });
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        // The JS startup watchdog replaces a canvas before trying another
        // backend. An abandoned async attempt must not allocate a device or
        // overwrite the renderer that its replacement has already started.
        if !canvas.is_connected() {
            return Err(JsValue::from_str("Tentative de démarrage annulée"));
        }
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("World Builder"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        if !canvas.is_connected() {
            return Err(JsValue::from_str("Tentative de démarrage annulée"));
        }
        device.on_uncaptured_error(std::sync::Arc::new(|error: wgpu::Error| {
            web_sys::console::error_1(&JsValue::from_str(&format!("GPU validation: {error}")));
        }));
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(capabilities.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: canvas.width().max(1),
            height: canvas.height().max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        let depth = depth_texture(&device, config.width, config.height);
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera"),
            contents: bytemuck::bytes_of(&Uniforms::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("planet layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("planet shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source().into()),
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 9 => Float32x4, 10 => Float32x4, 11 => Float32x3];
        let dynamic_attributes =
            wgpu::vertex_attr_array![5 => Float32x4, 6 => Float32x3, 7 => Float32x3, 8=>Float32];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("planet"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<StaticVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attributes,
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<DynamicVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &dynamic_attributes,
                    },
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let info = adapter.get_info();
        let label = format!(
            "{:?} · {}",
            info.backend,
            if info.name.is_empty() {
                "GPU navigateur"
            } else {
                &info.name
            }
        );
        if !canvas.is_connected() {
            return Err(JsValue::from_str("Tentative de démarrage annulée"));
        }
        RENDERER.with(|state| {
            *state.borrow_mut() = Some(Renderer {
                surface,
                device,
                queue,
                config,
                pipeline,
                uniform,
                binding,
                depth,
                world: None,
                globe: None,
                base_surfaces: [Vec::new(), Vec::new()],
                tiles: Vec::new(),
                pending: None,
                surface_update: None,
                surface_field: None,
                surface_version: 0,
                surface_priority_until: 0.0,
                cut_dirty: true,
                cut_visible: false,
                signature: None,
                last_camera_change: 0.0,
                cut_serial: 1,
                morph_start: 0.0,
                epochs: [MorphEpoch::default(); 32],
                next_epoch: 0,
                neighbor_serial: u64::MAX,
                neighbor_cache: HashMap::new(),
                desired: Vec::new(),
                wanted: 0,
                draw_keys: Vec::new(),
                tick: 0,
                metrics: RenderMetrics::default(),
                tile_lookup: HashMap::new(),
                active_keys: Vec::new(),
                active_set: HashSet::new(),
                rivers: RiverLayer::default(),
                human: HumanLayer::default(),
                objects: None,
                show_rivers: true,
                show_human: true,
                yaw: 0.65,
                pitch: 0.35,
                view_tilt: 0.0,
                altitude: 2.2,
                exaggeration: 1.0,
                mode: 0.0,
            })
        });
        Ok(label)
    }

    // Avoid requiring unstable web-sys WebGPU bindings merely to choose the backend.
    #[wasm_bindgen(
        inline_js = "export function js_sys_gpu_exists() { return !!navigator.gpu; } export function js_now() { return performance.now() * 0.001; }"
    )]
    extern "C" {
        fn js_sys_gpu_exists() -> bool;
        fn js_now() -> f64;
    }

    #[wasm_bindgen]
    pub fn set_world(json: &str) -> Result<(), JsValue> {
        let world: World =
            serde_json::from_str(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        RENDERER.with(|state| {
            let mut state = state.borrow_mut();
            let renderer = state.as_mut().ok_or("Visualiseur absent")?;
            renderer.surface_field = None;
            renderer.surface_version = 0;
            renderer.world = Some(world);
            renderer.globe = None;
            renderer.base_surfaces = [Vec::new(), Vec::new()];
            renderer.metrics = RenderMetrics::default();
            renderer
                .install_roots()
                .map_err(|e| JsValue::from_str(&e))?;
            renderer.objects = None;
            Ok(())
        })
    }

    #[wasm_bindgen]
    pub fn prepare_tile_binary(
        bytes: &[u8],
        radius_m: f64,
        exaggeration: f64,
    ) -> Result<Vec<u8>, JsValue> {
        let tile = world_core::tile_wire::decode_tile(bytes)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        prepare_tile(from_core_tile(tile), radius_m, exaggeration)
            .map_err(|e| JsValue::from_str(&e))
    }
    #[wasm_bindgen]
    pub fn prepare_tile_json(
        json: &str,
        radius_m: f64,
        exaggeration: f64,
    ) -> Result<Vec<u8>, JsValue> {
        let tile = decode_json_tile(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        prepare_tile(tile, radius_m, exaggeration).map_err(|e| JsValue::from_str(&e))
    }
    #[wasm_bindgen]
    pub fn set_worker_world(json: &str) -> Result<(), JsValue> {
        let world = serde_json::from_str::<World>(json)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        WORKER_WORLD.with(|state| *state.borrow_mut() = Some(world));
        Ok(())
    }

    #[derive(Deserialize)]
    struct FieldContext {
        world_id: String,
        revision: u64,
        radius_m: f64,
        #[serde(default)]
        kind: world_core::hybrid::SurfaceKind,
    }
    #[derive(Deserialize)]
    struct PatchContext {
        world_id: String,
        face: u8,
        #[serde(default)]
        level: u8,
        x: i32,
        y: i32,
    }
    fn parsed_field(json: &str) -> Result<Option<world_core::hybrid::HybridField>, JsValue> {
        if json.is_empty() {
            return Ok(None);
        }
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|error| JsValue::from_str(&error.to_string()))?;
        if value.get("patches").is_some() {
            return world_core::hybrid::HybridField::from_json(json)
                .map(Some)
                .map_err(|error| JsValue::from_str(&error.to_string()));
        }
        let context: FieldContext =
            serde_json::from_value(value).map_err(|error| JsValue::from_str(&error.to_string()))?;
        FIELD_PATCHES.with(|patches| {
            let prefix = format!("{}/", context.world_id);
            let patches = patches
                .borrow()
                .iter()
                .filter(|(key, _)| key.starts_with(&prefix))
                .map(|(_, patch)| patch.clone())
                .collect();
            world_core::hybrid::HybridField::with_kind(
                context.world_id,
                context.revision,
                context.radius_m,
                patches,
                context.kind,
            )
            .map(Some)
            .map_err(|error| JsValue::from_str(&error.to_string()))
        })
    }

    #[wasm_bindgen]
    pub fn register_hybrid_patch(meta_json: &str, values: &[f32]) -> Result<(), JsValue> {
        let meta: PatchContext = serde_json::from_str(meta_json)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        if meta.face >= 6
            || values.len() != 258 * 258
            || values.iter().any(|value| !value.is_finite())
        {
            return Err(JsValue::from_str("Patch hybride invalide"));
        }
        let key = format!(
            "{}/{}/{}/{}/{}",
            meta.world_id, meta.face, meta.level, meta.x, meta.y
        );
        FIELD_PATCHES.with(|patches| {
            let mut patches = patches.borrow_mut();
            if patches.len() >= 96 && !patches.contains_key(&key) {
                return Err(JsValue::from_str("Cache de patches plein"));
            }
            patches.insert(
                key,
                world_core::hybrid::HybridPatch {
                    face: meta.face,
                    level: meta.level,
                    x: meta.x,
                    y: meta.y,
                    values: values.to_vec(),
                },
            );
            Ok(())
        })
    }

    #[wasm_bindgen]
    pub fn remove_hybrid_patch(meta_json: &str) -> Result<(), JsValue> {
        let meta: PatchContext = serde_json::from_str(meta_json)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        FIELD_PATCHES.with(|patches| {
            patches.borrow_mut().remove(&format!(
                "{}/{}/{}/{}/{}",
                meta.world_id, meta.face, meta.level, meta.x, meta.y
            ));
        });
        Ok(())
    }

    #[wasm_bindgen]
    pub fn clear_hybrid_patch_registry() {
        FIELD_PATCHES.with(|patches| patches.borrow_mut().clear());
    }

    #[wasm_bindgen]
    pub fn clear_hybrid_patches() {
        clear_hybrid_patch_registry();
        WORKER_FIELD.with(|state| *state.borrow_mut() = None);
        WORKER_SNAPSHOTS.with(|state| state.borrow_mut().procedural());
    }

    #[wasm_bindgen]
    pub fn set_worker_hybrid_field(json: &str) -> Result<(), JsValue> {
        let field = parsed_field(json)?;
        let field =
            WORKER_SNAPSHOTS.with(|state| field.map(|field| state.borrow_mut().prepare(field)));
        WORKER_FIELD.with(|state| *state.borrow_mut() = field);
        Ok(())
    }

    /// Restore/commit selects a stored revision without consulting patch data.
    #[wasm_bindgen]
    pub fn select_worker_hybrid_field(json: &str) -> Result<(), JsValue> {
        if json.is_empty() {
            WORKER_FIELD.with(|state| *state.borrow_mut() = None);
            WORKER_SNAPSHOTS.with(|state| state.borrow_mut().procedural());
            return Ok(());
        }
        let context: FieldContext =
            serde_json::from_str(json).map_err(|error| JsValue::from_str(&error.to_string()))?;
        let key = format!(
            "{}/{}/{}",
            context.world_id,
            context.revision,
            context.radius_m.to_bits()
        );
        let field = WORKER_SNAPSHOTS
            .with(|state| state.borrow_mut().select(&key))
            .ok_or("Snapshot hybride absent")?;
        WORKER_FIELD.with(|state| *state.borrow_mut() = Some(field));
        Ok(())
    }

    #[wasm_bindgen]
    pub fn prepare_tile_binary_hybrid(
        bytes: &[u8],
        radius_m: f64,
        exaggeration: f64,
    ) -> Result<Vec<u8>, JsValue> {
        let tile = world_core::tile_wire::decode_tile(bytes)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        WORKER_FIELD.with(|field| {
            prepare_hybrid_tile(
                from_core_tile(tile),
                radius_m,
                exaggeration,
                field.borrow().as_deref(),
            )
            .map_err(|error| JsValue::from_str(&error))
        })
    }

    #[wasm_bindgen]
    pub fn prepare_tile_json_hybrid(
        json: &str,
        radius_m: f64,
        exaggeration: f64,
    ) -> Result<Vec<u8>, JsValue> {
        let tile = decode_json_tile(json).map_err(|error| JsValue::from_str(&error.to_string()))?;
        WORKER_FIELD.with(|field| {
            prepare_hybrid_tile(tile, radius_m, exaggeration, field.borrow().as_deref())
                .map_err(|error| JsValue::from_str(&error))
        })
    }

    #[wasm_bindgen]
    pub fn prepare_root_tile_hybrid(
        face: u8,
        radius_m: f64,
        exaggeration: f64,
    ) -> Result<Vec<u8>, JsValue> {
        WORKER_WORLD.with(|world| {
            let world = world.borrow();
            let world = world.as_ref().ok_or("Atlas worker absent")?;
            let mut roots = root_tiles(world).map_err(|error| JsValue::from_str(&error))?;
            if face >= 6 {
                return Err(JsValue::from_str("Face invalide"));
            }
            let tile = roots.swap_remove(face as usize);
            WORKER_FIELD.with(|field| {
                prepare_hybrid_tile(tile, radius_m, exaggeration, field.borrow().as_deref())
                    .map_err(|error| JsValue::from_str(&error))
            })
        })
    }

    /// Returns the exact immutable partition to reprepare. A busy LOD morph is
    /// allowed to finish; beginning a field transaction never cancels its epoch.
    #[wasm_bindgen]
    pub fn begin_surface_update(version: u32, field_json: &str) -> Result<String, JsValue> {
        let field = parsed_field(field_json)?;
        RENDERER.with(|state| {
            let mut state = state.borrow_mut();
            let r = state.as_mut().ok_or("Visualiseur absent")?;
            if r.world.is_none() {
                return Err(JsValue::from_str("Monde absent"));
            }
            if let Some(update) = &r.surface_update {
                if update.version == version {
                    return Ok(serde_json::json!({"state":"preparing", "version":version,
                        "keys":update.keys})
                    .to_string());
                }
                r.cancel_surface_update();
            }
            let time = js_now();
            if r.pending.is_some()
                || r.active_keys.iter().any(|key| {
                    r.tiles[r.tile_lookup[key]].transition_start + MORPH_DURATION_SECONDS > time
                })
            {
                r.surface_priority_until = time + 0.25;
                return Ok(serde_json::json!({"state":"busy", "version":version}).to_string());
            }
            let Some(epoch) = next_morph_epoch(r.next_epoch, time, &r.epochs) else {
                return Ok(serde_json::json!({"state":"busy", "version":version}).to_string());
            };
            let mut changed = HashSet::new();
            for tile in &r.tiles {
                if tile
                    .tile
                    .vertices
                    .iter()
                    .zip(&tile.base_heights)
                    .any(|(sample, height)| {
                        let target = *height
                            + field
                                .as_ref()
                                .map_or(0.0, |field| hybrid_delta(field, sample, *height));
                        (target - sample.height_m).abs() > 1e-7
                    })
                {
                    changed.insert(tile.key);
                }
            }
            let affected = surface_refresh_keys(&r.active_set, &changed);
            // Reserve the complete batch before freezing LOD. Committed tags
            // and inactive changed cache entries are untouched until commit.
            let reservations: HashMap<_, _> = affected
                .iter()
                .map(|key| {
                    let tile = &r.tiles[r.tile_lookup[key]];
                    let geometry_bytes = tile.gpu.as_ref().map_or(0, |gpu| gpu.bytes);
                    let mesh_cpu = tile.gpu.as_ref().map_or(0, |gpu| {
                        gpu.geometry.len() * std::mem::size_of::<Vertex>() + gpu.count as usize * 4
                    });
                    let gpu_cpu = tile.gpu.as_ref().map_or(0, GpuMesh::cpu_bytes);
                    let base = tile.cpu_base
                        + tile.base_heights.capacity() * 8
                        + tile.tile.morph_offsets.capacity() * 24
                        + std::mem::size_of::<CachedTile>();
                    (
                        *key,
                        (
                            surface_cpu_reservation(base, mesh_cpu, gpu_cpu),
                            geometry_bytes,
                        ),
                    )
                })
                .collect();
            let field_bytes = field.as_ref().map_or(0, |field| {
                field
                    .patches()
                    .iter()
                    .map(|patch| patch.values.capacity() * 4)
                    .sum()
            });
            // A single decoder/upload plus both shared-normal hash tables.
            // Roots are at most129² samples, so32MiB also covers wire/PackedSample
            // overlap and one temporary Vec<StaticVertex> at full root size.
            let scratch = if reservations.is_empty() {
                0
            } else {
                32 * 1024 * 1024
            };
            let reserve = reservations
                .values()
                .fold((field_bytes + scratch, 0usize), |sum, bytes| {
                    (sum.0 + bytes.0, sum.1 + bytes.1)
                });
            r.evict_for_capacity(reserve, &affected);
            if !within_surface_budget(
                r.cache_bytes(),
                reserve,
                (TERRAIN_MEMORY_LIMIT, TERRAIN_MEMORY_LIMIT),
            ) {
                return Ok(serde_json::json!({"state":"memory", "version":version,
                    "required_cpu_bytes":reserve.0,"required_gpu_bytes":reserve.1,
                    "current_cpu_bytes":r.cache_bytes().0,"current_gpu_bytes":r.cache_bytes().1,
                    "affected_tiles":affected.len(),"scratch_cpu_bytes":scratch})
                .to_string());
            }
            let mut keys: Vec<_> = affected.into_iter().collect();
            keys.sort_by_key(|key| (key.level, key.face, key.x, key.y)); // Coarse first, across faces.
            r.next_epoch = epoch;
            r.epochs[epoch as usize % 32] = MorphEpoch {
                id: epoch,
                start: 1e9,
            };
            r.surface_update = Some(SurfaceUpdate {
                version,
                field,
                keys: keys.clone(),
                packets: HashMap::new(),
                staged: HashMap::new(),
                shared: HashMap::new(),
                epoch,
                invalidated: changed,
                reservations,
            });
            Ok(
                serde_json::json!({"state":"preparing", "version":version, "keys":keys})
                    .to_string(),
            )
        })
    }

    #[wasm_bindgen]
    pub fn cancel_surface_update() {
        RENDERER.with(|state| {
            if let Some(r) = state.borrow_mut().as_mut() {
                r.cancel_surface_update();
            }
        });
    }

    #[wasm_bindgen]
    pub fn stage_surface_tile(bytes: &[u8], version: u32) -> Result<(), JsValue> {
        let packet = decode_prepared_tile(bytes).map_err(|error| JsValue::from_str(&error))?;
        RENDERER.with(|state| {
            let mut state = state.borrow_mut();
            let r = state.as_mut().ok_or("Visualiseur absent")?;
            let update = r.surface_update.as_mut().ok_or("Mise à jour absente")?;
            if update.version != version
                || !update.keys.contains(&packet.tile.key)
                || packet.radius != r.world.as_ref().ok_or("Monde absent")?.config.radius_m
                || packet.exaggeration != r.exaggeration
            {
                return Err(JsValue::from_str("Préparation de surface périmée"));
            }
            let key = packet.tile.key;
            let expected = update.reservations.get(&key).ok_or("Réservation absente")?;
            let mesh_cpu = packet.mesh.vertices.capacity() * std::mem::size_of::<Vertex>()
                + packet.mesh.indices.capacity() * 4;
            let base = tile_cpu_base(&packet.tile, &packet.borders)
                + packet.base_heights.capacity() * 8
                + packet.tile.morph_offsets.capacity() * 24;
            let gpu_cpu = packet.mesh.vertices.len()
                * (std::mem::size_of::<SurfaceVertex>() + std::mem::size_of::<DynamicVertex>());
            let cpu = surface_cpu_reservation(base, mesh_cpu, gpu_cpu);
            let gpu = packet.mesh.vertices.len()
                * (std::mem::size_of::<StaticVertex>() + std::mem::size_of::<DynamicVertex>())
                + packet.mesh.indices.len() * 4;
            if cpu > expected.0 || gpu > expected.1 {
                return Err(JsValue::from_str(
                    "La surface dépasse sa réservation mémoire",
                ));
            }
            if !update.staged.contains_key(&key) {
                update.packets.insert(key, packet);
            }

            Ok(())
        })
    }

    #[wasm_bindgen]
    pub fn install_prepared_tile_versioned(bytes: &[u8], version: u32) -> Result<String, JsValue> {
        let packet = decode_prepared_tile(bytes).map_err(|error| JsValue::from_str(&error))?;
        let valid = RENDERER.with(|state| {
            state.borrow().as_ref().is_some_and(|r| {
                r.surface_update
                    .as_ref()
                    .map_or(r.surface_version, |update| update.version)
                    == version
            })
        });
        if !valid {
            return Err(JsValue::from_str("Champ de terrain périmé"));
        }
        install_prepared(packet, Some(version))
    }

    #[wasm_bindgen]
    pub fn install_prepared_tile(bytes: &[u8]) -> Result<String, JsValue> {
        let prepared = decode_prepared_tile(bytes).map_err(|e| JsValue::from_str(&e))?;
        install_prepared(prepared, None)
    }
    #[wasm_bindgen]
    pub fn set_tile(json: &str) -> Result<String, JsValue> {
        let tile = decode_json_tile(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        install_tile(tile)
    }
    #[wasm_bindgen]
    pub fn add_tile_binary(bytes: &[u8]) -> Result<String, JsValue> {
        let tile = world_core::tile_wire::decode_tile(bytes)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        install_tile(from_core_tile(tile))
    }
    fn install_tile(tile: Tile) -> Result<String, JsValue> {
        let (radius, exaggeration) = RENDERER
            .with(|state| {
                state.borrow().as_ref().and_then(|r| {
                    r.world
                        .as_ref()
                        .map(|w| (w.config.radius_m, r.exaggeration))
                })
            })
            .ok_or("Monde absent")?;
        let bytes = prepare_tile(tile, radius, exaggeration).map_err(|e| JsValue::from_str(&e))?;
        install_prepared_tile(&bytes)
    }
    fn install_prepared(
        prepared: PreparedTile,
        surface_version: Option<u32>,
    ) -> Result<String, JsValue> {
        RENDERER.with(|state| {
            let mut state = state.borrow_mut();
            let r = state.as_mut().ok_or("Visualiseur absent")?;
            let world = r.world.as_ref().ok_or("Monde absent")?;
            if prepared.radius != world.config.radius_m || prepared.exaggeration != r.exaggeration {
                return Err(JsValue::from_str("Préparation périmée"));
            }
            if world.render_grid.as_deref() == Some("vertices")
                && !valid_tile_vertex_count(
                    prepared.tile.key,
                    prepared.tile.vertices.len(),
                    world.mesh_resolution(),
                )
            {
                return Err(JsValue::from_str(
                    "La tuile doit utiliser la grille canonique de son niveau",
                ));
            }
            let key = prepared.tile.key;
            if !r.tile_lookup.contains_key(&key) {
                let bounds = mesh_bounds(&prepared.mesh);
                let cpu_base = prepared.tile.vertices.capacity() * std::mem::size_of::<Sample>()
                    + prepared
                        .tile
                        .vertices
                        .iter()
                        .map(|s| s.biome.capacity())
                        .sum::<usize>()
                    + prepared.tile.indices.capacity() * 4
                    + prepared.borders.capacity() * std::mem::size_of::<BorderNormal>();
                r.tiles.push(CachedTile {
                    key,
                    base_heights: prepared.base_heights,
                    surface_version: surface_version.unwrap_or(r.surface_version),
                    tile: prepared.tile,
                    prepared: Some(prepared.mesh),
                    borders: prepared.borders,
                    gpu: None,
                    last_used: r.tick,
                    cpu_base,
                    bounds,
                    morph_source: 0,
                    transition_start: -1e9,
                });
                r.rebuild_lookup();
                r.cut_dirty = true;
                r.metrics.mesh_builds += 1;
                r.evict_cache();
            }
            serde_json::to_string(&r.tiles.iter().map(|tile| tile.key).collect::<Vec<_>>())
                .map_err(|e| JsValue::from_str(&e.to_string()))
        })
    }
    #[wasm_bindgen]
    pub fn clear_tiles() -> Result<(), JsValue> {
        RENDERER.with(|state| {
            if let Some(r) = state.borrow_mut().as_mut() {
                r.install_roots()
                    .map_err(|error| JsValue::from_str(&error))?;
                r.cut_serial = r.cut_serial.wrapping_add(1);
            }
            Ok(())
        })
    }

    fn rebuild_objects(r: &mut Renderer) {
        let Some(world) = &r.world else {
            return;
        };
        let mesh = object_mesh(&r.rivers, &r.human, world.config.radius_m, r.exaggeration);
        r.objects = if mesh.indices.is_empty() {
            None
        } else {
            Some(GpuMesh::upload(&r.device, &r.queue, &mesh))
        };
    }

    #[wasm_bindgen]
    pub fn set_rivers(json: &str) -> Result<(), JsValue> {
        let rivers: RiverLayer =
            serde_json::from_str(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        RENDERER.with(|state| {
            let mut state = state.borrow_mut();
            let r = state.as_mut().ok_or("Visualiseur absent")?;
            r.rivers = rivers;
            rebuild_objects(r);
            Ok(())
        })
    }

    #[wasm_bindgen]
    pub fn set_human(json: &str) -> Result<(), JsValue> {
        let human: HumanLayer =
            serde_json::from_str(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        RENDERER.with(|state| {
            let mut state = state.borrow_mut();
            let r = state.as_mut().ok_or("Visualiseur absent")?;
            r.human = human;
            rebuild_objects(r);
            Ok(())
        })
    }

    #[wasm_bindgen]
    pub fn set_overlays(rivers: bool, human: bool) {
        RENDERER.with(|state| {
            if let Some(r) = state.borrow_mut().as_mut() {
                r.show_rivers = rivers;
                r.show_human = human;
            }
        });
    }

    #[wasm_bindgen]
    pub fn render(width: u32, height: u32) -> Result<(), JsValue> {
        RENDERER.with(|state| {
            // Browser drivers/instrumentation may read metrics synchronously.
            // Never hold a RefCell borrow across their GPU calls.
            let renderer = state.borrow_mut().take();
            let Some(mut renderer) = renderer else {
                return Ok(());
            };
            let result = renderer
                .render(width, height)
                .map_err(|e| JsValue::from_str(&e));
            *state.borrow_mut() = Some(renderer);
            result
        })
    }

    #[wasm_bindgen]
    pub fn rotate(dx: f64, dy: f64) {
        RENDERER.with(|state| {
            if let Some(r) = state.borrow_mut().as_mut() {
                let speed = 0.002 * r.altitude.clamp(0.000001, 2.0);
                r.yaw -= dx * speed;
                r.pitch = (r.pitch + dy * speed)
                    .clamp(-std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2);
            }
        });
    }

    #[wasm_bindgen]
    pub fn zoom(delta: f64) {
        RENDERER.with(|state| {
            if let Some(r) = state.borrow_mut().as_mut() {
                let radius = r.world.as_ref().map_or(1_737_000.0, |w| w.config.radius_m);
                r.altitude = (r.altitude * (delta * 0.0015).exp()).clamp(5.0 / radius, 7.0);
            }
        });
    }

    #[wasm_bindgen]
    pub fn reset_view() {
        RENDERER.with(|state| {
            if let Some(r) = state.borrow_mut().as_mut() {
                r.altitude = 2.2;
                r.pitch = 0.35;
                r.yaw = 0.65;
                r.view_tilt = 0.0;
            }
        });
    }

    #[wasm_bindgen]
    pub fn fly_to(direction_json: &str, altitude_m: f64) -> Result<(), JsValue> {
        let direction: [f64; 3] = serde_json::from_str(direction_json)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        let direction = DVec3::from_array(direction);
        if !direction.is_finite() || direction.length_squared() <= 1e-20 || !altitude_m.is_finite()
        {
            return Err(JsValue::from_str("Destination invalide"));
        }
        let direction = direction.normalize();
        RENDERER.with(|state| {
            let mut state = state.borrow_mut();
            let renderer = state.as_mut().ok_or("Visualiseur absent")?;
            let radius = renderer
                .world
                .as_ref()
                .ok_or("Monde absent")?
                .config
                .radius_m;
            renderer.yaw = direction.x.atan2(direction.z);
            renderer.pitch = direction
                .y
                .asin()
                .clamp(-std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2);
            renderer.altitude = (altitude_m / radius).clamp(5.0 / radius, 7.0);
            Ok(())
        })
    }

    #[wasm_bindgen]
    pub fn set_view_tilt(degrees: f64) {
        RENDERER.with(|state| {
            if let Some(r) = state.borrow_mut().as_mut() {
                r.view_tilt = degrees.clamp(0.0, 35.0).to_radians();
            }
        });
    }

    #[wasm_bindgen]
    pub fn set_mode(mode: u32) {
        RENDERER.with(|state| {
            if let Some(r) = state.borrow_mut().as_mut() {
                r.mode = mode.min(5) as f32;
            }
        });
    }

    #[wasm_bindgen]
    pub fn set_exaggeration(exaggeration: f64) -> Result<(), JsValue> {
        RENDERER.with(|state| {
            let mut state = state.borrow_mut();
            let r = state.as_mut().ok_or("Visualiseur absent")?;
            r.exaggeration = exaggeration.clamp(1.0, 30.0);
            r.install_roots().map_err(|e| JsValue::from_str(&e))?;
            rebuild_objects(r);
            Ok(())
        })
    }

    #[wasm_bindgen]
    pub fn prediction_requests(direction_json: &str, altitude_m: f64) -> Result<String, JsValue> {
        let direction: [f64; 3] =
            serde_json::from_str(direction_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let direction = DVec3::from_array(direction).normalize();
        RENDERER.with(|state| {
            let state = state.borrow();
            let r = state.as_ref().ok_or("Visualiseur absent")?;
            let radius = r.world.as_ref().ok_or("Monde absent")?.config.radius_m;
            serde_json::to_string(
                &r.requests_at(direction, (altitude_m / radius).max(5.0 / radius)),
            )
            .map_err(|e| JsValue::from_str(&e.to_string()))
        })
    }

    /// On-demand geometry evidence for a visible facet, including its current
    /// morph and coarse-edge constraints. This does no work during rendering.
    #[wasm_bindgen]
    pub fn surface_probe(direction_json: &str) -> Result<String, JsValue> {
        let direction: [f64; 3] = serde_json::from_str(direction_json)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        let direction = DVec3::from_array(direction);
        if !direction.is_finite() || direction.length_squared() < 1e-20 {
            return Err(JsValue::from_str("Direction de sonde invalide"));
        }
        let direction = direction.normalize();
        let result: Result<String, String> = RENDERER.with(|state| {
            let state = state.borrow();
            let r = state.as_ref().ok_or("Visualiseur absent")?;
            let world = r.world.as_ref().ok_or("Monde absent")?;
            let radius = world.config.radius_m;
            let metres = radius / r.exaggeration;
            let time = f64::from(r.metrics.last_frame_time);
            let (face, u, v) = face_uv(direction);
            let key = lod::covering_key(&r.active_set, face as u8, u, v)
                .ok_or("Direction hors de la coupe affichée")?;
            let tile = &r.tiles[r.tile_lookup[&key]];
            let gpu = tile.gpu.as_ref().ok_or("Géométrie absente")?;
            let side = (tile.tile.vertices.len() as f64).sqrt() as usize;
            let attempted = RefCell::new(Vec::new());
            let hit = surface_query(direction, key, side, |i| {
                attempted.borrow_mut().push(i);
                let g = gpu.geometry[i];
                let d = &gpu.dynamic[i];
                let progress = epoch_progress(d.morph[1], time, &r.epochs);
                (
                    g.direction * (g.radius + r.dynamic_delta(d, time, false)),
                    Vec3::from_array(d.parent_normal)
                        .lerp(Vec3::from_array(d.shading_normal), progress as f32)
                        .normalize_or_zero()
                        .to_array(),
                )
            })
            .ok_or("Intersection avec le triangle absent")?;
            let attempted = attempted.into_inner();
            let selected = &attempted[attempted.len() - 3..];
            let vertices: Vec<_> = selected
                .iter()
                .map(|&i| {
                    let sample = &tile.tile.vertices[i];
                    let g = gpu.geometry[i];
                    let d = &gpu.dynamic[i];
                    let progress = epoch_progress(d.morph[1], time, &r.epochs);
                    let delta = r.dynamic_delta(d, time, false);
                    let neighbors = border_neighbors(key, i, side, &r.active_set);
                    let stitch = r.stitch_target(key, i, &r.active_set, time);
                    let normal = Vec3::from_array(d.parent_normal)
                        .lerp(Vec3::from_array(d.shading_normal), progress as f32).normalize_or_zero();
                    let slope = physical_slope_degrees(normal, g.direction, sample.height_m, radius, r.exaggeration);
                    serde_json::json!({
                        "index": i, "direction": sample.direction,
                        "canonical_height_m": sample.height_m,
                        "procedural_height_m": tile.base_heights[i],
                        "learned_delta_m": sample.height_m - tile.base_heights[i],
                        "base_height_m": (g.radius - 1.0) * metres,
                        "displayed_height_m": (g.radius + delta - 1.0) * metres,
                        "current_delta_m": delta * metres,
                        "initial_delta_m": d.morph[0] as f64 * metres,
                        "final_delta_m": d.morph[3] as f64 * metres,
                        "morph_progress": progress,
                        "water_level_m": sample.water_level_m,
                        "water_body_id": sample.water_body_id,
                        "water_depth_m": d.morph[2] as f64 * (1.0-progress) + d.water_depth as f64 * progress,
                        "initial_normal": d.parent_normal, "final_normal": d.shading_normal,
                        "neighbors": neighbors,
                        "stitch_height_m": stitch.map(|target| (target.0-1.0) * metres),
                        "physical_slope_degrees": slope,
                        "slope_cover_tree_snow_rock": slope_cover(slope),
                    })
                })
                .collect();
            let water = r.cached_surface(key, direction, time, false);
            Ok(serde_json::json!({
                "direction": direction.to_array(), "key": key, "time_s": time,
                "exaggeration": r.exaggeration,
                "displayed_height_m": (hit.0-1.0) * metres, "normal": hit.1,
                "sea_height_m": water.map(|s| (s.2-1.0) * metres),
                "sea_depth_m": water.map(|s| s.3),
                "triangle": vertices,
            })
            .to_string())
        });
        result.map_err(|error| JsValue::from_str(&error))
    }

    /// Largest edge displacements and LOD gaps; bounded output, no frame work.
    #[wasm_bindgen]
    pub fn surface_edge_audit() -> String {
        RENDERER.with(|state| {
            let state = state.borrow();
            let Some(r) = state.as_ref() else { return "{}".into(); };
            let Some(world) = &r.world else { return "{}".into(); };
            let time = f64::from(r.metrics.last_frame_time);
            let metres = world.config.radius_m/r.exaggeration;
            let frame = r.view_frame(time);
            let mut largest: Vec<(bool, f64, serde_json::Value)> = Vec::new();
            let (mut maximum_gap, mut count, mut visible_count) = (0u8, 0usize, 0usize);
            let mut worst_pair = None;
            for key in &r.active_keys {
                for neighbor in tile_neighbors(*key, &r.active_set) {
                    let gap = key.level.abs_diff(neighbor.level);
                    if gap > maximum_gap { maximum_gap = gap; worst_pair = Some((*key,neighbor)); }
                }
                let tile = &r.tiles[r.tile_lookup[key]];
                let Some(gpu) = &tile.gpu else { continue; };
                let side = (tile.tile.vertices.len() as f64).sqrt() as usize;
                for border in &tile.borders {
                    let i = border.index as usize;
                    let (g,d,s) = (gpu.geometry[i], &gpu.dynamic[i], &tile.tile.vertices[i]);
                    let delta = r.dynamic_delta(d,time,false);
                    let height = (g.radius+delta-1.0)*metres;
                    let lift = height-s.height_m;
                    let visible = sphere_in_frustum(g.direction*(g.radius+delta),0.0,frame,r.aspect());
                    count += 1; visible_count += usize::from(visible);
                    if largest.len()==8 && (visible, lift.abs()) <= (largest[7].0,largest[7].1) { continue; }
                    largest.push((visible,lift.abs(),serde_json::json!({
                        "key":key,"index":i,"direction":s.direction,"visible":visible,
                        "canonical_height_m":s.height_m,"displayed_height_m":height,"lift_m":lift,
                        "current_delta_m":delta*metres,"final_delta_m":d.morph[3] as f64*metres,
                        "neighbors":border_neighbors(*key,i,side,&r.active_set),
                    })));
                    largest.sort_by(|a,b| b.0.cmp(&a.0).then(b.1.total_cmp(&a.1)));
                    largest.truncate(8);
                }
            }
            serde_json::json!({"time_s":time,"max_neighbor_lod_gap":maximum_gap,"worst_pair":worst_pair,
                "active_tiles":r.active_keys.len(),"border_vertices":count,"visible_border_vertices":visible_count,
                "largest_lifts":largest.into_iter().map(|record|record.2).collect::<Vec<_>>()}).to_string()
        })
    }

    #[wasm_bindgen]
    pub fn view_info() -> String {
        RENDERER.with(|state| {
            let Ok(state) = state.try_borrow() else { return VIEW_SNAPSHOT.with(|snapshot| snapshot.borrow().clone()); };
            let Some(r) = state.as_ref() else { return VIEW_SNAPSHOT.with(|snapshot| snapshot.borrow().clone()); };
            let (face, u, v) = face_uv(r.direction());
            let radius = r.world.as_ref().map_or(1_737_000.0, |w| w.config.radius_m);
            let level = u32::from(r.detail_level());
            let count = 1u32 << level;
            let x = (((u + 1.0) * 0.5 * count as f64) as u32).min(count - 1);
            let y = (((v + 1.0) * 0.5 * count as f64) as u32).min(count - 1);
            let (requests, essential_count) = r.requests();
            let delivered = lod::covering_key(&r.active_set,face as u8,u,v).map(|key|key.level);
            let requested = r.requested_level_for(r.altitude);
            let time = js_now();
            let eye = r.view_frame(time).0;
            let clearance = (eye.length() - r.displayed_radius(eye.normalize(), false, None, time)
                .max(r.displayed_radius(eye.normalize(), true, None, time))) * radius;
            let morph_remaining = r.active_keys.iter().map(|key|
                (r.tiles[r.tile_lookup[key]].transition_start + MORPH_DURATION_SECONDS - time).max(0.0))
                .fold(0.0f64, f64::max);
            let spacing = delivered.map(|level| {
                let segments = if level == 0 {
                    r.world.as_ref().map_or(128, World::mesh_resolution)
                } else if level <= 2 { STREAM_SEGMENTS } else { DEEP_STREAM_SEGMENTS };
                let step = 2.0 * radius / (segments as f64 * (1u32 << level) as f64);
                step * (1.0 + u * u).sqrt().max((1.0 + v * v).sqrt()) / (1.0 + u * u + v * v)
            });
            let mut snapshot = serde_json::json!({ "face": face, "level": level, "x": x, "y": y, "altitude_m": r.altitude * radius, "direction": r.direction().to_array(), "view_tilt_deg": r.view_tilt.to_degrees(), "active_level":lod::covering_key(&r.active_set,face as u8,u,v).map(|key|key.level), "active_tiles": r.active_keys.len(), "draw_tiles":r.draw_keys.len(),
                "camera_clearance_m":clearance,"grid_spacing_m":spacing,"morph_remaining_s":morph_remaining,"requested_level":requested,"delivered_level":delivered,"detail_limit_reached":level < u32::from(requested),"planned_leaves":graduated_leaf_count(&r.desired,r.wanted),"max_planned_leaves":MAX_PLANNED_LEAVES,
                "cpu_bytes":r.cache_bytes().0,"gpu_bytes":r.cache_bytes().1,"cpu_budget_bytes":768*1024*1024u64,"gpu_budget_bytes":768*1024*1024u64,"resident_keys":r.tiles.iter().map(|tile|tile.key).collect::<Vec<_>>(),
                "surface_version":r.surface_version,"surface_update_version":r.surface_update.as_ref().map(|update|update.version),"surface_update_keys":r.surface_update.as_ref().map(|update|&update.keys),"surface_update_staged":r.surface_update.as_ref().map_or(0,|update|update.staged.len()),"active_keys":r.active_keys,
                "pending_tiles":r.pending.as_ref().map_or(0,|p|p.jobs.len()-p.cursor),"mesh_builds":r.metrics.mesh_builds,"uploads":r.metrics.uploads,"upload_bytes":r.metrics.upload_bytes,
                "cut_commits":r.metrics.cut_commits,"instant_coarsen_commits":r.metrics.instant_coarsen_commits,"reused_tiles":r.metrics.reused_tiles,"prepare_ms":r.metrics.last_prepare_ms,"surface_fallbacks":r.metrics.surface_fallbacks,"max_prepare_ms":r.metrics.max_prepare_ms,"max_render_ms":r.metrics.max_render_ms,"essential_count": essential_count,"requests": requests });
            snapshot["exaggeration"] = serde_json::json!(r.exaggeration);
            snapshot["instant_refine_commits"] = serde_json::json!(r.metrics.instant_refine_commits);
            snapshot["morph_duration_s"] = serde_json::json!(MORPH_DURATION_SECONDS);
            let snapshot = snapshot.to_string();
            VIEW_SNAPSHOT.with(|cached| *cached.borrow_mut() = snapshot.clone());
            snapshot
        })
    }
}

#[cfg(target_arch = "wasm32")]
pub use browser::*;

#[cfg(test)]
mod tests {
    #[test]
    fn learned_inspection_centre_reaches_native_resolution_without_refining_the_entire_view() {
        let radius = 6_371_000.0;
        let direction = DVec3::new(
            -0.1392381651514895,
            0.015470907239054392,
            0.9901380632994811,
        )
        .normalize();
        let altitude = 18000.0 / radius;
        let base = bounded_footprint_level(direction, 14, 1.6, altitude, 25f64.to_radians());
        let targets = inspection_targets(direction, 14, base, 1.6, altitude, 25f64.to_radians());
        assert!(graduated_leaf_count(&targets, 14) <= MAX_PLANNED_LEAVES);
        let (face, u, v) = face_uv(direction);
        let count = 1u32 << 13;
        let parent = TileKey {
            face: face as u8,
            level: 13,
            x: (((u + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
            y: (((v + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
        };
        assert!(targets.contains(&parent));
        assert!(2.0 * radius / (32.0 * (1u32 << 14) as f64) <= 30.0);
        assert!(targets.iter().any(|key| key.level < base));
    }
    use super::*;

    fn desired_partition(targets: &[TileKey], wanted: u8) -> HashSet<TileKey> {
        let mut parents = HashSet::new();
        for target in targets {
            for level in 0..wanted.min(target.level + 1) {
                parents.insert(lod::ancestor(*target, level).unwrap());
            }
        }
        let mut parents: Vec<_> = parents.into_iter().collect();
        parents.sort_by_key(|key| key.level);
        let mut cut = lod::initial_leaves();
        for parent in parents {
            assert!(lod::split(&mut cut, parent));
        }
        cut
    }

    fn assert_graded_cut(cut: &HashSet<TileKey>) {
        for key in cut {
            for neighbor in tile_neighbors(*key, cut) {
                assert!(
                    key.level.abs_diff(neighbor.level) <= 1,
                    "ungraded neighbors {key:?} / {neighbor:?}"
                );
            }
        }
    }

    #[test]
    fn graduated_targets_cover_quadrant_boundaries_and_every_cube_edge() {
        for face in 0..6 {
            for (u, v) in [
                (1e-8, 1e-8),
                (0.9999999, 0.01),
                (-0.9999999, 0.01),
                (0.01, 0.9999999),
                (0.01, -0.9999999),
                (0.9999999, 0.9999999),
                (-0.9999999, -0.9999999),
            ] {
                for level in [6, 14, 20, 21, 22, 23] {
                    let direction = cube_direction(face, u, v);
                    let raw = footprint_keys(direction, level, 1.4, 8.0 / 1_737_000.0, 0.6);
                    if raw.len() > MAX_VISIBLE_TILES {
                        continue;
                    }
                    let targets = graduate_targets(&raw, level, direction);
                    let cut = desired_partition(&targets, level);
                    assert_graded_cut(&cut);
                    assert_eq!(cut.len(), graduated_leaf_count(&targets, level));
                    let (face, u, v) = face_uv(direction);
                    assert_eq!(
                        lod::covering_key(&cut, face as u8, u, v).unwrap().level,
                        level
                    );
                }
                let direction = cube_direction(face, u, v);
                let level = bounded_footprint_level(direction, 24, 1.4, 8.0 / 1_737_000.0, 0.6);
                let raw = footprint_keys(direction, level, 1.4, 8.0 / 1_737_000.0, 0.6);
                let targets = graduate_targets(&raw, level, direction);
                let cut = desired_partition(&targets, level);
                assert!(cut.len() <= MAX_PLANNED_LEAVES);
                assert_graded_cut(&cut);
            }
        }
        let shore = DVec3::new(
            0.6591403268957244,
            -0.7460306669641357,
            -0.09472208511664254,
        );
        for requested in [19, 20, 21, 22, 23, 24] {
            let level = bounded_footprint_level(
                shore,
                requested,
                918.0 / 773.0,
                5.0 / 1_737_000.0,
                30f64.to_radians(),
            );
            let raw = footprint_keys(
                shore,
                level,
                918.0 / 773.0,
                5.0 / 1_737_000.0,
                30f64.to_radians(),
            );
            let targets = graduate_targets(&raw, level, shore);
            let cut = desired_partition(&targets, level);
            assert!(level <= requested);
            assert!(cut.len() <= MAX_PLANNED_LEAVES);
            assert_graded_cut(&cut);
            let (face, u, v) = face_uv(shore);
            assert_eq!(
                lod::covering_key(&cut, face as u8, u, v).unwrap().level,
                level
            );
        }
    }

    #[test]
    fn hot_center_waits_for_cold_support_and_every_transition_remains_graded() {
        let wanted = 10;
        let target = TileKey {
            face: 4,
            level: wanted,
            x: (1 << 9) - 1,
            y: 1 << 9,
        };
        let direction = tile_center(target);
        let targets = graduate_targets(&[target], wanted, direction);
        let mut available = lod::initial_leaves();
        for level in 0..wanted {
            available.extend(lod::children(lod::ancestor(target, level).unwrap()));
        }
        let mut cut = lod::initial_leaves();
        // Prior visits have warmed only the center's descendants.
        loop {
            let parents: Vec<_> = cut
                .iter()
                .copied()
                .filter(|key| {
                    key.level < wanted
                        && lod::is_ancestor(*key, target)
                        && lod::children(*key)
                            .iter()
                            .all(|child| available.contains(child))
                        && split_preserves_grading(*key, &cut)
                })
                .collect();
            if parents.is_empty() {
                break;
            }
            for parent in parents {
                assert!(lod::split(&mut cut, parent));
                assert_graded_cut(&cut);
            }
        }
        let (_, u, v) = face_uv(direction);
        assert!(lod::covering_key(&cut, target.face, u, v).unwrap().level < wanted);
        assert!(
            frontier_keys(&cut, &targets, wanted, direction, false)
                .iter()
                .any(|key| !available.contains(key)),
            "cold ring was never requested"
        );
        // Deliver only requested quartets. Guards cannot deadlock the ring.
        for _ in 0..MAX_PLANNED_LEAVES {
            if cut == desired_partition(&targets, wanted) {
                break;
            }
            let staged = cut
                .iter()
                .copied()
                .filter(|key| needs_refinement(*key, &targets, wanted))
                .filter(|key| split_preserves_grading(*key, &cut))
                .min_by(|a, b| {
                    a.level.cmp(&b.level).then_with(|| {
                        tile_center(*b)
                            .dot(direction)
                            .total_cmp(&tile_center(*a).dot(direction))
                    })
                })
                .expect("graded support has no usable reserved quartet");
            let requested = pressure_frontier(
                frontier_keys(&cut, &targets, wanted, direction, false),
                &cut,
                Some(staged),
            );
            assert_eq!(
                requested.len(),
                4,
                "pressure must preserve a complete reserved quartet"
            );
            available.extend(requested);
            let mut parents: Vec<_> = cut
                .iter()
                .copied()
                .filter(|key| {
                    key.level < wanted
                        && targets.iter().any(|target| lod::is_ancestor(*key, *target))
                        && lod::children(*key)
                            .iter()
                            .all(|child| available.contains(child))
                })
                .collect();
            parents.sort_by_key(|key| key.level);
            let mut changed = false;
            for parent in parents {
                if split_preserves_grading(parent, &cut) {
                    assert!(lod::split(&mut cut, parent));
                    assert_graded_cut(&cut);
                    changed = true;
                }
            }
            assert!(changed, "requested support was blocked permanently");
        }
        assert_eq!(cut, desired_partition(&targets, wanted));
        // Zooming back out must also retain the supporting ring until its fine
        // neighbors have coarsened; every intermediate partition stays graded.
        let coarse_target = lod::ancestor(target, 2).unwrap();
        let coarse_targets = graduate_targets(&[coarse_target], 2, direction);
        for _ in 0..100 {
            if cut == desired_partition(&coarse_targets, 2) {
                break;
            }
            let mut parents: Vec<_> = cut
                .iter()
                .filter_map(|key| lod::parent(*key))
                .filter(|parent| !needs_refinement(*parent, &coarse_targets, 2))
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            parents.sort_by_key(|key| std::cmp::Reverse(key.level));
            let mut changed = false;
            for parent in parents {
                if coarsen_preserves_grading(parent, &cut) {
                    assert!(lod::coarsen(&mut cut, parent));
                    assert_graded_cut(&cut);
                    changed = true;
                }
            }
            assert!(changed, "coarsening ring was blocked permanently");
        }
        assert_eq!(cut, desired_partition(&coarse_targets, 2));
    }

    #[test]
    fn local_transition_neighbors_match_full_scans_on_cube_edges_and_coarsening_batches() {
        let mut total_groups = 0;
        let mut largest_batch = 0;
        for face in 0..6 {
            for (u, v) in [(1e-8, -1e-8), (0.9999999, 0.13), (0.9999999, 0.9999999)] {
                let direction = cube_direction(face, u, v);
                let raw = footprint_keys(direction, 9, 1.4, 0.002, 0.5);
                let targets = graduate_targets(&raw, 9, direction);
                let mut cut = desired_partition(&targets, 9);
                assert_graded_cut(&cut);
                let mut leaves: Vec<_> = cut.iter().copied().collect();
                leaves.sort();
                for leaf in leaves.iter().step_by((leaves.len() / 32).max(1)) {
                    assert_eq!(
                        graded_transition_neighbors(*leaf, &cut, false),
                        transition_neighbors(*leaf, &cut, false),
                        "split neighbors {leaf:?}"
                    );
                }
                // Run entire inverse batches. Every guard is evaluated against
                // the old cached neighborhood and checked against the proposed
                // partition by the independent, full-scan oracle.
                for _ in 0..100 {
                    if cut == lod::initial_leaves() {
                        break;
                    }
                    let mut candidates: Vec<_> = cut
                        .iter()
                        .filter_map(|key| lod::parent(*key))
                        .collect::<HashSet<_>>()
                        .into_iter()
                        .collect();
                    candidates.sort_by_key(|key| std::cmp::Reverse(key.level));
                    let mut proposed = cut.clone();
                    let mut reserved = HashSet::new();
                    let mut groups = 0;
                    for parent in candidates {
                        if !lod::children(parent).iter().all(|key| cut.contains(key)) {
                            continue;
                        }
                        let affected = graded_transition_neighbors(parent, &cut, true);
                        assert_eq!(
                            affected,
                            transition_neighbors(parent, &cut, true),
                            "coarsen neighbors {parent:?}"
                        );
                        if affected.iter().any(|key| key.level > parent.level + 1) {
                            continue;
                        }
                        let footprint = transition_footprint(parent, &cut, true, &affected);
                        if !reserved.is_disjoint(&footprint) {
                            continue;
                        }
                        assert!(
                            coarsen_preserves_grading(parent, &proposed),
                            "cached guard accepted an unsafe proposed merge {parent:?}"
                        );
                        assert!(lod::coarsen(&mut proposed, parent));
                        assert_graded_cut(&proposed);
                        reserved.extend(footprint);
                        groups += 1;
                        if groups == 8 {
                            break;
                        }
                    }
                    assert!(
                        groups > 0,
                        "local neighbor lookup blocked every inverse group"
                    );
                    total_groups += groups;
                    largest_batch = largest_batch.max(groups);
                    cut = proposed;
                }
                assert_eq!(cut, lod::initial_leaves());
            }
        }
        assert!(total_groups > 100);
        assert!(
            largest_batch >= 2,
            "independent merges were serialized one at a time"
        );
    }

    #[test]
    fn cartographic_palette_preserves_land_mixtures_without_a_dry_bank_halo() {
        let mut sample = Sample {
            direction: DVec3::Z.to_array(),
            height_m: 100.0,
            water_m: 100.0,
            temperature_c: 20.0,
            biome: "Forest".into(),
            ..Default::default()
        };
        sample.biome_mix[5] = 0.4;
        sample.biome_mix[6] = 0.6;
        let expected = mix_color(biome_color("Grassland"), biome_color("Forest"), 0.6);
        let color = cartographic_land_color(&sample);
        for i in 0..3 {
            assert!((color[i] - expected[i]).abs() < 1e-6);
        }
        sample.height_m = 1.0;
        sample.water_level_m = Some(0.0);
        sample.water_body_id = Some(0);
        let shore = vertex(&sample, 1_737_000.0, 1.0, false);
        assert_eq!(&shore.color[..3], &color);
        assert_eq!(shore.fields[3], -1.0);
        assert_eq!(shore.color[3], 0.0);
        assert_eq!(sample.height_m, 1.0);
        assert_eq!(sample.water_level_m, Some(0.0));
        // The presence, owner and altitude of a hydraulic datum do not paint
        // a beach or recolor an inland plain that happens to share its height.
        sample.water_body_id = Some(77);
        sample.water_level_m = Some(-300.0);
        assert_eq!(cartographic_land_color(&sample), color);
    }

    #[test]
    fn terrestrial_gpu_fractions_exclude_water_and_use_climate_for_submerged_beds() {
        let mut sample = Sample {
            direction: DVec3::Z.to_array(),
            biome: "Ocean".into(),
            temperature_c: 18.0,
            moisture: 0.6,
            ..Default::default()
        };
        sample.biome_mix[0] = 1.0;
        assert_eq!(
            terrestrial_mix(&sample),
            [0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]
        );
        sample.biome_mix[1] = 100.0;
        sample.biome_mix[5] = 2.0;
        sample.biome_mix[6] = 3.0;
        let land = vertex(&sample, 1_737_000.0, 1.0, false);
        assert_eq!(land.biome_a, [0.0, 0.0, 0.0, 0.4]);
        assert_eq!(land.biome_b, [0.6, 0.0, 0.0, 0.0]);
        assert_eq!(land.color[3], 0.0);
        assert_eq!(terrestrial_mix(&sample).iter().sum::<f32>(), 1.0);
    }

    #[test]
    fn two_metre_noise_phase_survives_camera_rebasing_and_submetre_motion() {
        let anchor = DVec3::new(0.31, -0.42, 0.852).normalize();
        let tangent = anchor.cross(DVec3::Y).normalize();
        let radius = 6_371_000.0;
        let point = (anchor + tangent * (500.0 / radius)).normalize();
        let next = (point + tangent * (0.25 / radius)).normalize();
        let nearby_anchor = (anchor + tangent * (2000.0 / radius)).normalize();
        let frame = noise_frame(anchor, radius);
        let rebased = noise_frame(nearby_anchor, radius);
        let shader_phase = |direction: DVec3, frame: NoiseFrame| {
            let sample = Sample {
                direction: direction.to_array(),
                ..Default::default()
            };
            let v = vertex(&sample, radius, 30.0, false);
            let high = Vec3::from_array(v.normal_height[..3].try_into().unwrap());
            let low = Vec3::from_array(v.direction_low);
            let anchor_high = Vec3::from_array(frame.anchor_high[..3].try_into().unwrap());
            let anchor_low = Vec3::from_array(frame.anchor_low[..3].try_into().unwrap());
            let delta = ((high - anchor_high) + (low - anchor_low)) * radius as f32;
            let rotated = Vec3::new(
                delta.dot(Vec3::new(0.36, 0.48, 0.8)),
                delta.dot(Vec3::new(-0.8, 0.6, 0.0)),
                delta.dot(Vec3::new(-0.48, -0.64, 0.6)),
            );
            let record = frame.bands[5];
            DVec3::new(
                record.cell[0] as f64,
                record.cell[1] as f64,
                record.cell[2] as f64,
            ) + (rotated / record.phase_scale[3]
                + Vec3::from_array(record.phase_scale[..3].try_into().unwrap()))
            .as_dvec3()
        };
        let phase = shader_phase(point, frame);
        let expected = noise_axes(point) * (radius / 2.0);
        assert!((phase - expected).length() < 0.001);
        assert!((shader_phase(point, rebased) - phase).length() < 0.002);
        let step = shader_phase(next, frame) - phase;
        let expected_step = noise_axes(next - point) * (radius / 2.0);
        assert!((step - expected_step).length() < 0.001);
        assert!(step.length() > 0.12);
        // Height exaggeration changes geometry, never these ink coordinates.
        let sample = Sample {
            direction: point.to_array(),
            height_m: 10_000.0,
            ..Default::default()
        };
        assert_eq!(
            vertex(&sample, radius, 1.0, false).direction_low,
            vertex(&sample, radius, 30.0, false).direction_low
        );
    }

    #[test]
    fn rollback_selects_the_committed_field_without_rebuilding_mutable_patches() {
        use world_core::hybrid::{HybridField, HybridPatch};
        let make = |revision, value| {
            HybridField::new(
                "world".into(),
                revision,
                6371000.0,
                vec![HybridPatch {
                    face: 4,
                    level: 0,
                    x: 0,
                    y: 0,
                    values: vec![value; 258 * 258],
                }],
            )
            .unwrap()
        };
        let mut snapshots = HybridSnapshots::default();
        let a = snapshots.prepare(make(1, 10.0));
        let a_key = HybridSnapshots::key(&a);
        let b = snapshots.prepare(make(2, 50.0));
        assert_eq!(snapshots.fields.len(), 2);
        assert_eq!(b.patches()[0].values[100], 50.0);
        let restored = snapshots.select(&a_key).unwrap();
        assert!(std::rc::Rc::ptr_eq(&a, &restored));
        assert_eq!(restored.patches()[0].values[100], 10.0);
        assert_eq!(snapshots.fields.len(), 1);
        snapshots.prepare(make(3, -25.0));
        assert_eq!(snapshots.fields.len(), 2);
        snapshots.procedural();
        assert!(snapshots.fields.is_empty());
    }

    #[test]
    fn coarse_edges_find_both_finer_neighbors_before_shared_surface_updates() {
        let mut cut = lod::initial_leaves();
        let root = TileKey {
            face: 4,
            level: 0,
            x: 0,
            y: 0,
        };
        lod::split(&mut cut, root);
        let coarse = TileKey {
            face: 4,
            level: 1,
            x: 0,
            y: 0,
        };
        let adjacent = TileKey {
            face: 4,
            level: 1,
            x: 1,
            y: 0,
        };
        lod::split(&mut cut, adjacent);
        let neighbors = tile_neighbors(coarse, &cut);
        assert!(neighbors.contains(&TileKey {
            face: 4,
            level: 2,
            x: 2,
            y: 0
        }));
        assert!(neighbors.contains(&TileKey {
            face: 4,
            level: 2,
            x: 2,
            y: 1
        }));
        let affected = surface_refresh_keys(&cut, &HashSet::from([coarse]));
        assert!(neighbors.is_subset(&affected));
    }

    #[test]
    fn inactive_roots_are_refreshed_as_a_complete_canonical_return_shell() {
        let roots = lod::initial_leaves();
        let root = TileKey {
            face: 4,
            level: 0,
            x: 0,
            y: 0,
        };
        let mut active = roots.clone();
        assert!(lod::split(&mut active, root));
        assert!(!active.contains(&root));
        let refresh = surface_refresh_keys(&active, &HashSet::from([root]));
        assert_eq!(refresh, roots);
        let mut resident: HashSet<_> = active.union(&roots).copied().collect();
        // Beginning/cancelling does not invalidate any committed cache entry.
        assert!(roots.is_subset(&resident));
        // A committed root update replaces all six resident atlas packets.
        resident.extend(refresh);
        assert!(lod::initial_leaves().is_subset(&resident));
        assert!(valid_tile_vertex_count(root, 129 * 129, 128));
        assert!(!valid_tile_vertex_count(root, 65 * 65, 128));
    }

    #[test]
    fn packet_upload_ownership_never_adds_all_mesh_and_gpu_cpu_representations() {
        let allocations = [(20, 50, 30, 15), (40, 25, 60, 12), (30, 55, 32, 18)];
        let reserve: usize = allocations
            .iter()
            .map(|(b, m, c, _)| surface_cpu_reservation(*b, *m, *c))
            .sum();
        let scratch = allocations
            .iter()
            .map(|(_, m, c, s)| m.min(c) + s)
            .max()
            .unwrap();
        for staged_mask in 0u32..8 {
            let owned: usize = allocations
                .iter()
                .enumerate()
                .map(|(i, (b, m, c, _))| b + if staged_mask & (1 << i) == 0 { *m } else { *c })
                .sum();
            assert!(owned <= reserve);
            for (_, m, c, s) in allocations {
                // During one upload both representations and the temporary
                // static vertices coexist; never multiply scratch by all tiles.
                assert!(owned + m.min(c) + s <= reserve + scratch);
            }
        }
    }

    #[test]
    fn invisible_refinement_still_uploads_all_children_before_its_cut_commits() {
        let parent = TileKey {
            face: 4,
            level: 0,
            x: 0,
            y: 0,
        };
        let mut active = lod::initial_leaves();
        let mut resident_gpu = active.clone();
        assert!(transition_needs_child_uploads(false, true));
        if transition_needs_child_uploads(false, true) {
            resident_gpu.extend(lod::children(parent));
        }
        assert!(lod::split(&mut active, parent));
        assert!(active.is_subset(&resident_gpu));
        assert!(!transition_needs_child_uploads(true, true));
        assert!(transition_needs_child_uploads(true, false));
    }

    #[test]
    fn refinement_visibility_guard_is_rechecked_for_a_camera_that_moves_during_preparation() {
        let bounds = [(DVec3::new(2.0, 0.0, 1.0), 0.01)];
        let away = camera_frame(DVec3::Z, 0.01, 1.0, 0.0);
        assert!(instant_coarsen_bounds(&bounds, away, 1.73, 771));
        let eye = DVec3::new(2.0, 0.0, 1.05);
        let visible = (eye, -DVec3::Z, DVec3::X, DVec3::Y);
        assert!(!instant_coarsen_bounds(&bounds, visible, 1.73, 771));
    }

    #[test]
    fn bounded_atomic_cut_still_delivers_twenty_centimetre_spacing_at_five_metres() {
        let radius = 6371000.0;
        for direction in [
            DVec3::Y,
            -DVec3::Y,
            DVec3::ONE.normalize(),
            DVec3::new(0.1642453664314664, -0.1052273755405935, 0.9807908334821612),
        ] {
            let level = bounded_footprint_level(direction, 24, 1.73, 5.0 / radius, 0.0);
            let targets = graduate_targets(
                &footprint_keys(direction, level, 1.73, 5.0 / radius, 0.0),
                level,
                direction,
            );
            let (_, u, v) = face_uv(direction);
            let spacing = 2.0 * radius / (32.0 * f64::from(1u32 << level))
                * (1.0 + u * u).sqrt().max((1.0 + v * v).sqrt())
                / (1.0 + u * u + v * v);
            assert!(graduated_leaf_count(&targets, level) <= MAX_PLANNED_LEAVES);
            assert!(
                spacing <= 0.2,
                "{direction:?}: level={level}, spacing={spacing}"
            );
        }
    }

    #[test]
    fn surface_reservations_reject_excess_before_collecting_or_uploading() {
        let budget = (1024, 768);
        let old = (400, 300);
        assert!(within_surface_budget(old, (624, 468), budget));
        assert!(!within_surface_budget(old, (625, 468), budget));
        assert!(!within_surface_budget(old, (624, 469), budget));
        assert!(!within_surface_budget((usize::MAX, 0), (1, 0), budget));
        // Moving allocated packet bytes into staged GPU ownership must still
        // reserve the rest of the complete transaction; cancellation releases it.
        assert!(within_surface_budget((600, 400), (400, 300), budget));
        assert!(within_surface_budget(old, (0, 0), budget));
    }

    #[test]
    fn physical_material_phase_matches_coincident_sloping_parent_child_triangles() {
        let project = |point: DVec3, anchor: DVec3, radius: f64| {
            let frame = noise_frame(anchor, radius);
            let high = point.as_vec3();
            let low = (point - high.as_dvec3()).as_vec3();
            let ah = Vec3::from_array(frame.anchor_high[..3].try_into().unwrap());
            let al = Vec3::from_array(frame.anchor_low[..3].try_into().unwrap());
            let delta = ((high - ah) + (low - al)) * radius as f32;
            let scale = frame.anchor_high[3];
            let u = (ah + al) * scale;
            let inv_radius = scale / radius as f32;
            let z = 2.0 * u.dot(delta) + delta.dot(delta) * inv_radius;
            let norm = (1.0 + z * inv_radius).sqrt();
            (delta - u * (z / (norm + 1.0))) * (scale / norm)
        };
        for origin in [DVec3::Z, DVec3::Y, -DVec3::Y] {
            let east = DVec3::X;
            let north = origin.cross(east);
            for radius in [6371000.0, 100000000.0] {
                for exag in [1.0, 4.0, 30.0] {
                    let points = [
                        (-0.2, -0.13, 1000.0),
                        (0.2, -0.1, 1000.12),
                        (0.0, 0.2, 999.92),
                    ]
                    .map(|(x, y, h)| {
                        (origin + (east * x + north * y) / radius).normalize()
                            * (1.0 + h * exag / radius)
                    });
                    let fragment = points[0] * 0.2 + points[1] * 0.35 + points[2] * 0.45;
                    let anchor = fragment.normalize() * (fragment.length() + 5.0 / radius);
                    let phase = project(fragment, anchor, radius);
                    let rebased = project(
                        fragment,
                        (anchor + east * (2000.0 / radius)).normalize() * anchor.length(),
                        radius,
                    );
                    let expected = (fragment.normalize() - anchor.normalize()) * radius;
                    assert!((phase.as_dvec3() - expected).length() < 0.004);
                    let rebase_shift = (anchor.normalize()
                        - (anchor + east * (2000.0 / radius)).normalize())
                        * radius;
                    assert!(
                        (rebased.as_dvec3() - phase.as_dvec3() - rebase_shift).length() < 0.004
                    );
                }
            }
        }
    }

    #[test]
    fn twenty_centimetre_material_phase_preserves_global_cells_at_poles_and_after_rebasing() {
        assert_eq!(std::mem::size_of::<NoiseFrame>(), 288);
        let shader_phase = |direction: DVec3, frame: NoiseFrame, radius: f64| {
            let vertex = vertex(
                &Sample {
                    direction: direction.to_array(),
                    ..Default::default()
                },
                radius,
                1.0,
                false,
            );
            let high = Vec3::from_array(vertex.normal_height[..3].try_into().unwrap());
            let low = Vec3::from_array(vertex.direction_low);
            let anchor_high = Vec3::from_array(frame.anchor_high[..3].try_into().unwrap());
            let anchor_low = Vec3::from_array(frame.anchor_low[..3].try_into().unwrap());
            let delta = ((high - anchor_high) + (low - anchor_low)) * radius as f32;
            let rotated = Vec3::new(
                delta.dot(Vec3::new(0.36, 0.48, 0.8)),
                delta.dot(Vec3::new(-0.8, 0.6, 0.0)),
                delta.dot(Vec3::new(-0.48, -0.64, 0.6)),
            );
            let band = frame.bands[7];
            DVec3::new(
                band.cell[0] as f64,
                band.cell[1] as f64,
                band.cell[2] as f64,
            ) + (rotated / band.phase_scale[3]
                + Vec3::from_array(band.phase_scale[..3].try_into().unwrap()))
            .as_dvec3()
        };
        for anchor in [
            DVec3::new(0.31, -0.42, 0.852).normalize(),
            DVec3::Y,
            -DVec3::Y,
        ] {
            let tangent = if anchor.cross(DVec3::Y).length_squared() > 1e-9 {
                anchor.cross(DVec3::Y).normalize()
            } else {
                DVec3::X
            };
            for radius in [6_371_000.0, 100_000_000.0] {
                let point = (anchor + tangent * (500.0 / radius)).normalize();
                let next = (point + tangent * (0.02 / radius)).normalize();
                let other = (anchor + tangent * (2000.0 / radius)).normalize();
                let frame = noise_frame(anchor, radius);
                let phase = shader_phase(point, frame, radius);
                let rebased = shader_phase(point, noise_frame(other, radius), radius);
                let expected = noise_axes(point) * (radius / 0.2);
                assert!((phase - expected).length() < 0.01);
                assert!((rebased - phase).length() < 0.02);
                let step = shader_phase(next, frame, radius) - phase;
                let expected_step = noise_axes(next - point) * (radius / 0.2);
                assert!((step - expected_step).length() < 0.01);
                assert!(step.length() > 0.075);
            }
        }
    }

    #[test]
    fn water_palette_retains_opaque_flags_real_depth_and_river_markers() {
        let marine = Sample {
            direction: DVec3::Z.to_array(),
            height_m: -0.001,
            water_m: 0.0,
            water_level_m: Some(0.0),
            water_body_id: Some(0),
            ocean: true,
            ..Default::default()
        };
        let mut fresh = marine.clone();
        fresh.height_m = 99.999;
        fresh.water_m = 100.0;
        fresh.water_level_m = Some(100.0);
        fresh.water_body_id = Some(77);
        fresh.ocean = false;
        fresh.lake = true;
        let mut river = fresh.clone();
        river.lake = false;
        river.water_body_id = Some(RIVER_BODY + 77);
        river.river = 0.8;
        let sea_vertex = vertex(&marine, 1_737_000.0, 1.0, true);
        let lake_vertex = vertex(&fresh, 1_737_000.0, 1.0, true);
        let river_vertex = vertex(&river, 1_737_000.0, 1.0, true);
        assert!(sea_vertex.color[2] > sea_vertex.color[1] * 1.3);
        assert!(lake_vertex.color[2] < lake_vertex.color[1] * 1.3);
        for v in [sea_vertex, lake_vertex, river_vertex] {
            assert_eq!(v.color[3], 1.0);
            assert!((v.fields[3] - 0.001).abs() < 1e-7);
            assert!(v.color[..3]
                .iter()
                .all(|c| c.is_finite() && *c > 0.0 && *c < 1.0));
        }
        assert_eq!(river_vertex.fields[2], -2.0);
        assert_eq!(sea_vertex.fields[2], 0.0);
        assert_eq!(lake_vertex.fields[2], 0.0);
    }

    #[test]
    fn cartographic_seed_keeps_the_full_imported_integer() {
        let config: Config = serde_json::from_str(
            r#"{"seed":9007199254740991,"radius_m":1737000,"resolution":128}"#,
        )
        .unwrap();
        let words = cartography_seed(config.seed);
        assert_eq!(
            u64::from(words[0]) | (u64::from(words[1]) << 32),
            config.seed
        );
        assert_ne!(
            cartography_seed(config.seed),
            cartography_seed(config.seed + (1 << 32))
        );
    }

    #[test]
    fn physical_cover_slope_is_independent_of_relief_exaggeration() {
        let radial = DVec3::new(0.31, 0.42, 0.852).normalize();
        let tangent = radial.cross(DVec3::Y).normalize();
        let radius = 1_737_000.0;
        for degrees in [0.0_f64, 15.0, 35.0, 50.0, 70.0, 85.0] {
            for height in [0.0, 10_000.0] {
                for exaggeration in [1.0, 5.0, 30.0] {
                    let rendered_tangent =
                        degrees.to_radians().tan() * exaggeration * (radius + height)
                            / (radius + exaggeration * height);
                    let rendered_normal =
                        (radial + tangent * rendered_tangent).normalize().as_vec3();
                    let recovered = physical_slope_degrees(
                        rendered_normal,
                        radial,
                        height,
                        radius,
                        exaggeration,
                    );
                    assert!(
                        (recovered - degrees).abs() < 0.001,
                        "slope{degrees}° height{height}×{exaggeration} recovered{recovered}°"
                    );
                    for (actual, expected) in
                        slope_cover(recovered).into_iter().zip(slope_cover(degrees))
                    {
                        assert!((actual - expected).abs() < 0.0001);
                    }
                }
            }
        }
        assert_eq!(slope_cover(15.0), [1.0, 1.0, 0.0]);
        assert_eq!(slope_cover(55.0)[0], 0.0);
        assert!(slope_cover(40.0)[1] < 0.4);
        assert_eq!(slope_cover(60.0)[1], 0.0);
        assert_eq!(slope_cover(65.0), [0.0, 0.0, 1.0]);
        assert_eq!(slope_cover(89.0), [0.0, 0.0, 1.0]);
    }

    fn prepared_test_tile() -> Tile {
        let side = 9;
        let key = TileKey {
            face: 4,
            level: 6,
            x: 30,
            y: 30,
        };
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for y in 0..side {
            for x in 0..side {
                let u = 2.0 * (key.x as f64 + x as f64 / (side - 1) as f64) / 64.0 - 1.0;
                let v = 2.0 * (key.y as f64 + y as f64 / (side - 1) as f64) / 64.0 - 1.0;
                let height = (x as f64 - 4.0) * 0.7 + (y as f64 * 0.31).sin();
                vertices.push(Sample {
                    direction: DVec3::new(u, v, 1.0).normalize().to_array(),
                    height_m: height,
                    water_m: height.max(0.0),
                    water_level_m: Some(0.0),
                    water_body_id: Some(0),
                    ocean: height < 0.0,
                    biome: "Forest".into(),
                    biome_mix: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
                    ..Default::default()
                });
            }
        }
        for y in 0..side - 1 {
            for x in 0..side - 1 {
                let a = (y * side + x) as u32;
                let b = a + 1;
                let c = a + side as u32;
                let d = c + 1;
                indices.extend([a, b, d, a, d, c]);
            }
        }
        Tile {
            key,
            vertices,
            indices,
            ..Default::default()
        }
    }
    #[test]
    fn worker_packet_preserves_every_rendered_triangle_and_collision_sample() {
        let tile = prepared_test_tile();
        let expected = tile_mesh(&tile, 1_737_000.0, 1.0).unwrap();
        let packet = prepare_tile(tile, 1_737_000.0, 1.0).unwrap();
        let decoded = decode_prepared_tile(&packet).unwrap();
        assert_eq!(decoded.mesh.indices.len(), expected.indices.len());
        for (&a, &b) in expected.indices.iter().zip(&decoded.mesh.indices) {
            assert_eq!(
                bytemuck::bytes_of(&expected.vertices[a as usize]),
                bytemuck::bytes_of(&decoded.mesh.vertices[b as usize])
            );
        }
        let again = prepared_test_tile();
        for (a, b) in again.vertices.iter().zip(&decoded.tile.vertices) {
            assert_eq!(a.direction, b.direction);
            assert_eq!(a.height_m, b.height_m);
            assert_eq!(a.water_level_m, b.water_level_m);
        }
        assert_eq!(again.indices, decoded.tile.indices);
        assert!(decoded.mesh.vertices.len() < expected.vertices.len());
        assert!(decode_prepared_tile(&packet[..packet.len() - 1]).is_err());
        let mut legacy = packet.clone();
        legacy[..4].copy_from_slice(b"WVP1");
        assert!(decode_prepared_tile(&legacy).is_err());
        let mut extra = packet;
        extra.push(0);
        assert!(decode_prepared_tile(&extra).is_err());
    }
    #[test]
    fn hybrid_packet_preserves_procedural_reference_and_rejects_truncated_base_heights() {
        let raw = prepared_test_tile();
        let reference: Vec<_> = raw.vertices.iter().map(|sample| sample.height_m).collect();
        let radius = 1_737_000.0;
        let sample = &raw.vertices[44];
        let [x, y] = world_core::hybrid::chart_plane(4, sample.direction, radius);
        let field = world_core::hybrid::HybridField::new(
            "test-world".into(),
            1,
            radius,
            vec![world_core::hybrid::HybridPatch {
                face: 4,
                level: 0,
                x: (x / 7680.0).floor() as i32,
                y: (y / 7680.0).floor() as i32,
                values: vec![40.0; 258 * 258],
            }],
        )
        .unwrap();
        let packet = prepare_hybrid_tile(raw, radius, 1.0, Some(&field)).unwrap();
        let hybrid = decode_prepared_tile(&packet).unwrap();
        assert_eq!(hybrid.base_heights, reference);
        assert!(hybrid
            .tile
            .vertices
            .iter()
            .zip(&reference)
            .any(|(sample, base)| (sample.height_m - base).abs() > 0.00001));
        for (sample, base) in hybrid.tile.vertices.iter().zip(&reference) {
            if *base <= 0.0 {
                assert_eq!(sample.height_m, *base);
            }
        }
        assert!(decode_prepared_tile(&packet[..packet.len() - 8]).is_err());
        // Mode changes never reuse an already modified sample as a source.
        let restored = decode_prepared_tile(
            &prepare_hybrid_tile(prepared_test_tile(), radius, 1.0, None).unwrap(),
        )
        .unwrap();
        assert_eq!(restored.base_heights, reference);
        assert_eq!(
            restored
                .tile
                .vertices
                .iter()
                .map(|sample| sample.height_m)
                .collect::<Vec<_>>(),
            reference
        );
    }

    #[test]
    fn worker_packets_retain_hydraulic_feather_and_legacy_packets_default_to_one() {
        let mut tile = prepared_test_tile();
        for sample in &mut tile.vertices {
            sample.hybrid_weight = 0.125;
        }
        let packet = prepare_hybrid_tile(tile, 1_737_000.0, 1.0, None).unwrap();
        let decoded = decode_prepared_tile(&packet).unwrap();
        assert!(decoded
            .tile
            .vertices
            .iter()
            .all(|sample| sample.hybrid_weight == 0.125));
        let json: Sample = serde_json::from_str("{\"height_m\":42}").unwrap();
        assert_eq!(json.hybrid_weight, 1.0);
        assert_eq!(Sample::default().hybrid_weight, 1.0);

        let mut header: PreparedHeader =
            bytemuck::pod_read_unaligned(&packet[..std::mem::size_of::<PreparedHeader>()]);
        let hsize = std::mem::size_of::<PreparedHeader>();
        let current_samples_end =
            hsize + header.counts[0] as usize * std::mem::size_of::<PackedSample>();
        let mut legacy = Vec::new();
        header.magic = *b"WVP2";
        header.counts[5] = 0;
        legacy.extend_from_slice(bytemuck::bytes_of(&header));
        for bytes in
            packet[hsize..current_samples_end].chunks_exact(std::mem::size_of::<PackedSample>())
        {
            let sample: PackedSample = bytemuck::pod_read_unaligned(bytes);
            let old = LegacyPackedSample {
                direction: sample.direction,
                height: sample.height,
                water: sample.water,
                level: sample.level,
                body: sample.body,
                fields: sample.fields,
                mix: sample.mix,
                biome: sample.biome,
                flags: sample.flags,
            };
            legacy.extend_from_slice(bytemuck::bytes_of(&old));
        }
        legacy.extend_from_slice(
            &packet[current_samples_end..packet.len() - decoded.base_heights.len() * 8],
        );
        let restored = decode_prepared_tile(&legacy).unwrap();
        assert!(restored
            .tile
            .vertices
            .iter()
            .all(|sample| sample.hybrid_weight == 1.0));
        assert_eq!(restored.base_heights, decoded.base_heights);
    }

    #[test]
    fn border_normal_lookup_uses_original_direction_not_gpu_roundtrip() {
        let direction = DVec3::new(0.1234567800015, 0.5, 0.8571805081029673);
        let sample = Sample {
            direction: direction.to_array(),
            height_m: 0.00123456789,
            ..Default::default()
        };
        let gpu_vertex = vertex(&sample, 1.0, 1.0, false);
        let reconstructed = mesh_point(&gpu_vertex).normalize();
        let canonical = normal_key(DVec3::from_array(sample.direction));
        assert_ne!(canonical, normal_key(reconstructed));
        let normals = HashMap::from([(canonical, [0.0, 0.0, 1.0])]);
        assert_eq!(
            normals.get(&normal_key(DVec3::from_array(sample.direction))),
            Some(&[0.0, 0.0, 1.0])
        );
        assert!(normals.get(&normal_key(reconstructed)).is_none());
    }

    #[test]
    fn indexed_normals_match_the_previous_single_tile_smoother() {
        let tile = prepared_test_tile();
        let mut mesh = tile_mesh(&tile, 1_737_000.0, 1.0).unwrap();
        let normals: Vec<_> = mesh.vertices.iter().map(|v| v.shading_normal).collect();
        smooth_mesh_normals(std::slice::from_mut(&mut mesh));
        for (v, old) in mesh.vertices.iter().zip(normals) {
            assert!((Vec3::from_array(v.shading_normal) - Vec3::from_array(old)).length() < 1e-6);
        }
    }
    #[test]
    fn level_22_normals_preserve_an_analytic_half_meter_per_meter_slope() {
        let radius = 1_737_000.0;
        let key = TileKey {
            face: 4,
            level: 22,
            x: 1 << 21,
            y: 1 << 21,
        };
        let segments = 128;
        let count = f64::from(1u32 << key.level);
        let mut tile = Tile {
            key,
            ..Default::default()
        };
        for y in 0..=segments {
            for x in 0..=segments {
                let u = 2.0 * (f64::from(key.x) + x as f64 / segments as f64) / count - 1.0;
                let v = 2.0 * (f64::from(key.y) + y as f64 / segments as f64) / count - 1.0;
                let direction = cube_direction(key.face, u, v);
                // All reconstructed points lie on z = 1 + 0.5*x.
                let distance = 1.0 / (direction.z - 0.5 * direction.x);
                tile.vertices.push(Sample {
                    direction: direction.to_array(),
                    height_m: (distance - 1.0) * radius,
                    biome: "Rock".into(),
                    ..Default::default()
                });
            }
        }
        for y in 0..segments {
            for x in 0..segments {
                let a = (y * (segments + 1) + x) as u32;
                let b = a + 1;
                let c = a + (segments + 1) as u32;
                tile.indices.extend([a, b, c + 1, a, c + 1, c]);
            }
        }
        let mut mesh = tile_mesh(&tile, radius, 1.0).unwrap();
        let sums = local_normal_sums(&mesh, false);
        assert!(sums[segments + 2].length_squared() < 1e-30);
        let expected = DVec3::new(-0.5, 0.0, 1.0).normalize();
        for pass in 0..2 {
            for vertex in mesh.vertices.iter().take(tile.vertices.len()) {
                for normal in [vertex.shading_normal, vertex.parent_normal] {
                    assert!(
                        DVec3::from_array(normal.map(f64::from)).dot(expected) > 0.99999,
                        "LOD 22 slope was lost in normal pass {pass}: {normal:?}"
                    );
                }
            }
            smooth_mesh_normals(std::slice::from_mut(&mut mesh));
        }
    }

    #[test]
    fn stationary_cut_budget_uses_available_frame_time() {
        assert_eq!(cut_prepare_budget(0.0, 2.0, 0.0), 0.004);
        assert_eq!(cut_prepare_budget(1.0, 2.0, 0.0), 0.008);
        assert_eq!(cut_prepare_budget(1.0, 12.0, 8.0), 0.008);
        assert_eq!(cut_prepare_budget(1.0, 12.0, 0.0), 0.004);
    }

    #[test]
    fn disjoint_transition_groups_preserve_the_partition_in_both_directions() {
        let mut cut = lod::initial_leaves();
        for level in 0..3 {
            let parents: Vec<_> = cut
                .iter()
                .copied()
                .filter(|key| key.face == 4 && key.level == level)
                .collect();
            for parent in parents {
                assert!(lod::split(&mut cut, parent));
            }
        }
        let key = |x, y| TileKey {
            face: 4,
            level: 3,
            x,
            y,
        };
        let first = key(1, 1);
        let adjacent = key(2, 1);
        let distant = key(6, 6);
        let footprint = |parent, cut: &HashSet<TileKey>, coarsening| {
            transition_footprint(
                parent,
                cut,
                coarsening,
                &transition_neighbors(parent, cut, coarsening),
            )
        };
        assert!(!footprint(first, &cut, false).is_disjoint(&footprint(adjacent, &cut, false)));
        assert!(footprint(first, &cut, false).is_disjoint(&footprint(distant, &cut, false)));
        let before = cut.clone();
        for parent in [first, distant] {
            assert!(lod::split(&mut cut, parent));
        }
        assert!(footprint(first, &cut, true).is_disjoint(&footprint(distant, &cut, true)));
        for face in 0..6 {
            let area: f64 = cut
                .iter()
                .filter(|key| key.face == face)
                .map(|key| 4f64.powi(-i32::from(key.level)))
                .sum();
            assert_eq!(area, 1.0);
        }
        for a in &cut {
            for b in &cut {
                assert!(a == b || !lod::is_ancestor(*a, *b));
            }
        }
        // Arrival/order cannot change the final surface partition.
        for parent in [distant, first] {
            assert!(lod::coarsen(&mut cut, parent));
        }
        assert_eq!(cut, before);
    }
    #[test]
    fn parent_surface_query_matches_the_exact_collision_triangle() {
        let tile = prepared_test_tile();
        let mesh = tile_mesh(&tile, 1_737_000.0, 1.0).unwrap();
        for i in [0, 5, 17, 40, 79, 80] {
            let dir = DVec3::from_array(tile.vertices[i].direction);
            let expected =
                surface_radius(dir, tile.key, 9, |j| mesh_point(&mesh.vertices[j])).unwrap();
            let hit = surface_query(dir, tile.key, 9, |j| {
                (
                    mesh_point(&mesh.vertices[j]),
                    mesh.vertices[j].shading_normal,
                )
            })
            .unwrap();
            assert!((expected - hit.0).abs() < 1e-13);
            assert!(Vec3::from_array(hit.1).is_finite());
        }
    }

    #[test]
    fn a_distant_coarse_coast_can_lift_a_wet_border_vertex_into_a_green_wall() {
        let radius = 1_737_000.0;
        let side = 65;
        let coarse = TileKey {
            face: 4,
            level: 5,
            x: 16,
            y: 15,
        };
        let coarse_step = 2.0 / ((1u32 << coarse.level) as f64 * (side - 1) as f64);
        // A smooth shore has a maximum true slope of just 0.119 m/m. Both
        // ends of this coarse cell are dry, while its fine border is wet.
        let shore_height = |u: f64| 32.0 * (std::f64::consts::TAU * u / coarse_step).cos();
        let u = coarse_step * 257.0 / 1024.0;
        let direction = cube_direction(4, u, 0.0);
        let true_height = shore_height(u);
        assert!(true_height < 0.0);
        let displayed = |key: TileKey| {
            surface_query(direction, key, side, |index| {
                let count = (1u32 << key.level) as f64;
                let u = 2.0 * (key.x as f64 + (index % side) as f64 / 64.0) / count - 1.0;
                let v = 2.0 * (key.y as f64 + (index / side) as f64 / 64.0) / count - 1.0;
                let d = cube_direction(4, u, v);
                (d * (1.0 + shore_height(u) / radius), d.as_vec3().to_array())
            })
            .unwrap()
            .0
        };
        let ungraded_lift = (displayed(coarse) - 1.0) * radius - true_height;
        assert!(ungraded_lift > 30.0);
        // A neighbor one level below the L15 fine coast resolves its actual
        // surface. Its stitch remains sub-millimetric for the same wet point.
        let graded = TileKey {
            face: 4,
            level: 14,
            x: (((u + 1.0) * 0.5) * (1u32 << 14) as f64) as u32,
            y: (1u32 << 13) - 1,
        };
        let graded_lift = (displayed(graded) - 1.0) * radius - true_height;
        assert!(
            graded_lift.abs() < 0.001,
            "graded coast moved {graded_lift} m"
        );
    }

    #[test]
    fn streamed_grid_keeps_physical_vertex_spacing_when_segment_count_halves() {
        for altitude in [0.000003, 0.0001, 0.001, 0.01, 0.1, 0.5, 1.0] {
            let previous_level =
                ((1.0_f64 / altitude).log2().floor() as i32 + 2).clamp(0, 23) as u8;
            let level = camera_level(altitude);
            let segments = if previous_level < 2 {
                STREAM_SEGMENTS
            } else {
                DEEP_STREAM_SEGMENTS
            };
            assert_eq!(
                level,
                previous_level + if previous_level < 2 { 1 } else { 2 }
            );
            assert_eq!(
                segments * (1usize << level),
                128 * (1usize << previous_level),
            );
        }
        let root = TileKey {
            face: 4,
            ..Default::default()
        };
        assert_eq!(canonical_tile_vertex_count(root, 128), 129 * 129);
        for child in lod::children(root) {
            assert_eq!(canonical_tile_vertex_count(child, 128), 65 * 65);
            for grandchild in lod::children(child) {
                assert_eq!(canonical_tile_vertex_count(grandchild, 128), 65 * 65);
                for deep_child in lod::children(grandchild) {
                    assert_eq!(canonical_tile_vertex_count(deep_child, 128), 33 * 33);
                    assert!(valid_tile_vertex_count(deep_child, 33 * 33, 128));
                    assert!(valid_tile_vertex_count(deep_child, 65 * 65, 128));
                    assert!(!valid_tile_vertex_count(deep_child, 17 * 17, 128));
                    assert!(!valid_tile_vertex_count(deep_child, 129 * 129, 128));
                }
            }
        }
    }

    #[test]
    fn first_64_segment_children_repartition_the_exact_128_segment_root_lattice() {
        let root = TileKey {
            face: 4,
            ..Default::default()
        };
        let root_side = 129;
        let root_at = |index: usize| {
            let u = 2.0 * (index % root_side) as f64 / 128.0 - 1.0;
            let v = 2.0 * (index / root_side) as f64 / 128.0 - 1.0;
            let direction = cube_direction(4, u, v);
            // Non-flat relief ensures this queries the actual parent surface.
            let radius = 1.0 + 0.001 * (17.0 * u).sin() * (11.0 * v).cos();
            (direction * radius, direction.as_vec3().to_array())
        };
        for child in lod::children(root) {
            for y in 0..=STREAM_SEGMENTS {
                for x in 0..=STREAM_SEGMENTS {
                    let direction = cube_direction(
                        child.face,
                        (child.x as f64 + x as f64 / 64.0) - 1.0,
                        (child.y as f64 + y as f64 / 64.0) - 1.0,
                    );
                    let parent_index =
                        (child.y as usize * 64 + y) * root_side + child.x as usize * 64 + x;
                    let expected = root_at(parent_index).0;
                    let parent_u = 2.0 * (parent_index % root_side) as f64 / 128.0 - 1.0;
                    let parent_v = 2.0 * (parent_index / root_side) as f64 / 128.0 - 1.0;
                    assert_eq!(cube_direction(root.face, parent_u, parent_v), direction);
                    // This is the same mixed-resolution fallback as parent_source.
                    let hit = surface_query(direction, root, root_side, root_at).unwrap();
                    assert!((hit.0 - expected.length()).abs() < 2e-12);
                    assert!(Vec3::from_array(hit.1).is_finite());
                }
            }
        }
    }

    #[test]
    fn deep_32_segment_children_repartition_exact_64_segment_terrain_and_coast() {
        let parent = TileKey {
            face: 4,
            level: 2,
            x: 2,
            y: 2,
        };
        let radius = 1_737_000.0;
        let exaggeration = 5.0;
        let make_tile = |key: TileKey, segments: usize| {
            let side = segments + 1;
            let count = (1u32 << key.level) as f64;
            let vertices = (0..side * side)
                .map(|i| {
                    let u =
                        2.0 * (key.x as f64 + (i % side) as f64 / segments as f64) / count - 1.0;
                    let v =
                        2.0 * (key.y as f64 + (i / side) as f64 / segments as f64) / count - 1.0;
                    let height = f64::from(((u - 0.25) * 120.0 + 4.0 * (19.0 * v).sin()) as f32);
                    Sample {
                        direction: cube_direction(key.face, u, v).to_array(),
                        height_m: height,
                        water_m: height.max(0.0),
                        water_level_m: Some(0.0),
                        water_body_id: Some(0),
                        ocean: height < 0.0,
                        biome: "Forest".into(),
                        biome_mix: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
                        ..Default::default()
                    }
                })
                .collect();
            let indices = (0..segments)
                .flat_map(|y| {
                    (0..segments).flat_map(move |x| {
                        let a = (y * side + x) as u32;
                        let b = a + 1;
                        let c = a + side as u32;
                        let d = c + 1;
                        [a, b, d, a, d, c]
                    })
                })
                .collect();
            Tile {
                key,
                vertices,
                indices,
                ..Default::default()
            }
        };
        let tile = make_tile(parent, STREAM_SEGMENTS);
        let mut mesh = tile_mesh(&tile, radius, exaggeration).unwrap();
        compact_mesh(&mut mesh, tile.vertices.len());
        let is_ocean = |v: &Vertex| v.color[3].round() as u32 & 1 != 0 && v.fields[2] == -1.0;
        let sea_start = mesh.vertices.iter().position(is_ocean).unwrap();
        let mut wet = 0;
        let mut dry = 0;
        for child in lod::children(parent) {
            let fine = make_tile(child, DEEP_STREAM_SEGMENTS);
            let mut fine_mesh = tile_mesh(&fine, radius, exaggeration).unwrap();
            compact_mesh(&mut fine_mesh, fine.vertices.len());
            let fine_sea = fine_mesh.vertices.iter().position(is_ocean).unwrap();
            for (i, sample) in fine.vertices.iter().enumerate() {
                let direction = DVec3::from_array(sample.direction);
                let ground = surface_query(direction, parent, 65, |j| {
                    let v = mesh.vertices[j];
                    (mesh_point(&v), v.shading_normal)
                })
                .unwrap()
                .0;
                let (sea, depth) = surface_scalar(direction, parent, 65, |j| {
                    let v = mesh.vertices[sea_start + j];
                    (mesh_point(&v), f64::from(v.fields[3]))
                })
                .unwrap();
                let fine_ground = mesh_point(&fine_mesh.vertices[i]).length();
                let fine_water = fine_mesh.vertices[fine_sea + i];
                assert!((ground - fine_ground).abs() < 2e-12);
                assert!((sea - mesh_point(&fine_water).length()).abs() < 2e-12);
                assert!((depth - f64::from(fine_water.fields[3])).abs() < 1e-8);
                for progress in [0.0, 0.5, 1.0] {
                    let displayed = ground * (1.0 - progress) + fine_ground * progress;
                    assert!(((displayed - fine_ground) * radius / exaggeration).abs() < 1e-6);
                    assert_eq!(depth > 0.0, fine_water.fields[3] > 0.0);
                }
                wet += usize::from(depth > 0.0);
                dry += usize::from(depth <= 0.0);
            }
        }
        assert!(
            wet > 100 && dry > 100,
            "fixture must exercise both marine and dry ground"
        );
    }

    #[test]
    fn canonical_cube_corners_are_shared_by_three_faces() {
        let mut counts = HashMap::new();
        for face in 0..6 {
            for x in [0, 16] {
                for y in [0, 16] {
                    *counts.entry(cube_key(face, x, y, 16)).or_insert(0) += 1;
                }
            }
        }
        assert_eq!(counts.len(), 8);
        assert!(counts.values().all(|&count| count == 3));
    }

    #[test]
    fn split_positions_preserve_submillimeter_offsets() {
        let sample = Sample {
            direction: [0.2, 0.3, 0.4],
            height_m: 0.003,
            ..Default::default()
        };
        let v = vertex(&sample, 1_737_000.0, 1.0, false);
        let reconstructed =
            Vec3::from_array(v.position).as_dvec3() + Vec3::from_array(v.position_low).as_dvec3();
        let expected =
            DVec3::from_array(sample.direction).normalize() * (1.0 + 0.003 / 1_737_000.0);
        assert!((reconstructed - expected).length() * 1_737_000.0 < 0.000001);
    }

    #[test]
    fn inspection_light_reveals_slopes_on_every_hemisphere_and_keeps_orbital_sun() {
        for direction in [
            DVec3::X,
            -DVec3::X,
            DVec3::Y,
            -DVec3::Y,
            DVec3::Z,
            -DVec3::Z,
            DVec3::ONE.normalize(),
            DVec3::new(-0.151, -0.841, 0.519).normalize(),
        ] {
            for altitude in [0.000001, 0.01, 0.02, 0.05, 0.1, 0.2, 2.2] {
                let light = inspection_light(direction, altitude).as_dvec3();
                assert!(light.is_finite());
                assert!((light.length() - 1.0).abs() < 1e-6);
                if altitude <= 0.02 {
                    assert!((light.dot(direction) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-6);
                }
                if altitude >= 0.2 {
                    assert!(light.dot(DVec3::new(-0.45, 0.8, 0.55).normalize()) > 0.999999);
                }
            }
        }
    }

    #[test]
    fn invalid_tile_indices_are_rejected() {
        assert!(tile_mesh(
            &Tile {
                key: TileKey::default(),
                vertices: vec![Sample::default()],
                indices: vec![5],
                ..Default::default()
            },
            100.0,
            1.0
        )
        .is_err());
    }

    #[test]
    fn binary_and_json_tiles_produce_equivalent_rendered_surfaces() {
        let world = world_core::World::generate(world_core::WorldConfig {
            seed: 7,
            resolution: 16,
            erosion_steps: 0,
            ..Default::default()
        })
        .unwrap();
        let source = world
            .tile(
                world_core::geometry::TileKey {
                    face: 4,
                    level: 2,
                    x: 1,
                    y: 2,
                },
                4,
            )
            .unwrap();
        let json = decode_json_tile(&serde_json::to_string(&source).unwrap()).unwrap();
        let bytes = world_core::tile_wire::encode_tile(&source).unwrap();
        let binary = from_core_tile(world_core::tile_wire::decode_tile(&bytes).unwrap());
        let a = tile_mesh(&json, world.config.radius_m, 1.0).unwrap();
        let b = tile_mesh(&binary, world.config.radius_m, 1.0).unwrap();
        assert_eq!(a.indices, b.indices);
        assert_eq!(a.vertices.len(), b.vertices.len());
        for (a, b) in a.vertices.iter().zip(&b.vertices) {
            let position = |v: &Vertex| {
                DVec3::from_array(v.position.map(f64::from))
                    + DVec3::from_array(v.position_low.map(f64::from))
            };
            assert!((position(a) - position(b)).length() * world.config.radius_m < 0.001);
            for (ca, cb) in a.color.iter().zip(b.color) {
                assert!((*ca - cb).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn lake_shore_is_clipped_against_its_level_inside_the_terrain_triangle() {
        let samples: Vec<_> = [[0.0, 0.0, 1.0], [0.001, 0.0, 1.0], [0.0, 0.001, 1.0]]
            .into_iter()
            .zip([-10.0, 10.0, 10.0])
            .map(|(direction, height_m)| Sample {
                direction,
                height_m,
                water_m: height_m.max(2.0),
                water_level_m: Some(2.0),
                water_body_id: Some(7),
                lake: height_m < 2.0,
                ..Default::default()
            })
            .collect();
        let radius = 2_000_000.0;
        let mesh = mesh_from_samples(&samples, &[0, 1, 2], 3, None, radius, 1.0).unwrap();
        let water_indices = &mesh.indices[3..];
        assert_eq!(water_indices.len(), 3);
        let mut shore_count = 0;
        for &i in water_indices {
            let v = mesh.vertices[i as usize];
            let position = DVec3::from_array(v.position.map(f64::from))
                + DVec3::from_array(v.position_low.map(f64::from));
            assert!(((position.length() - 1.0) * radius - 2.0).abs() < 1e-5);
            assert!(v.fields[3] >= 0.0);
            if v.fields[3] == 0.0 {
                shore_count += 1;
                assert!((f64::from(v.normal_height[3]) * 1000.0 - 2.0).abs() < 1e-5);
            }
            let normal = DVec3::from_array(v.shading_normal.map(f64::from));
            assert!(normal.normalize().dot(position.normalize()) > 0.9999999);
        }
        assert_eq!(shore_count, 2);
        let mut dry = samples.clone();
        for s in &mut dry {
            s.height_m = 3.0;
        }
        let dry_mesh = mesh_from_samples(&dry, &[0, 1, 2], 3, None, radius, 1.0).unwrap();
        assert_eq!(dry_mesh.indices, [0, 1, 2]);
    }

    #[test]
    fn river_water_does_not_flood_vertices_outside_its_hydraulic_domain() {
        let mut samples = vec![
            Sample {
                direction: [0.0, 0.0, 1.0],
                height_m: 8.0,
                water_m: 10.0,
                water_level_m: Some(10.0),
                water_body_id: Some(RIVER_BODY + 1),
                river: 1.0,
                ..Default::default()
            },
            Sample {
                direction: [0.001, 0.0, 1.0],
                height_m: 5.0,
                water_m: 5.0,
                ..Default::default()
            },
            Sample {
                direction: [0.0, 0.001, 1.0],
                height_m: 5.0,
                water_m: 5.0,
                ..Default::default()
            },
        ];
        for dry_support in [false, true] {
            if dry_support {
                samples[1].water_level_m = Some(10.0);
                samples[1].water_body_id = Some(RIVER_BODY + 1);
            }
            let mesh = mesh_from_samples(&samples, &[0, 1, 2], 4, None, 2_000_000.0, 1.0).unwrap();
            let water_indices = &mesh.indices[3..];
            assert_eq!(water_indices.len(), 3);
            // Both outside vertices are lower than the river, but outside its wet section.
            assert!(!water_indices.contains(&4));
            assert!(!water_indices.contains(&5));
            let mut boundaries = 0;
            for &index in water_indices {
                let vertex = mesh.vertices[index as usize];
                assert!(vertex.normal_height[0].abs() < 0.0003);
                assert!(vertex.normal_height[1].abs() < 0.0003);
                if vertex.fields[3] == 0.0 {
                    boundaries += 1;
                }
            }
            assert_eq!(boundaries, 2);
        }
    }

    #[test]
    fn visual_resolution_is_independent_of_the_physical_atlas() {
        let mut cells = Vec::new();
        for face in 0..6 {
            let b = BASES[face];
            for y in 0..2 {
                for x in 0..2 {
                    let vec = |a: [i32; 3]| DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
                    cells.push(Sample {
                        direction: (vec(b[0])
                            + vec(b[1]) * (x as f64 - 0.5)
                            + vec(b[2]) * (y as f64 - 0.5))
                            .normalize()
                            .to_array(),
                        height_m: 100.0,
                        water_m: 100.0,
                        ..Default::default()
                    });
                }
            }
        }
        let world = World {
            config: Config {
                seed: 42,
                radius_m: 2_000_000.0,
                resolution: 512,
            },
            render_resolution: Some(2),
            render_grid: None,
            cells,
        };
        let mesh = world_mesh(&world, 1.0).unwrap();
        assert_eq!(
            mesh.indices
                .chunks_exact(3)
                .filter(|t| mesh.vertices[t[0] as usize].color[3].round() as u32 & 1 == 0)
                .count()
                * 3,
            6 * 2 * 2 * 6
        );
        let surfaces = mesh_surfaces(&mesh, world.mesh_resolution());
        assert_eq!(surfaces[0].len(), 6 * 3 * 3);
        assert_eq!(surfaces[1].len(), 6 * 3 * 3);
    }

    #[test]
    fn planetary_interpolation_does_not_expand_a_river_profile_into_a_lake() {
        let world = World {
            config: Config {
                seed: 42,
                radius_m: 2_000_000.0,
                resolution: 2,
            },
            render_resolution: None,
            render_grid: None,
            cells: vec![
                Sample {
                    height_m: 100.0,
                    water_m: 102.0,
                    water_level_m: Some(102.0),
                    water_body_id: Some(RIVER_BODY + 12),
                    river: 0.8,
                    ..Default::default()
                };
                24
            ],
        };
        let sample = atlas_sample(&world, DVec3::Z);
        assert_eq!(sample.water_level_m, None);
        assert_eq!(sample.water_m, sample.height_m);
        assert_eq!(sample.river, 0.8);
        let mesh = world_mesh(&world, 1.0).unwrap();
        assert_eq!(
            mesh.indices
                .chunks_exact(3)
                .filter(|t| mesh.vertices[t[0] as usize].color[3].round() as u32 & 1 == 0)
                .count()
                * 3,
            6 * 2 * 2 * 6
        );
    }

    #[test]
    fn river_surface_keeps_its_downstream_gradient_during_lod_morph() {
        let samples: Vec<_> = [[0.0, 0.0, 1.0], [0.001, 0.0, 1.0], [0.0, 0.001, 1.0]]
            .into_iter()
            .zip([14.0, 12.0, 10.0])
            .map(|(direction, level)| Sample {
                direction,
                height_m: level - 4.0,
                water_m: level,
                water_level_m: Some(level),
                water_body_id: Some(RIVER_BODY + 4),
                river: 1.0,
                ..Default::default()
            })
            .collect();
        let offsets = [[0.0001, 0.0002, 4.0]; 3];
        let radius = 2_000_000.0;
        let mesh =
            mesh_from_samples(&samples, &[0, 1, 2], 3, Some((&offsets, 1.0)), radius, 1.0).unwrap();
        assert_eq!(mesh.indices.len(), 6);
        for (i, sample) in samples.iter().enumerate() {
            let v = mesh.vertices[3 + i];
            let point = DVec3::from_array(v.position.map(f64::from))
                + DVec3::from_array(v.position_low.map(f64::from));
            assert!(((point.length() - 1.0) * radius - sample.water_m).abs() < 1e-5);
            assert_eq!(v.morph[0], 0.0);
            assert_eq!(
                v.fields[2], -2.0,
                "unclipped river triangles must share the overlay mask"
            );
        }
    }

    #[test]
    fn orbital_river_overlay_bounds_dense_chords_without_repeated_subdivision() {
        let rivers = RiverLayer {
            segments: (0..32_769)
                .map(|_| RiverSegment {
                    start: [0.0, 0.0, 1.0],
                    end: [0.00005, 0.0, 1.0],
                    start_height_m: 10.0,
                    end_height_m: 0.0,
                    width_m: 20.0,
                })
                .collect(),
        };
        let mesh = object_mesh(&rivers, &HumanLayer::default(), 2_000_000.0, 1.0);
        assert_eq!(mesh.vertices.len(), 32_768 * 4);
        assert_eq!(mesh.indices.len(), 32_768 * 6);
    }

    #[test]
    fn camera_intersection_uses_the_rendered_triangle_plane() {
        let a = DVec3::new(-1.0, -1.0, 2.0);
        let b = DVec3::new(1.0, -1.0, 4.0);
        let c = DVec3::new(1.0, 1.0, 2.0);
        assert!((ray_triangle(DVec3::Z, a, b, c).unwrap() - 2.0).abs() < 1e-12);
        assert!(ray_triangle(-DVec3::Z, a, b, c).is_none());
    }

    #[test]
    fn morph_has_exact_endpoints_and_stays_bounded() {
        assert_eq!(morph_progress(2.0, 3.0), 0.0);
        assert_eq!(morph_progress(4.0, 3.0), 1.0);
        assert!((morph_progress(3.0 + MORPH_DURATION_SECONDS * 0.5, 3.0) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn shorter_morph_keeps_inverse_geometry_symmetric_and_releases_sources_at_its_end() {
        let start = 10.0;
        let parent_radius = 1.000123;
        let refined_radius = 0.999456;
        let mut previous = 0.0;
        for frame in 0..=10 {
            let elapsed = MORPH_DURATION_SECONDS * frame as f64 / 10.0;
            let forward = morph_progress(start + elapsed, start);
            let reverse = morph_progress(start + MORPH_DURATION_SECONDS - elapsed, start);
            assert!(forward >= previous && (0.0..=1.0).contains(&forward));
            let refining = parent_radius * (1.0 - forward) + refined_radius * forward;
            let coarsening = refined_radius * (1.0 - reverse) + parent_radius * reverse;
            assert!((refining - coarsening).abs() < 1e-14);
            previous = forward;
        }
        assert_eq!(morph_progress(start + MORPH_DURATION_SECONDS, start), 1.0);
        assert!(MORPH_DURATION_SECONDS < 0.18);
    }

    #[test]
    fn preview_vertices_keep_native_samples_and_root_child_common_positions() {
        let source = world_core::World::generate(world_core::WorldConfig {
            seed: 71,
            resolution: 16,
            erosion_steps: 0,
            ..Default::default()
        })
        .unwrap();
        let mut cells = Vec::new();
        let mut expected = Vec::new();
        for face in 0..6 {
            let tile = source
                .tile(
                    world_core::geometry::TileKey {
                        face,
                        level: 0,
                        x: 0,
                        y: 0,
                    },
                    8,
                )
                .unwrap();
            cells.extend(tile.vertices.iter().cloned());
            expected.push(tile);
        }
        let preview = serde_json::json!({
            "config": { "radius_m": source.config.radius_m, "resolution": 512 },
            "render_resolution": 8,
            "render_grid": "vertices",
            "cells": cells,
        });
        let world: World = serde_json::from_value(preview).unwrap();
        let roots = root_tiles(&world).unwrap();
        for (root, expected) in roots.iter().zip(expected) {
            assert_eq!(root.indices, expected.indices);
            for (sample, native) in root.vertices.iter().zip(expected.vertices) {
                assert_eq!(sample.direction, native.direction);
                assert_eq!(sample.height_m, f64::from(native.height_m));
                assert_eq!(sample.water_m, f64::from(native.water_m));
                assert_eq!(sample.water_level_m, native.water_level_m.map(f64::from));
            }
            for child in lod::children(root.key) {
                let native = source
                    .tile(
                        world_core::geometry::TileKey {
                            face: child.face,
                            level: child.level,
                            x: child.x,
                            y: child.y,
                        },
                        8,
                    )
                    .unwrap();
                for y in 0..=4 {
                    for x in 0..=4 {
                        let parent = &root.vertices
                            [(child.y as usize * 4 + y) * 9 + child.x as usize * 4 + x];
                        let fine = &native.vertices[y * 2 * 9 + x * 2];
                        assert_eq!(parent.direction, fine.direction);
                        assert_eq!(parent.height_m, f64::from(fine.height_m));
                    }
                }
            }
        }
    }

    #[test]
    fn ocean_shell_supports_both_morph_directions_without_flooding_closed_basins() {
        let mut samples: Vec<_> = [[0.0, 0.0, 1.0], [0.01, 0.0, 1.0], [0.0, 0.01, 1.0]]
            .into_iter()
            .map(|direction| Sample {
                direction,
                height_m: 10.0,
                ..Default::default()
            })
            .collect();
        for candidate in [false, true] {
            for sample in &mut samples {
                sample.height_m = if candidate { 10.0 } else { -10.0 };
                sample.water_level_m = candidate.then_some(0.0);
                sample.water_body_id = candidate.then_some(0);
            }
            let mut mesh =
                mesh_from_samples_inner(&samples, &[0, 1, 2], 1, None, 2_000_000.0, 1.0, true)
                    .unwrap();
            compact_mesh(&mut mesh, samples.len());
            let shell: Vec<_> = mesh.vertices.iter().filter(|v| v.fields[2] < 0.0).collect();
            assert_eq!(
                shell.len(),
                3,
                "a final dry triangle still needs its parent-water shell"
            );
            assert!(shell.iter().all(|v| v.fields[3] < 0.0));
            // Geometry remains resident even when all final depths are dry;
            // a parent-wet morph can therefore expose it through dynamic depths.
            let shell_indices = &mesh.indices[mesh.indices.len() - 3..];
            assert!(shell_indices
                .iter()
                .all(|&i| mesh.vertices[i as usize].fields[2] < 0.0));
        }
        for sample in &mut samples {
            sample.height_m = -10.0;
            sample.water_level_m = Some(0.0);
            sample.water_body_id = Some(0);
        }
        let mesh =
            mesh_from_samples_inner(&samples, &[0, 1, 2], 1, None, 2_000_000.0, 1.0, true).unwrap();
        assert!(mesh
            .vertices
            .iter()
            .filter(|v| v.fields[2] < 0.0)
            .all(|v| v.fields[3] == 10.0));
    }

    #[test]
    fn independent_morph_epochs_never_restart_a_cached_tile() {
        let mut epochs = [MorphEpoch::default(); 32];
        epochs[1] = MorphEpoch { id: 1, start: 1e9 };
        assert_eq!(epoch_progress(-1.0, 10.0, &epochs), 0.0);
        epochs[1].start = 10.0;
        assert!(
            (epoch_progress(-1.0, 10.0 + MORPH_DURATION_SECONDS * 0.5, &epochs) - 0.5).abs()
                < 1e-12
        );
        epochs[2] = MorphEpoch { id: 2, start: 11.0 };
        assert_eq!(epoch_progress(-1.0, 11.0, &epochs), 1.0);
        assert_eq!(epoch_progress(-2.0, 11.0, &epochs), 0.0);
        epochs[1] = MorphEpoch {
            id: 33,
            start: 12.0,
        };
        assert_eq!(epoch_progress(-1.0, 12.0, &epochs), 1.0);
        assert_eq!(epoch_progress(-33.0, 12.0, &epochs), 0.0);
    }

    #[test]
    fn instant_commits_cannot_recycle_a_visible_epoch_before_its_morph_finishes() {
        let mut epochs = [MorphEpoch::default(); 32];
        let start = 10.0;
        epochs[1] = MorphEpoch {
            id: 1,
            start: start as f32,
        };
        let mut last = 1;
        for frame in 1..32 {
            // Wrap the ring while the first visible morph is still running,
            // independently of the configured transition duration.
            let time = start + MORPH_DURATION_SECONDS * frame as f64 / 64.0;
            let id = next_morph_epoch(last, time, &epochs).unwrap();
            epochs[id as usize % 32] = MorphEpoch {
                id,
                start: (time - MORPH_DURATION_SECONDS) as f32,
            };
            last = id;
            assert_eq!(
                epoch_progress(-1.0, time, &epochs),
                morph_progress(time, start)
            );
        }
        let time = start + MORPH_DURATION_SECONDS * 0.5;
        let visible_progress = epoch_progress(-1.0, time, &epochs);
        assert!(visible_progress < 1.0);
        assert_eq!(next_morph_epoch(last, time, &epochs), None);
        assert_eq!(epoch_progress(-1.0, time, &epochs), visible_progress);
        let finished = start + MORPH_DURATION_SECONDS;
        assert_eq!(next_morph_epoch(last, finished, &epochs), Some(33));
        epochs[1] = MorphEpoch { id: 33, start: 1e9 };
        assert_eq!(epoch_progress(-1.0, finished, &epochs), 1.0);
        assert_eq!(next_morph_epoch(last, finished, &epochs), None);
        // Cancellation releases the future preparation sentinel, without
        // resetting IDs and restarting any cached tile's old epoch.
        epochs[1].start = -1e9;
        assert_eq!(next_morph_epoch(last, finished, &epochs), Some(33));
        assert_eq!(next_morph_epoch(last, f64::NAN, &epochs), None);
    }

    #[test]
    fn clearing_mid_preparation_restores_retained_root_normals_and_water_depth() {
        let normal = [0.0, 0.0, 1.0];
        let mut epochs = [MorphEpoch::default(); 32];
        epochs[7] = MorphEpoch { id: 7, start: 1e9 };
        // An uploaded root Edge keeps its old coastline until the proposed
        // partition commits, but its future normal and depth can differ.
        let prepared_edge = DynamicVertex {
            morph: [0.0, -7.0, 3.0, 0.0],
            parent_normal: normal,
            shading_normal: [0.6, 0.0, 0.8],
            water_depth: -2.0,
        };
        assert_eq!(epoch_progress(prepared_edge.morph[1], 10.0, &epochs), 0.0);
        epochs[7].start = -1e9;
        assert_eq!(epoch_progress(prepared_edge.morph[1], 10.0, &epochs), 1.0);
        assert_ne!(prepared_edge.shading_normal, normal);
        assert!(prepared_edge.water_depth < 0.0);
        // clear_tiles retains roots only and rebuilds this same settled
        // dynamic state from each root's immutable SurfaceVertex.
        let restored = DynamicVertex::settled(normal, 3.0);
        for time in [10.0, 10.08, 11.0] {
            let progress = epoch_progress(restored.morph[1], time, &epochs) as f32;
            assert_eq!(
                restored.morph[0] * (1.0 - progress) + restored.morph[3] * progress,
                0.0
            );
            assert_eq!(
                restored.morph[2] * (1.0 - progress) + restored.water_depth * progress,
                3.0
            );
            assert_eq!(restored.parent_normal, normal);
            assert_eq!(restored.shading_normal, normal);
        }
        assert_eq!(std::mem::size_of::<DynamicVertex>(), 44);
    }

    #[test]
    fn transition_guard_includes_every_fine_leaf_along_a_coarse_cube_edge() {
        let parent = TileKey {
            face: 4,
            ..Default::default()
        };
        let mut cut = lod::initial_leaves();
        for level in 0..5 {
            let edge: Vec<_> = cut
                .iter()
                .copied()
                .filter(|key| key.face == 0 && key.level == level && key.x == 0)
                .collect();
            for key in edge {
                assert!(lod::split(&mut cut, key));
            }
        }
        let refining = transition_neighbors(parent, &cut, false);
        assert_eq!(refining.len(), 32);
        assert!(refining
            .iter()
            .all(|key| key.face == 0 && key.level == 5 && key.x == 0));
        assert!(lod::split(&mut cut, parent));
        let coarsening = transition_neighbors(parent, &cut, true);
        assert_eq!(
            coarsening
                .iter()
                .copied()
                .filter(|key| key.level > 0)
                .collect::<Vec<_>>(),
            refining
        );
        assert!(coarsening.iter().any(|key| key.level == 0));
    }

    #[test]
    fn nested_chord_source_matches_parent_triangles_at_orbital_and_local_scales() {
        for face in 0..6 {
            for level in [0, 8, 16] {
                let parent = TileKey {
                    face,
                    level,
                    x: (1u32 << level) / 2,
                    y: (1u32 << level) / 2,
                };
                let side = 9;
                let at = |index: usize| {
                    let x = index % side;
                    let y = index / side;
                    let count = (1u32 << level) as f64;
                    let d = cube_direction(
                        face,
                        2.0 * (parent.x as f64 + x as f64 / 8.0) / count - 1.0,
                        2.0 * (parent.y as f64 + y as f64 / 8.0) / count - 1.0,
                    );
                    let radius = 1.001 + (x as f64 + y as f64 * 0.7) * 0.001 / count;
                    (d * radius, d.as_vec3().to_array(), x as f64 - y as f64)
                };
                for child in lod::children(parent) {
                    for index in 0..side * side {
                        let count = (1u32 << child.level) as f64;
                        let d = cube_direction(
                            face,
                            2.0 * (child.x as f64 + (index % side) as f64 / 8.0) / count - 1.0,
                            2.0 * (child.y as f64 + (index / side) as f64 / 8.0) / count - 1.0,
                        );
                        let nested = nested_parent_source(child, index, side, d, at).unwrap();
                        let triangle = surface_query(d, parent, side, |i| {
                            let (p, n, _) = at(i);
                            (p, n)
                        })
                        .unwrap();
                        let scalar = surface_scalar(d, parent, side, |i| {
                            let (p, _, s) = at(i);
                            (p, s)
                        })
                        .unwrap();
                        assert!(
                            (nested.0 - triangle.0).abs() < 1e-10,
                            "face{face} level{level} index{index}"
                        );
                        assert!((nested.2 - scalar.1).abs() < 1e-5);
                    }
                }
            }
        }
    }

    #[test]
    fn frustum_spheres_include_touching_edges_and_reject_offscreen_groups() {
        let frame = (DVec3::ZERO, -DVec3::Z, DVec3::X, DVec3::Y);
        assert!(sphere_in_frustum(
            DVec3::new(0.0, 0.0, -5.0),
            0.1,
            frame,
            1.6
        ));
        assert!(!sphere_in_frustum(
            DVec3::new(10.0, 0.0, -5.0),
            0.1,
            frame,
            1.6
        ));
        assert!(!sphere_in_frustum(
            DVec3::new(0.0, 10.0, -5.0),
            0.1,
            frame,
            1.6
        ));
        assert!(!sphere_in_frustum(
            DVec3::new(0.0, 0.0, 5.0),
            0.1,
            frame,
            1.6
        ));
        let edge = 5.0 * 22.5f64.to_radians().tan() * 1.6;
        assert!(sphere_in_frustum(
            DVec3::new(edge, 0.0, -5.0),
            0.1,
            frame,
            1.6
        ));
        assert!(sphere_in_frustum(
            DVec3::new(edge + 0.1, 0.0, -5.0),
            0.1,
            frame,
            1.6
        ));
    }

    #[test]
    fn projected_sphere_bound_contains_perspective_points_off_axis_and_at_tilt() {
        for tilt in [0.0, 35f64.to_radians(), 60f64.to_radians()] {
            let frame = camera_frame(DVec3::new(0.3, 0.4, 0.8).normalize(), 1.0, tilt, 1.0);
            let (eye, forward, right, up) = frame;
            for (x, y, depth, radius) in [
                (0.0, 0.0, 10.0, 0.3),
                (8.0, 4.0, 10.0, 0.3),
                (10.0, -10.0, 0.5, 0.49),
            ] {
                let center = eye + right * x + up * y + forward * depth;
                for height in [400, 800, 1600] {
                    let bound = projected_diameter_pixels(center, radius, frame, height);
                    let focal = height as f64 / (2.0 * 22.5f64.to_radians().tan());
                    let mut points = Vec::new();
                    for x in -1..=1 {
                        for y in -1..=1 {
                            for z in -1..=1 {
                                let offset = DVec3::new(x as f64, y as f64, z as f64)
                                    .normalize_or_zero()
                                    * radius;
                                let p = center + offset - eye;
                                let d = p.dot(forward);
                                points.push(glam::DVec2::new(p.dot(right), p.dot(up)) * focal / d);
                            }
                        }
                    }
                    for a in &points {
                        for b in &points {
                            assert!(
                                (*a - *b).length() <= bound + 1e-7,
                                "perspective diameter escaped its off-axis sphere bound"
                            );
                        }
                    }
                }
            }
            assert!(projected_diameter_pixels(eye + forward * 0.1, 0.11, frame, 800).is_infinite());
        }
        let frame = (DVec3::ZERO, -DVec3::Z, DVec3::X, DVec3::Y);
        let bounds = [(DVec3::new(0.0, 0.0, -100.0), 0.01)];
        assert!(instant_coarsen_bounds(&bounds, frame, 1.4, 400));
        assert!(!instant_coarsen_bounds(&bounds, frame, 1.4, 1600));
        assert!(!instant_coarsen_bounds(
            &[(DVec3::new(0.0, 0.0, -0.01), 0.02)],
            frame,
            1.4,
            800
        ));
        assert!(!instant_coarsen_bounds(
            &[(DVec3::NAN, 0.01)],
            frame,
            1.4,
            800
        ));
        assert!(!instant_coarsen_bounds(&[], frame, 1.4, 800));
        for invalid in [
            (DVec3::NAN, frame.1, frame.2, frame.3),
            (frame.0, DVec3::NAN, frame.2, frame.3),
            (frame.0, frame.1, DVec3::NAN, frame.3),
            (frame.0, frame.1, frame.2, DVec3::NAN),
        ] {
            assert!(!instant_coarsen_bounds(&bounds, invalid, 1.4, 800));
        }
        assert!(!instant_coarsen_bounds(&bounds, frame, f64::NAN, 800));
        assert!(!instant_coarsen_bounds(&bounds, frame, 0.0, 800));
        assert!(!instant_coarsen_bounds(&bounds, frame, 1.4, 0));
    }

    #[test]
    fn camera_frame_and_render_rotation_stay_finite_at_both_poles() {
        for sign in [-1.0, 1.0] {
            for direction in [DVec3::Y * sign, DVec3::new(1e-9, sign, 0.0).normalize()] {
                for altitude in [5.0 / 1_737_000.0, 1.0] {
                    for tilt in [0.0, 35f64.to_radians(), 60f64.to_radians()] {
                        let (eye, forward, right, up) =
                            camera_frame(direction, altitude, tilt, 1.01);
                        assert!([eye, forward, right, up].iter().all(|v| v.is_finite()));
                        for axis in [forward, right, up] {
                            assert!((axis.length() - 1.0).abs() < 1e-12);
                        }
                        assert!(forward.dot(right).abs() < 1e-12);
                        assert!(forward.dot(up).abs() < 1e-12);
                        assert!(right.dot(up).abs() < 1e-12);
                        let rotation =
                            glam::Mat4::look_to_rh(Vec3::ZERO, forward.as_vec3(), up.as_vec3());
                        assert!(rotation.to_cols_array().iter().all(|v| v.is_finite()));
                        let rendered_right =
                            Vec3::new(rotation.x_axis.x, rotation.y_axis.x, rotation.z_axis.x);
                        assert!((rendered_right.as_dvec3() - right).length() < 1e-6);
                    }
                }
            }
        }
    }

    #[test]
    fn inclined_camera_support_keeps_its_morph_even_when_the_nadir_is_outside_view() {
        let direction = cube_direction(4, 0.13, 0.08);
        let level = 18;
        let key_at = |direction| {
            let (face, u, v) = face_uv(direction);
            let count = 1u32 << level;
            TileKey {
                face: face as u8,
                level,
                x: (((u + 1.0) * 0.5 * count as f64) as u32).min(count - 1),
                y: (((v + 1.0) * 0.5 * count as f64) as u32).min(count - 1),
            }
        };
        for tilt in [35f64.to_radians(), 60f64.to_radians()] {
            let frame = camera_frame(direction, 1_000.0 / 1_737_000.0, tilt, 1.0);
            let nadir = frame.0.normalize();
            assert!(!sphere_in_frustum(nadir, 1e-6, frame, 1.4));
            assert!(instant_coarsen_bounds(&[(nadir, 1e-6)], frame, 1.4, 800));
            let targets = graduate_targets(&[key_at(direction), key_at(nadir)], level, direction);
            let cut = desired_partition(&targets, level);
            assert_ne!(key_at(direction), key_at(nadir));
            // Both radial samples position the actual camera: one raises its
            // focus, the other keeps the inclined eye above terrain/water.
            for support in [key_at(direction), key_at(nadir)] {
                assert!(cut.contains(&support));
                let parent = lod::parent(support).unwrap();
                let keys: Vec<_> = std::iter::once(parent)
                    .chain(lod::children(parent))
                    .collect();
                assert!(touches_camera_support(&keys, &cut, direction, frame.0));
                assert!(touches_camera_support(&[support], &cut, direction, frame.0));
            }
            let unrelated = TileKey {
                face: 5,
                ..Default::default()
            };
            assert!(!touches_camera_support(
                &[unrelated],
                &cut,
                direction,
                frame.0
            ));
            assert!(touches_camera_support(
                &[unrelated],
                &HashSet::new(),
                direction,
                frame.0
            ));
        }
    }

    #[test]
    fn instantaneous_coarsening_bounds_cover_relief_water_and_morph_and_recheck_the_camera() {
        let radius = 1_737_000.0;
        let samples: Vec<_> = [
            ([0.0, 0.0, 1.0], -10.0),
            ([2e-6, 0.0, 1.0], 20.0),
            ([0.0, 2e-6, 1.0], 5.0),
        ]
        .into_iter()
        .map(|(direction, height)| Sample {
            direction,
            height_m: height,
            water_m: height.max(0.0),
            water_level_m: Some(0.0),
            water_body_id: Some(0),
            ocean: height < 0.0,
            ..Default::default()
        })
        .collect();
        for exaggeration in [1.0, 5.0, 30.0] {
            let mesh =
                mesh_from_samples_inner(&samples, &[0, 1, 2], 3, None, radius, exaggeration, true)
                    .unwrap();
            let (center, size) = mesh_bounds(&mesh);
            let pad = 10.0 * exaggeration / radius;
            for vertex in &mesh.vertices {
                let p = mesh_point(vertex);
                let radial = Vec3::from_array(vertex.normal_height[..3].try_into().unwrap())
                    .as_dvec3()
                    .normalize();
                for delta in [-pad, 0.0, pad] {
                    assert!((p + radial * delta - center).length() <= size + pad + 1e-12);
                }
            }
            let bounds = [(center, size + pad + 1e-6)];
            for tilt in [0.0, 35f64.to_radians()] {
                let distant = camera_frame(DVec3::Z, 7.0, tilt, 1.0);
                assert!(instant_coarsen_bounds(&bounds, distant, 1.4, 800));
                // A zoom or camera motion during mesh preparation must restore
                // the ordinary inverse morph before any visible commit.
                let near = camera_frame(DVec3::Z, 5.0 / radius, tilt, 1.0);
                assert!(!instant_coarsen_bounds(&bounds, near, 1.4, 800));
            }
        }
    }

    #[test]
    fn request_frontier_never_reloads_the_history_of_a_deep_cut() {
        let root = TileKey {
            face: 4,
            ..Default::default()
        };
        let target = TileKey {
            face: 4,
            level: 18,
            x: 262143,
            y: 262143,
        };
        let direction = cube_direction(4, 0.8, 0.8);
        let mut cut = lod::initial_leaves();
        let mut parent = root;
        for level in 0..18 {
            let requests = frontier_keys(&cut, &[target], 18, direction, false);
            let expected: HashSet<_> = lod::children(parent).into_iter().collect();
            assert_eq!(requests.into_iter().collect::<HashSet<_>>(), expected);
            assert!(lod::split(&mut cut, parent));
            parent = lod::children(parent)[3];
            assert_eq!(parent.level, level + 1);
        }
        assert!(frontier_keys(&cut, &[target], 18, direction, false).is_empty());
        for level in (0..18).rev() {
            let requests = frontier_keys(&cut, &[root], 0, direction, true);
            assert_eq!(requests, vec![lod::ancestor(target, level).unwrap()]);
            assert!(lod::coarsen(&mut cut, requests[0]));
        }
        assert_eq!(cut, lod::initial_leaves());
    }

    #[test]
    fn prefetch_projects_one_requested_frontier_without_changing_the_real_cut() {
        let target = TileKey {
            face: 4,
            level: 18,
            x: 209715,
            y: 141557,
        };
        let direction = tile_center(target);
        let mut cut = lod::initial_leaves();
        for level in 0..target.level {
            let parent = lod::ancestor(target, level).unwrap();
            let before = cut.clone();
            let essential = frontier_keys(&cut, &[target], target.level, direction, true);
            let future = prefetch_keys(&cut, &[target], target.level, direction);
            let mut requests = essential.clone();
            append_prefetch(&mut requests, &future);
            assert_eq!(&requests[..essential.len()], essential);
            assert_eq!(cut, before, "forecast changed the displayed partition");
            let mut expected: HashSet<_> = lod::children(parent).into_iter().collect();
            if level + 1 < target.level && split_preserves_grading(parent, &cut) {
                let next = lod::ancestor(target, level + 1).unwrap();
                expected.extend(lod::children(next));
            }
            assert_eq!(future.iter().copied().collect::<HashSet<_>>(), expected);
            assert_eq!(future.len(), expected.len());
            assert!(future
                .iter()
                .all(|key| { key.level > level && key.level <= (level + 2).min(target.level) }));
            assert!(lod::split(&mut cut, parent));
        }
        assert!(prefetch_keys(&cut, &[target], target.level, direction).is_empty());
    }

    #[test]
    fn forecast_center_keeps_its_next_quartet_when_new_fringes_exceed_the_budget() {
        let mut cut = lod::initial_leaves();
        for level in 0..3 {
            let parents: Vec<_> = cut
                .iter()
                .copied()
                .filter(|key| key.face == 4 && key.level == level)
                .collect();
            for parent in parents {
                assert!(lod::split(&mut cut, parent));
            }
        }
        let targets: Vec<_> = (0..8)
            .flat_map(|y| {
                (0..8).map(move |x| TileKey {
                    face: 4,
                    level: 8,
                    x: x * 32 + 16,
                    y: y * 32 + 16,
                })
            })
            .collect();
        let current = targets[0];
        let forecast = targets[36];
        let direction = tile_center(forecast);
        let essential = frontier_keys(&cut, &[current], 8, tile_center(current), true);
        let immediate = frontier_keys(&cut, &targets, 8, direction, false);
        assert!(immediate.len() > MAX_PREFETCH_TILES);
        let future = prefetch_keys(&cut, &targets, 8, direction);
        let mut requests = essential.clone();
        append_prefetch(&mut requests, &future);
        let extras = &requests[essential.len()..];
        let central = lod::children(lod::ancestor(forecast, 3).unwrap());
        let following = lod::children(lod::ancestor(forecast, 4).unwrap());
        assert_eq!(&requests[..essential.len()], essential);
        assert_eq!(&extras[..4], central);
        assert_eq!(&extras[4..8], following);
        assert_eq!(extras.len(), MAX_PREFETCH_TILES);
        let unique: HashSet<_> = requests.iter().copied().collect();
        assert_eq!(unique.len(), requests.len());
        for quartet in future.chunks_exact(4) {
            if quartet.iter().any(|key| unique.contains(key)) {
                assert!(quartet.iter().all(|key| unique.contains(key)));
            }
        }
        assert!(future.iter().all(|key| key.level == 4 || key.level == 5));
    }

    #[test]
    fn prefetch_budget_deduplicates_before_admitting_complete_families() {
        let families: Vec<_> = (0..17)
            .map(|x| {
                lod::children(TileKey {
                    face: 4,
                    level: 5,
                    x,
                    y: 0,
                })
            })
            .collect();
        let future: Vec<_> = families.iter().flatten().copied().collect();
        let first = families[0][0];
        let mut requests = vec![first];
        append_prefetch(&mut requests, &future);
        assert_eq!(requests[0], first);
        assert_eq!(requests.len(), 64);
        let unique: HashSet<_> = requests.iter().copied().collect();
        assert_eq!(unique.len(), requests.len());
        assert!(families[..16]
            .iter()
            .flatten()
            .all(|key| unique.contains(key)));
        assert!(families[16].iter().all(|key| !unique.contains(key)));
    }

    #[test]
    fn forecast_does_not_reload_a_deep_cut_during_zoom_out_or_before_initialization() {
        let target = TileKey {
            face: 4,
            level: 18,
            x: 209715,
            y: 141557,
        };
        let direction = tile_center(target);
        assert!(prefetch_keys(&HashSet::new(), &[target], 18, direction).is_empty());
        let mut cut = lod::initial_leaves();
        for level in 0..target.level {
            assert!(lod::split(&mut cut, lod::ancestor(target, level).unwrap()));
        }
        let orbital = lod::ancestor(target, 3).unwrap();
        let before = cut.clone();
        assert!(prefetch_keys(&cut, &[orbital], 3, direction).is_empty());
        assert_eq!(cut, before);
    }

    #[test]
    fn zoom_out_frontier_keeps_essential_parents_above_the_target_level() {
        let root = TileKey {
            face: 4,
            ..Default::default()
        };
        let deep = TileKey {
            face: 4,
            level: 18,
            x: 262143,
            y: 262143,
        };
        let orbital = lod::ancestor(deep, 1).unwrap();
        let mut cut = lod::initial_leaves();
        let mut parent = root;
        for _ in 0..18 {
            assert!(lod::split(&mut cut, parent));
            parent = lod::children(parent)[3];
        }
        for level in (1..18).rev() {
            let requests = frontier_keys(&cut, &[orbital], 1, tile_center(orbital), true);
            assert_eq!(requests, vec![lod::ancestor(deep, level).unwrap()]);
            assert!(requests[0].level >= 1);
            assert!(lod::coarsen(&mut cut, requests[0]));
        }
        assert!(cut.contains(&orbital));
        assert!(frontier_keys(&cut, &[orbital], 1, tile_center(orbital), true).is_empty());
    }

    #[test]
    fn pressure_requests_reserve_complete_siblings_and_keep_every_inverse_parent() {
        let root = |face| TileKey {
            face,
            ..Default::default()
        };
        let old = root(0);
        let staged = root(4);
        let deferred = root(5);
        let mut cut = lod::initial_leaves();
        assert!(lod::split(&mut cut, old));
        let targets = [
            TileKey {
                face: 4,
                level: 3,
                x: 2,
                y: 2,
            },
            TileKey {
                face: 5,
                level: 3,
                x: 2,
                y: 2,
            },
        ];
        let complete = frontier_keys(&cut, &targets, 3, tile_center(staged), true);
        assert_eq!(complete.len(), 9);
        let bounded = pressure_frontier(complete.clone(), &cut, Some(staged));
        assert_eq!(bounded.len(), 5);
        assert!(
            bounded.contains(&old),
            "missing parent required by inverse zoom"
        );
        assert!(lod::children(staged)
            .iter()
            .all(|key| bounded.contains(key)));
        assert!(lod::children(deferred)
            .iter()
            .all(|key| !bounded.contains(key)));
        // Once the first quartet is usable, another family can receive all its
        // siblings rather than repeatedly loading one immediately evicted tile.
        assert!(lod::split(&mut cut, staged));
        let next = pressure_frontier(
            frontier_keys(&cut, &targets, 3, tile_center(deferred), true),
            &cut,
            Some(deferred),
        );
        assert!(next.contains(&old));
        assert!(lod::children(deferred).iter().all(|key| next.contains(key)));
        assert_eq!(pressure_frontier(complete, &cut, None), vec![old]);
    }

    #[test]
    fn cube_face_fine_boundary_projects_to_the_actual_coarse_segment() {
        let parent = TileKey {
            face: 0,
            ..Default::default()
        };
        let at = |index: usize| {
            let u = index % 5;
            let v = index / 5;
            let direction = cube_direction(0, u as f64 * 0.5 - 1.0, v as f64 * 0.5 - 1.0);
            let radius = 1.0 + u as f64 * 0.001 + v as f64 * 0.002;
            (direction * radius, direction.as_vec3().to_array())
        };
        for y in 0..=16 {
            let direction = cube_direction(4, 1.0, y as f64 / 8.0 - 1.0);
            let (radius, _) = surface_query(direction, parent, 5, at).unwrap();
            let point = direction * radius;
            let coarse_y = (y / 4).min(3);
            let a = at(coarse_y * 5).0;
            let b = at((coarse_y + 1) * 5).0;
            let t = (point - a).dot(b - a) / (b - a).length_squared();
            assert!((-1e-12..=1.000000000001).contains(&t));
            assert!((point - a.lerp(b, t)).length() < 1e-12);
            let (sea_radius, depth) = surface_scalar(direction, parent, 5, |i| {
                let d = at(i).0.normalize();
                (d, if i / 5 < 2 { 5.0 } else { -5.0 })
            })
            .unwrap();
            assert!(sea_radius > 0.95 && sea_radius <= 1.000000000001);
            assert!((-5.0..=5.0).contains(&depth));
        }
    }

    #[test]
    fn smooth_normals_match_across_separate_tile_meshes() {
        let make = |point: DVec3| {
            vertex(
                &Sample {
                    direction: point.normalize().to_array(),
                    height_m: point.length() - 1.0,
                    ..Default::default()
                },
                1.0,
                1.0,
                false,
            )
        };
        let mut meshes = [
            Mesh {
                vertices: vec![
                    make(DVec3::new(0.0, 0.0, 1.0)),
                    make(DVec3::new(1.0, 0.0, 1.0)),
                    make(DVec3::new(0.0, 1.0, 1.0)),
                ],
                indices: vec![0, 1, 2],
            },
            Mesh {
                vertices: vec![
                    make(DVec3::new(0.0, 0.0, 1.0)),
                    make(DVec3::new(0.0, 1.0, 1.0)),
                    make(DVec3::new(-1.0, 0.0, 2.0)),
                ],
                indices: vec![0, 1, 2],
            },
        ];
        smooth_mesh_normals(&mut meshes);
        assert_eq!(
            meshes[0].vertices[0].shading_normal,
            meshes[1].vertices[0].shading_normal
        );
        assert_eq!(
            meshes[0].vertices[2].shading_normal,
            meshes[1].vertices[1].shading_normal
        );
        let normal = Vec3::from_array(meshes[0].vertices[0].shading_normal);
        assert!((normal.length() - 1.0).abs() < 1e-6);
        assert!(normal.x > 0.1);
    }

    #[test]
    fn wide_cut_covers_the_frustum_at_a_cube_corner_lod_threshold() {
        let direction = DVec3::ONE.normalize();
        let altitude = 1900.0 / 2_000_000.0;
        let level = camera_level(altitude);
        assert_eq!(level, 14);
        let level = footprint_level(direction, altitude, 3.0, 0.0);
        let keys = footprint_keys(direction, level, 3.0, altitude, 0.0);
        let eye = direction * (1.0 + altitude);
        let right = (-direction).cross(DVec3::Y).normalize();
        let up = right.cross(-direction);
        for y in -5..=5 {
            for x in -10..=10 {
                let ray = (-direction
                    + right * (x as f64 / 10.0 * 3.0 * 22.5f64.to_radians().tan())
                    + up * (y as f64 / 5.0 * 22.5f64.to_radians().tan()))
                .normalize();
                let b = eye.dot(ray);
                let t = -b - (b * b - eye.length_squared() + 1.0).sqrt();
                let point = (eye + ray * t).normalize();
                let (face, u, v) = face_uv(point);
                let count = 1u32 << level;
                let key = TileKey {
                    face: face as u8,
                    level,
                    x: (((u + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
                    y: (((v + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
                };
                assert!(
                    keys.contains(&key),
                    "missing face {} tile {},{} for pixel {},{}",
                    key.face,
                    key.x,
                    key.y,
                    x,
                    y
                );
            }
        }
    }

    #[test]
    fn inclined_cut_covers_wide_viewports_at_every_cube_corner() {
        for signs in [
            DVec3::ONE,
            DVec3::new(-1.0, 1.0, 1.0),
            DVec3::new(1.0, -1.0, -1.0),
        ] {
            for aspect in [1.6, 3.0, 4.0] {
                for altitude in [1900.0 / 2_000_000.0, 5.0 / 2_000_000.0] {
                    let direction = signs.normalize();
                    let tilt = 35f64.to_radians();
                    let level = footprint_level(direction, altitude, aspect, tilt);
                    let keys = footprint_keys(direction, level, aspect, altitude, tilt);
                    assert!(keys.len() <= MAX_VISIBLE_TILES);
                    let (eye, forward, right, up) = camera_frame(direction, altitude, tilt, 1.0);
                    for y in -20..=20 {
                        for x in -40..=40 {
                            let ray = (forward
                                + right * (x as f64 / 40.0 * aspect * 22.5f64.to_radians().tan())
                                + up * (y as f64 / 20.0 * 22.5f64.to_radians().tan()))
                            .normalize();
                            let b = eye.dot(ray);
                            let t = -b - (b * b - eye.length_squared() + 1.0).sqrt();
                            let p = (eye + ray * t).normalize();
                            let (face, u, v) = face_uv(p);
                            let count = 1u32 << level;
                            let key = TileKey {
                                face: face as u8,
                                level,
                                x: (((u + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
                                y: (((v + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
                            };
                            assert!(
                                keys.contains(&key),
                                "missing {key:?} at aspect {aspect} and altitude {altitude}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn surface_ray_hits_the_rendered_mesh_on_every_cube_face() {
        for face in 0..6 {
            let b = BASES[face];
            let vec = |a: [i32; 3]| DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
            let at = |index: usize| {
                let x = index % 9;
                let y = index / 9;
                (vec(b[0])
                    + vec(b[1]) * (x as f64 / 4.0 - 1.0)
                    + vec(b[2]) * (y as f64 / 4.0 - 1.0))
                    .normalize()
                    * 1.003
            };
            let direction = (vec(b[0]) + vec(b[1]) * 0.37 - vec(b[2]) * 0.41).normalize();
            let radius = surface_radius(
                direction,
                TileKey {
                    face: face as u8,
                    ..Default::default()
                },
                9,
                at,
            )
            .unwrap();
            assert!((0.98..1.003).contains(&radius));
            let point = direction * radius;
            let cell_x = 5;
            let cell_y = 2;
            let a = at(cell_y * 9 + cell_x);
            let b = at(cell_y * 9 + cell_x + 1);
            let d = at((cell_y + 1) * 9 + cell_x + 1);
            let c = at((cell_y + 1) * 9 + cell_x);
            let distance_a = (b - a).cross(d - a).normalize().dot(point - a).abs();
            let distance_b = (d - a).cross(c - a).normalize().dot(point - a).abs();
            assert!(distance_a.min(distance_b) < 1e-12);
        }
    }
}
