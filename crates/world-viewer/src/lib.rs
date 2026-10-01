//! Browser explorer: all geometry, precision handling, camera and rendering are Rust.
use bytemuck::{Pod, Zeroable};
use glam::{DVec3, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

const BASES: [[[i32; 3]; 3]; 6] = [
    [[1, 0, 0], [0, 0, -1], [0, 1, 0]],
    [[-1, 0, 0], [0, 0, 1], [0, 1, 0]],
    [[0, 1, 0], [1, 0, 0], [0, 0, -1]],
    [[0, -1, 0], [1, 0, 0], [0, 0, 1]],
    [[0, 0, 1], [1, 0, 0], [0, 1, 0]],
    [[0, 0, -1], [-1, 0, 0], [0, 1, 0]],
];

const RIVER_BODY: u64 = 1 << 52;

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
    height_m: f64,
    water_m: f64,
    water_level_m: Option<f64>,
    #[serde(alias = "lake_id")]
    water_body_id: Option<u64>,
    temperature_c: f64,
    moisture: f64,
    discharge: f64,
    river: f64,
    biome: String,
    biome_mix: [f64; 10],
    ocean: bool,
    lake: bool,
}

#[derive(Deserialize)]
struct World {
    config: Config,
    #[serde(default)]
    render_resolution: Option<usize>,
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
) {
    let water_start = samples.len() as u32;
    for triangle in terrain_indices.chunks_exact(3) {
        let mut bodies: Vec<(u64, f64)> = Vec::new();
        for &i in triangle {
            let s = &samples[i as usize];
            if let Some((id, level)) = water_body(s) {
                let id = if id >= RIVER_BODY { RIVER_BODY } else { id };
                if level - s.height_m > 0.005
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

/// The global visual mesh is bounded independently of the physical atlas resolution.
fn world_mesh(world: &World, exaggeration: f64) -> Result<Mesh, String> {
    let source = world.mesh_resolution();
    if source < 2 || source > 2048 || world.cells.len() != 6 * source * source {
        return Err("Atlas invalide : six faces de resolution² cellules sont nécessaires".into());
    }
    let n = source.min(128);
    let mut samples = Vec::with_capacity(6 * (n + 1) * (n + 1));
    let mut indices = Vec::with_capacity(36 * n * n);
    for face in 0..6 {
        let start = samples.len() as u32;
        for y in 0..=n {
            for x in 0..=n {
                let key = cube_key(face, x, y, n);
                let direction = DVec3::new(key[0] as f64, key[1] as f64, key[2] as f64).normalize();
                samples.push(atlas_sample(world, direction));
            }
        }
        for y in 0..n {
            for x in 0..n {
                let a = start + (y * (n + 1) + x) as u32;
                let b = a + 1;
                let c = a + (n + 1) as u32;
                let d = c + 1;
                indices.extend([a, b, d, a, d, c]);
            }
        }
    }
    mesh_from_samples(
        &samples,
        &indices,
        0,
        None,
        world.config.radius_m,
        exaggeration,
    )
}

fn mesh_from_samples(
    samples: &[Sample],
    indices: &[u32],
    tag: u8,
    morph: Option<(&[[f64; 3]], f64)>,
    radius: f64,
    exaggeration: f64,
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
                v.fields[2] = s.river as f32;
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
    append_water_geometry(&mut mesh, samples, indices, radius, exaggeration);
    smooth_mesh_normals(std::slice::from_mut(&mut mesh));
    Ok(mesh)
}

fn tile_mesh(tile: &Tile, radius: f64, exaggeration: f64) -> Result<Mesh, String> {
    mesh_from_samples(
        &tile.vertices,
        &tile.indices,
        tile.key.level + 1,
        Some((&tile.morph_offsets, tile.morph_start)),
        radius,
        exaggeration,
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
    let t = ((time - start) / 0.35).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn camera_level(altitude: f64) -> u8 {
    ((1.0 / altitude).log2().floor() as i32 + 2).clamp(0, 18) as u8
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
                let normal = if normal.length_squared() > 1e-30 {
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
    while level > 0 && footprint_keys(direction, level, aspect, altitude, tilt).len() > 64 {
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
    let first = mesh.vertices.len() as u32;
    for i in 0..=8 {
        let t = f64::from(i) / 8.0;
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
        if i < 8 {
            let a = first + i * 2;
            mesh.indices.extend([a, a + 1, a + 2, a + 1, a + 3, a + 2]);
        }
    }
}

fn object_mesh(rivers: &RiverLayer, human: &HumanLayer, radius: f64, exaggeration: f64) -> Mesh {
    let mut mesh = Mesh::default();
    for river in &rivers.segments {
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

    struct GpuMesh {
        vertices: wgpu::Buffer,
        indices: wgpu::Buffer,
        count: u32,
    }

    impl GpuMesh {
        fn upload(device: &wgpu::Device, queue: &wgpu::Queue, mesh: &Mesh) -> Self {
            let vertices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("terrain vertices"),
                size: ((mesh.vertices.len() * std::mem::size_of::<Vertex>()) as u64).max(4),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let indices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("terrain indices"),
                size: ((mesh.indices.len() * std::mem::size_of::<u32>()) as u64).max(4),
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            if !mesh.vertices.is_empty() {
                queue.write_buffer(&vertices, 0, bytemuck::cast_slice(&mesh.vertices));
            }
            if !mesh.indices.is_empty() {
                queue.write_buffer(&indices, 0, bytemuck::cast_slice(&mesh.indices));
            }
            Self {
                vertices,
                indices,
                count: mesh.indices.len() as u32,
            }
        }
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
        tiles: Vec<(TileKey, GpuMesh, Tile)>,
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
        }
        fn rebuild_lookup(&mut self) {
            self.tile_lookup = self
                .tiles
                .iter()
                .enumerate()
                .map(|(i, (key, _, _))| (*key, i))
                .collect();
        }
        fn aspect(&self) -> f64 {
            f64::from(self.config.width) / f64::from(self.config.height.max(1))
        }
        fn detail_level(&self) -> u8 {
            footprint_level(
                self.direction(),
                self.altitude,
                self.aspect(),
                self.view_tilt,
            )
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
        fn requests(&self) -> Vec<TileKey> {
            let level = self.detail_level();
            let mut keys = self.footprint(level);
            keys.extend(self.footprint((level + 1).min(18)).into_iter().take(192));
            keys.extend(self.footprint(level.saturating_sub(1)).into_iter().take(64));
            let mut seen = HashSet::new();
            keys.retain(|key| seen.insert(*key));
            keys
        }
        fn direction(&self) -> DVec3 {
            DVec3::new(
                self.pitch.cos() * self.yaw.sin(),
                self.pitch.sin(),
                self.pitch.cos() * self.yaw.cos(),
            )
        }

        fn displayed_radius(
            &self,
            direction: DVec3,
            water: bool,
            below_level: Option<u8>,
            time: f64,
        ) -> f64 {
            let Some(world) = &self.world else {
                return 1.0;
            };
            if let Some(active) = self.active_keys.first() {
                if !below_level.is_some_and(|level| active.level >= level) {
                    let (face, u, v) = face_uv(direction);
                    let count = 1u32 << active.level;
                    let key = TileKey {
                        face: face as u8,
                        level: active.level,
                        x: (((u + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
                        y: (((v + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
                    };
                    if self.active_set.contains(&key) {
                        if let Some(&index) = self.tile_lookup.get(&key) {
                            let tile = &self.tiles[index].2;
                            let side = (tile.vertices.len() as f64).sqrt() as usize;
                            let progress = morph_progress(time, tile.morph_start);
                            if let Some(hit) = surface_radius(direction, key, side, |index| {
                                let sample = &tile.vertices[index];
                                let height = if water {
                                    water_height(sample)
                                } else {
                                    sample.height_m
                                };
                                let offset = tile.morph_offsets.get(index).map_or(0.0, |v| {
                                    if water && water_body(sample).is_some() {
                                        0.0
                                    } else {
                                        v[0]
                                    }
                                });
                                DVec3::from_array(sample.direction).normalize()
                                    * (1.0
                                        + height * self.exaggeration / world.config.radius_m
                                        + offset * (1.0 - progress))
                            }) {
                                return hit;
                            }
                        }
                    }
                }
            }
            let (face, _, _) = face_uv(direction);
            let side = world.mesh_resolution().min(128) + 1;
            let start = face * side * side;
            let vertices = &self.base_surfaces[usize::from(water)];
            surface_radius(
                direction,
                TileKey {
                    face: face as u8,
                    ..Default::default()
                },
                side,
                |i| vertices[start + i],
            )
            .unwrap_or(1.0)
        }

        fn ground_height(&self, time: f64) -> f64 {
            let direction = self.direction();
            self.displayed_radius(direction, false, None, time)
                .max(self.displayed_radius(direction, true, None, time))
                - 1.0
        }

        fn refresh_cut(&mut self) {
            let wanted = self.detail_level();
            if wanted < 2 {
                self.clear_cut();
                return;
            }
            let mut next = None;
            for level in (1..=wanted).rev() {
                let keys = self.footprint(level);
                if keys.iter().all(|key| self.tile_lookup.contains_key(key)) {
                    next = Some(keys);
                    break;
                }
            }
            let Some(next) = next else {
                if let Some(key) = self.active_keys.first() {
                    let required = self.footprint(key.level);
                    if key.level > wanted
                        || !required.iter().all(|key| self.active_keys.contains(key))
                    {
                        self.clear_cut();
                    }
                }
                return;
            };
            if next.len() == self.active_keys.len()
                && next.iter().all(|key| self.active_set.contains(key))
            {
                return;
            }
            let Some(world) = &self.world else {
                return;
            };
            let radius = world.config.radius_m;
            let start = f64::from(js_now() as f32);
            let updates: Vec<_> = next
                .iter()
                .filter_map(|key| {
                    let index = *self.tile_lookup.get(key)?;
                    let offsets = self.tiles[index]
                        .2
                        .vertices
                        .iter()
                        .map(|sample| {
                            let dir = DVec3::from_array(sample.direction).normalize();
                            let ground = self.displayed_radius(dir, false, None, start);
                            let water = self.displayed_radius(dir, true, None, start);
                            [
                                ground - (1.0 + sample.height_m * self.exaggeration / radius),
                                water - (1.0 + water_height(sample) * self.exaggeration / radius),
                                (water - ground) * radius / self.exaggeration,
                            ]
                        })
                        .collect();
                    Some((index, offsets))
                })
                .collect();
            let mut meshes = Vec::new();
            let mut indices = Vec::new();
            for (index, offsets) in updates {
                let (_, _, tile) = &mut self.tiles[index];
                tile.morph_start = start;
                tile.morph_offsets = offsets;
                if let Ok(mesh) = tile_mesh(tile, radius, self.exaggeration) {
                    meshes.push(mesh);
                    indices.push(index);
                }
            }
            smooth_mesh_normals(&mut meshes);
            for (index, mesh) in indices.into_iter().zip(meshes) {
                self.tiles[index].1 = GpuMesh::upload(&self.device, &self.queue, &mesh);
            }
            self.active_set = next.iter().copied().collect();
            self.active_keys = next;
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
            }
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
            let clips = [TileClip::zeroed(); 32];
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
                    .filter(|_| self.active_keys.is_empty())
                    .chain(
                        self.tiles
                            .iter()
                            .filter(|(key, _, _)| self.active_keys.contains(key))
                            .map(|(_, mesh, _)| mesh),
                    )
                    .chain(self.objects.iter())
                {
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.count, 0, 0..1);
                }
            }
            self.queue.submit(Some(encoder.finish()));
            output.present();
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
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x3, 7 => Float32x3];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("planet"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attributes,
                }],
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
            let mesh =
                world_mesh(&world, renderer.exaggeration).map_err(|e| JsValue::from_str(&e))?;
            renderer.globe = Some(GpuMesh::upload(&renderer.device, &renderer.queue, &mesh));
            renderer.base_surfaces = mesh_surfaces(&mesh, world.mesh_resolution());
            renderer.world = Some(world);
            renderer.tiles.clear();
            renderer.tile_lookup.clear();
            renderer.clear_cut();
            renderer.objects = None;
            Ok(())
        })
    }

    #[wasm_bindgen]
    pub fn set_tile(json: &str) -> Result<String, JsValue> {
        let tile = decode_json_tile(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        install_tile(tile)
    }

    #[wasm_bindgen]
    pub fn add_tile_binary(bytes: &[u8]) -> Result<String, JsValue> {
        let tile = world_core::tile_wire::decode_tile(bytes)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        install_tile(from_core_tile(tile))
    }

    fn install_tile(mut tile: Tile) -> Result<String, JsValue> {
        RENDERER.with(|state| {
            let mut state = state.borrow_mut();
            let renderer = state.as_mut().ok_or("Visualiseur absent")?;
            renderer.world.as_ref().ok_or("Monde absent")?;
            if renderer.tile_lookup.contains_key(&tile.key) {
                return serde_json::to_string(
                    &renderer
                        .tiles
                        .iter()
                        .map(|(key, _, _)| key)
                        .collect::<Vec<_>>(),
                )
                .map_err(|error| JsValue::from_str(&error.to_string()));
            }
            if tile.vertices.is_empty()
                || tile
                    .indices
                    .iter()
                    .any(|&index| index as usize >= tile.vertices.len())
            {
                return Err(JsValue::from_str("Indices de tuile invalides"));
            }
            tile.morph_offsets.clear();
            if let Some(index) = renderer
                .tiles
                .iter()
                .position(|(key, _, _)| *key == tile.key)
            {
                renderer.tiles.remove(index);
            }
            if renderer.tiles.len() == 320 {
                let desired = renderer.requests();
                let remove = renderer
                    .tiles
                    .iter()
                    .position(|(key, _, _)| {
                        !renderer.active_set.contains(key) && !desired.contains(key)
                    })
                    .or_else(|| {
                        renderer
                            .tiles
                            .iter()
                            .position(|(key, _, _)| !renderer.active_set.contains(key))
                    })
                    .unwrap_or(0);
                renderer.tiles.remove(remove);
            }
            renderer.tiles.push((
                tile.key,
                GpuMesh::upload(&renderer.device, &renderer.queue, &Mesh::default()),
                tile,
            ));
            renderer.rebuild_lookup();
            serde_json::to_string(
                &renderer
                    .tiles
                    .iter()
                    .map(|(key, _, _)| key)
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| JsValue::from_str(&error.to_string()))
        })
    }

    #[wasm_bindgen]
    pub fn clear_tiles() {
        RENDERER.with(|state| {
            if let Some(r) = state.borrow_mut().as_mut() {
                r.tiles.clear();
                r.tile_lookup.clear();
                r.clear_cut();
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
            if let Some(world) = &r.world {
                let mesh = world_mesh(world, r.exaggeration).map_err(|e| JsValue::from_str(&e))?;
                r.base_surfaces = mesh_surfaces(&mesh, world.mesh_resolution());
                r.globe = Some(GpuMesh::upload(&r.device, &r.queue, &mesh));
            }
            for (_, gpu, tile) in &mut r.tiles {
                tile.morph_offsets.clear();
                *gpu = GpuMesh::upload(&r.device, &r.queue, &Mesh::default());
            }
            r.clear_cut();
            rebuild_objects(r);
            Ok(())
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
            serde_json::json!({ "face": face, "level": level, "x": x, "y": y, "altitude_m": r.altitude * radius, "direction": r.direction().to_array(), "view_tilt_deg": r.view_tilt.to_degrees(), "active_level": r.active_keys.first().map(|key|key.level), "active_tiles": r.active_keys.len(), "requests": requests }).to_string()
        })
    }
}

#[cfg(target_arch = "wasm32")]
pub use browser::*;

#[cfg(test)]
mod tests {
    use super::*;

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
            cells,
        };
        let mesh = world_mesh(&world, 1.0).unwrap();
        assert_eq!(mesh.indices.len(), 6 * 2 * 2 * 6);
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
        assert_eq!(mesh.indices.len(), 6 * 2 * 2 * 6);
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
        }
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
        assert!((morph_progress(3.175, 3.0) - 0.5).abs() < 1e-12);
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
        assert_eq!(level, 12);
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
                    assert!(keys.len() <= 64);
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
