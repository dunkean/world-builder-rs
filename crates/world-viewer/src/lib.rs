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
// Halving a tile's grid requires twice the tile frequency for the same
// physical vertex spacing, hence up to four times as many visible leaves.
const MAX_VISIBLE_TILES: usize = 256;
const MORPH_DURATION_SECONDS: f64 = 0.16;

fn shader_source() -> String {
    format!(
        "const MORPH_DURATION_SECONDS: f32 = {MORPH_DURATION_SECONDS};\n{}",
        include_str!("shader.wgsl")
    )
}

fn canonical_tile_vertex_count(key: TileKey, root_segments: usize) -> usize {
    let segments = if key.level == 0 {
        root_segments
    } else {
        STREAM_SEGMENTS
    };
    (segments + 1).pow(2)
}

fn cut_prepare_budget(motion_age: f64, render_ms: f64, prepare_ms: f64) -> f64 {
    let target_ms: f64 = if motion_age < 0.15 { 4.0 } else { 8.0 };
    let other_ms = (render_ms - prepare_ms).max(0.0);
    target_ms.min((16.0 - other_ms - 2.0).max(4.0)) * 0.001
}

#[derive(Clone, Deserialize)]
struct Config {
    radius_m: f64,
    resolution: usize,
}

#[derive(Clone, Default, Deserialize)]
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
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BorderNormal {
    index: u32,
    reserved: u32,
    sum: [f64; 3],
}

struct PreparedTile {
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
        })
        .collect();
    let header = PreparedHeader {
        magic: *b"WVP1",
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

fn decode_prepared_tile(bytes: &[u8]) -> Result<PreparedTile, String> {
    let hsize = std::mem::size_of::<PreparedHeader>();
    if bytes.len() < hsize {
        return Err("Préparation tronquée".into());
    }
    let header: PreparedHeader = bytemuck::pod_read_unaligned(&bytes[..hsize]);
    if header.magic != *b"WVP1"
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
    let samples: Vec<PackedSample> = read(bytes, &mut offset, header.counts[0] as usize)?;
    let vertices: Vec<Vertex> = read(bytes, &mut offset, header.counts[1] as usize)?;
    let indices: Vec<u32> = read(bytes, &mut offset, header.counts[2] as usize)?;
    offset = (offset + 7) & !7;
    let borders = read(bytes, &mut offset, header.counts[3] as usize)?;
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
            })
            .collect(),
        ..Default::default()
    };
    Ok(PreparedTile {
        tile,
        mesh: Mesh { vertices, indices },
        borders,
        radius: header.radius,
        exaggeration: header.exaggeration,
    })
}

fn biome_color(biome: &str) -> [f32; 3] {
    match biome {
        "Ocean" => [0.08, 0.25, 0.40],
        "Lake" => [0.12, 0.39, 0.48],
        "Ice" => [0.87, 0.92, 0.93],
        "Tundra" => [0.47, 0.53, 0.41],
        "Desert" => [0.79, 0.64, 0.39],
        "Grassland" => [0.43, 0.56, 0.26],
        "Forest" => [0.20, 0.39, 0.22],
        "Rainforest" => [0.09, 0.31, 0.20],
        "Wetland" => [0.25, 0.48, 0.34],
        "Rock" => [0.52, 0.49, 0.43],
        _ => [0.49, 0.54, 0.37],
    }
}

fn vertex(sample: &Sample, radius: f64, exaggeration: f64, water: bool) -> Vertex {
    let direction = DVec3::from_array(sample.direction).normalize();
    let height = if water {
        water_height(sample)
    } else {
        sample.height_m
    };
    let position = direction * (1.0 + height * exaggeration / radius);
    let high = position.as_vec3();
    let low = (position - high.as_dvec3()).as_vec3();
    let color = if water {
        if sample.ocean {
            [0.045, 0.26, 0.43]
        } else {
            [0.06, 0.37, 0.45]
        }
    } else {
        let weight: f64 = sample.biome_mix[2..].iter().sum();
        if weight > 0.0 {
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
            let mut result = [0.0; 3];
            for (name, amount) in names.into_iter().zip(sample.biome_mix).skip(2) {
                let c = biome_color(name);
                for i in 0..3 {
                    result[i] += c[i] * (amount / weight) as f32;
                }
            }
            result
        } else {
            if sample.biome == "Ocean" || sample.biome == "Lake" {
                biome_color("Wetland")
            } else {
                biome_color(&sample.biome)
            }
        }
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
            river as f32,
            (water_height(sample) - sample.height_m) as f32,
        ],
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
                    v.color[..3].copy_from_slice(if id == 0 {
                        &[0.045, 0.26, 0.43]
                    } else {
                        &[0.06, 0.37, 0.45]
                    });
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
        (-1e-6, 0.5),
        (1.000001, 0.5),
        (0.5, -1e-6),
        (0.5, 1.000001),
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
    let visible = |key: TileKey| {
        targets
            .iter()
            .any(|target| lod::is_ancestor(key, *target) || lod::is_ancestor(*target, key))
    };
    let mut parents: Vec<_> = cut
        .iter()
        .copied()
        .filter(|key| key.level < level && visible(*key))
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
                (parent.level >= level || !visible(*parent))
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
                v.fields[2] = if water && water_body(s).is_some_and(|(id, _)| id >= RIVER_BODY) {
                    -2.0
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
    ((1.0 / altitude).log2().floor() as i32 + 3).clamp(0, i32::from(MAX_TILE_LEVEL)) as u8
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

fn camera_frame(
    direction: DVec3,
    altitude: f64,
    tilt: f64,
    radius: f64,
) -> (DVec3, DVec3, DVec3, DVec3) {
    let north = (DVec3::Y - direction * direction.y).normalize_or_zero();
    let eye = direction * (radius + altitude) - north * altitude * tilt.tan();
    let forward = (direction * radius - eye).normalize();
    let right = forward.cross(DVec3::Y).normalize();
    let up = right.cross(forward).normalize();
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
    let mut level = camera_level(altitude);
    while level > 0
        && footprint_keys(direction, level, aspect, altitude, tilt).len() > MAX_VISIBLE_TILES
    {
        level -= 1;
    }
    level
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
            [0.13, 0.66, 0.81],
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

    thread_local! { static RENDERER: RefCell<Option<Renderer>> = const { RefCell::new(None) }; }

    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct Uniforms {
        view_projection: [[f32; 4]; 4],
        eye: [f32; 4],
        eye_low: [f32; 4],
        light: [f32; 4],
        options: [f32; 4],
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
    struct DynamicVertex {
        morph: [f32; 4],
        shading_normal: [f32; 3],
        parent_normal: [f32; 3],
        water_depth: f32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Pod, Zeroable)]
    struct StaticVertex {
        position: [f32; 3],
        position_low: [f32; 3],
        normal_height: [f32; 4],
        color: [f32; 4],
        fields: [f32; 4],
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
            let (mut low, mut high) =
                (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
            for g in &geometry {
                let p = g.direction * g.radius;
                low = low.min(p);
                high = high.max(p);
            }
            let bound_center = (low + high) * 0.5;
            let bound_radius = (high - low).length() * 0.5;
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
        tile: Tile,
        prepared: Option<Mesh>,
        borders: Vec<BorderNormal>,
        gpu: Option<GpuMesh>,
        last_used: u64,
        cpu_base: usize,
        morph_source: u64,
        transition_start: f64,
    }
    impl CachedTile {
        fn cpu_bytes(&self) -> usize {
            self.cpu_base
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
        reused_tiles: u64,
        max_prepare_ms: f64,
        last_prepare_ms: f64,
        last_render_ms: f64,
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
        cut_dirty: bool,
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

        fn level_for(&self, direction: DVec3, altitude: f64) -> u8 {
            let step = ((self.config.height.max(1) as f64
                / (STREAM_SEGMENTS as f64 * altitude * 0.41421356237 * 2.0))
                .log2()
                .ceil() as i32)
                .clamp(0, i32::from(MAX_TILE_LEVEL)) as u8;
            let mut level = camera_level(altitude).max(step);
            while level > 0
                && footprint_keys(direction, level, self.aspect(), altitude, self.view_tilt).len()
                    > MAX_VISIBLE_TILES
            {
                level -= 1;
            }
            level
        }
        fn detail_level(&self) -> u8 {
            self.level_for(self.direction(), self.altitude)
        }
        fn footprint(&self, level: u8) -> Vec<TileKey> {
            footprint_keys(
                self.direction(),
                level,
                self.aspect(),
                self.altitude,
                self.view_tilt,
            )
        }
        fn requests_at(&self, direction: DVec3, altitude: f64) -> Vec<TileKey> {
            let level = self.level_for(direction, altitude);
            let targets = footprint_keys(direction, level, self.aspect(), altitude, self.view_tilt);
            frontier_keys(&self.active_set, &targets, level, direction, false)
        }
        fn staged_parent(&self) -> Option<TileKey> {
            self.active_set
                .iter()
                .copied()
                .filter(|key| {
                    key.level < self.wanted
                        && self.desired.iter().any(|d| lod::is_ancestor(*key, *d))
                })
                .min_by(|a, b| {
                    a.level.cmp(&b.level).then_with(|| {
                        tile_center(*b)
                            .dot(self.direction())
                            .total_cmp(&tile_center(*a).dot(self.direction()))
                    })
                })
        }
        fn requests(&self) -> Vec<TileKey> {
            let mut keys = frontier_keys(
                &self.active_set,
                &self.desired,
                self.wanted,
                self.direction(),
                true,
            );
            let mut seen: HashSet<_> = keys.iter().copied().collect();
            let (cpu, gpu) = self.cache_bytes();
            if cpu < 768 * 1024 * 1024 * 4 / 5 && gpu < 768 * 1024 * 1024 * 4 / 5 {
                let future = self.requests_at(self.direction(), self.altitude * 0.5);
                keys.extend(future.into_iter().filter(|key| seen.insert(*key)).take(64));
            } else {
                keys = pressure_frontier(keys, &self.active_set, self.staged_parent());
            }
            keys
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
            let right = forward.cross(DVec3::Y).normalize();
            (eye, forward, right, right.cross(forward).normalize())
        }
        fn outside_view(&self, keys: impl IntoIterator<Item = TileKey>, time: f64) -> bool {
            let frame = self.view_frame(time);
            keys.into_iter().all(|key| {
                self.tile_lookup
                    .get(&key)
                    .and_then(|i| self.tiles[*i].gpu.as_ref())
                    .is_some_and(|gpu| {
                        !sphere_in_frustum(
                            gpu.bound_center,
                            gpu.bound_radius + gpu.morph_pad + 1e-6,
                            frame,
                            self.aspect(),
                        )
                    })
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
            }
        }
        fn cache_bytes(&self) -> (usize, usize) {
            (
                self.tiles.iter().map(CachedTile::cpu_bytes).sum(),
                self.tiles
                    .iter()
                    .filter_map(|t| t.gpu.as_ref())
                    .map(|g| g.bytes)
                    .sum(),
            )
        }
        fn evict_cache(&mut self) {
            const CPU_LIMIT: usize = 768 * 1024 * 1024;
            const GPU_LIMIT: usize = 768 * 1024 * 1024;
            let (cpu, gpu) = self.cache_bytes();
            if cpu <= CPU_LIMIT && gpu <= GPU_LIMIT {
                return;
            }
            let mut pinned = self.active_set.clone();
            pinned.extend(self.active_set.iter().filter_map(|leaf| lod::parent(*leaf)));
            let (face, u, v) = face_uv(self.direction());
            if let Some(center) = lod::covering_key(&self.active_set, face as u8, u, v) {
                // Keep the central return path resident without retaining every
                // historical ancestor of the entire visible partition.
                pinned.extend((0..center.level).filter_map(|level| lod::ancestor(center, level)));
            }
            // Reserve a complete sibling quartet before its fourth packet arrives.
            // Without this reservation, pressure can repeatedly evict the first three.
            if let Some(parent) = self.staged_parent() {
                pinned.extend(lod::children(parent));
            }
            if let Some(p) = &self.pending {
                pinned.extend(p.parents.iter().copied());
                pinned.extend(p.children.iter().copied());
                pinned.extend(p.jobs.iter().map(|v| v.0));
            }
            let wanted: HashSet<_> = self.requests().into_iter().collect();
            loop {
                let (cpu, gpu) = self.cache_bytes();
                if cpu <= CPU_LIMIT && gpu <= GPU_LIMIT {
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

        fn install_roots(&mut self) -> Result<(), String> {
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
                let mut mesh = tile_mesh(&tile, radius, self.exaggeration)?;
                compact_mesh(&mut mesh, tile.vertices.len());
                let borders = tile_borders(&tile, &mesh);
                compiled.push((tile, mesh, borders));
            }
            let mut sums: HashMap<[i64; 3], DVec3> = HashMap::new();
            for (tile, _, borders) in &compiled {
                for border in borders {
                    *sums
                        .entry(normal_key(DVec3::from_array(
                            tile.vertices[border.index as usize].direction,
                        )))
                        .or_default() += DVec3::from_array(border.sum);
                }
            }
            for (tile, mut mesh, borders) in compiled {
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
                let cpu_base = tile_cpu_base(&tile, &borders);
                self.tiles.push(CachedTile {
                    key,
                    tile,
                    prepared: None,
                    borders,
                    gpu: Some(gpu),
                    last_used: self.tick,
                    cpu_base,
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
                let affected = self.affected_neighbors(parent, coarsening);
                let quartet = lod::children(parent);
                let candidate_instant = coarsening
                    && self.outside_view(
                        std::iter::once(parent)
                            .chain(quartet)
                            .chain(affected.iter().copied()),
                        now,
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
            self.next_epoch = self.next_epoch.wrapping_add(1).max(1);
            let epoch = self.next_epoch;
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
            if !instant {
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
                .or_insert_with(|| transition_neighbors(parent, &self.active_set, coarsening))
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
            // In-flight local work is never cancelled merely because the camera moved.
            if self.pending.is_some() {
                self.process_cut(deadline);
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
                        && (parent.level >= self.wanted
                            || !self.desired.iter().any(|d| {
                                lod::is_ancestor(*parent, *d) || lod::is_ancestor(*d, *parent)
                            }))
                        && lod::children(*parent).iter().all(|k| {
                            self.tiles[self.tile_lookup[k]].transition_start
                                + MORPH_DURATION_SECONDS
                                <= now
                        })
                })
                .collect();
            coarsen.sort_by(|a, b| b.level.cmp(&a.level));
            let (cpu, gpu) = self.cache_bytes();
            if cpu > 768 * 1024 * 1024 || gpu > 768 * 1024 * 1024 {
                let outside: Vec<_> = coarsen
                    .iter()
                    .copied()
                    .filter(|parent| {
                        !self
                            .desired
                            .iter()
                            .any(|d| lod::is_ancestor(*parent, *d) || lod::is_ancestor(*d, *parent))
                    })
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
                    key.level < self.wanted
                        && self.desired.iter().any(|d| lod::is_ancestor(*key, *d))
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
                    && !self.outside_view(
                        pending
                            .parents
                            .iter()
                            .copied()
                            .chain(pending.children.iter().copied())
                            .chain(pending.jobs.iter().filter_map(|(key, kind, _)| {
                                (*kind == WorkKind::Edge).then_some(*key)
                            })),
                        js_now(),
                    )
                {
                    // Camera movement can expose a group while it is being prepared.
                    // Complete the visible inverse morph instead of committing a jump.
                    pending.instant = false;
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
            let radius = 1.0 + self.ground_height(f64::from(time));
            let (mut eye, _, _, _) = camera_frame(dir, self.altitude, self.view_tilt, radius);
            let eye_direction = eye.normalize();
            let ground = self
                .displayed_radius(eye_direction, false, None, f64::from(time))
                .max(self.displayed_radius(eye_direction, true, None, f64::from(time)));
            eye = eye_direction * eye.length().max(ground + self.altitude * 0.25);
            let look = (dir * radius - eye).normalize();
            let high = eye.as_vec3();
            let low = (eye - high.as_dvec3()).as_vec3();
            let rotation = Mat4::look_to_rh(Vec3::ZERO, look.as_vec3(), Vec3::Y);
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
                    0.0,
                    if self.show_rivers { 1.0 } else { 0.0 },
                    if self.show_human { 1.0 } else { 0.0 },
                ],
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
        let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4];
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
    pub fn install_prepared_tile(bytes: &[u8]) -> Result<String, JsValue> {
        let prepared = decode_prepared_tile(bytes).map_err(|e| JsValue::from_str(&e))?;
        install_prepared(prepared)
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
    fn install_prepared(prepared: PreparedTile) -> Result<String, JsValue> {
        RENDERER.with(|state| {
            let mut state = state.borrow_mut();
            let r = state.as_mut().ok_or("Visualiseur absent")?;
            let world = r.world.as_ref().ok_or("Monde absent")?;
            if prepared.radius != world.config.radius_m || prepared.exaggeration != r.exaggeration {
                return Err(JsValue::from_str("Préparation périmée"));
            }
            if world.render_grid.as_deref() == Some("vertices")
                && prepared.tile.vertices.len()
                    != canonical_tile_vertex_count(prepared.tile.key, world.mesh_resolution())
            {
                return Err(JsValue::from_str(
                    "La tuile doit utiliser la grille canonique de son niveau",
                ));
            }
            let key = prepared.tile.key;
            if !r.tile_lookup.contains_key(&key) {
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
                    tile: prepared.tile,
                    prepared: Some(prepared.mesh),
                    borders: prepared.borders,
                    gpu: None,
                    last_used: r.tick,
                    cpu_base,
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
    pub fn clear_tiles() {
        RENDERER.with(|state| {
            if let Some(r) = state.borrow_mut().as_mut() {
                r.cancel_pending();
                r.tiles.retain(|t| t.key.level == 0);
                r.rebuild_lookup();
                r.active_set = lod::initial_leaves();
                r.cut_serial = r.cut_serial.wrapping_add(1);
                r.neighbor_cache.clear();
                r.sync_cut();
                r.cut_dirty = true;
                r.signature = None;
            }
        });
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
            if let Some(renderer) = state.borrow_mut().as_mut() {
                renderer
                    .render(width, height)
                    .map_err(|e| JsValue::from_str(&e))
            } else {
                Ok(())
            }
        })
    }

    #[wasm_bindgen]
    pub fn rotate(dx: f64, dy: f64) {
        RENDERER.with(|state| {
            if let Some(r) = state.borrow_mut().as_mut() {
                let speed = 0.002 * r.altitude.clamp(0.000001, 2.0);
                r.yaw -= dx * speed;
                r.pitch = (r.pitch + dy * speed).clamp(-1.55, 1.55);
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
            renderer.pitch = direction.y.asin().clamp(-1.55, 1.55);
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
                r.mode = mode.min(4) as f32;
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

    #[wasm_bindgen]
    pub fn view_info() -> String {
        RENDERER.with(|state| {
            let state = state.borrow();
            let Some(r) = state.as_ref() else { return "{}".into(); };
            let (face, u, v) = face_uv(r.direction());
            let radius = r.world.as_ref().map_or(1_737_000.0, |w| w.config.radius_m);
            let level = u32::from(r.detail_level());
            let count = 1u32 << level;
            let x = (((u + 1.0) * 0.5 * count as f64) as u32).min(count - 1);
            let y = (((v + 1.0) * 0.5 * count as f64) as u32).min(count - 1);
            let requests = r.requests();
            serde_json::json!({ "face": face, "level": level, "x": x, "y": y, "altitude_m": r.altitude * radius, "direction": r.direction().to_array(), "view_tilt_deg": r.view_tilt.to_degrees(), "active_level":lod::covering_key(&r.active_set,face as u8,u,v).map(|key|key.level), "active_tiles": r.active_keys.len(), "draw_tiles":r.draw_keys.len(),
                "cpu_bytes":r.cache_bytes().0,"gpu_bytes":r.cache_bytes().1,"cpu_budget_bytes":768*1024*1024u64,"gpu_budget_bytes":768*1024*1024u64,"resident_keys":r.tiles.iter().map(|tile|tile.key).collect::<Vec<_>>(),
                "pending_tiles":r.pending.as_ref().map_or(0,|p|p.jobs.len()-p.cursor),"mesh_builds":r.metrics.mesh_builds,"uploads":r.metrics.uploads,"upload_bytes":r.metrics.upload_bytes,
                "cut_commits":r.metrics.cut_commits,"reused_tiles":r.metrics.reused_tiles,"prepare_ms":r.metrics.last_prepare_ms,"surface_fallbacks":r.metrics.surface_fallbacks,"max_prepare_ms":r.metrics.max_prepare_ms,"max_render_ms":r.metrics.max_render_ms,"requests": requests }).to_string()
        })
    }
}

#[cfg(target_arch = "wasm32")]
pub use browser::*;

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut extra = packet;
        extra.push(0);
        assert!(decode_prepared_tile(&extra).is_err());
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
    fn streamed_grid_keeps_physical_vertex_spacing_after_one_extra_level() {
        for altitude in [0.000003, 0.0001, 0.001, 0.01, 0.1, 0.5, 1.0] {
            let previous_level =
                ((1.0_f64 / altitude).log2().floor() as i32 + 2).clamp(0, 23) as u8;
            let level = camera_level(altitude);
            assert_eq!(level, previous_level + 1);
            assert_eq!(
                STREAM_SEGMENTS * (1usize << level),
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
        assert_eq!(level, 13);
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
