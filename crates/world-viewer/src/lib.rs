//! Browser explorer: all geometry, precision handling, camera and rendering are Rust.
use bytemuck::{Pod, Zeroable};
use glam::{DVec3, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const BASES: [[[i32; 3]; 3]; 6] = [
    [[1, 0, 0], [0, 0, -1], [0, 1, 0]],
    [[-1, 0, 0], [0, 0, 1], [0, 1, 0]],
    [[0, 1, 0], [1, 0, 0], [0, 0, -1]],
    [[0, -1, 0], [1, 0, 0], [0, 0, 1]],
    [[0, 0, 1], [1, 0, 0], [0, 1, 0]],
    [[0, 0, -1], [-1, 0, 0], [0, 1, 0]],
];

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
    cells: Vec<Sample>,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
        sample.water_m
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
        let weight: f64 = sample.biome_mix.iter().sum();
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
            for (name, amount) in names.into_iter().zip(sample.biome_mix) {
                let c = biome_color(name);
                for i in 0..3 {
                    result[i] += c[i] * (amount / weight) as f32;
                }
            }
            result
        } else {
            biome_color(&sample.biome)
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
            (sample.water_m - sample.height_m) as f32,
        ],
        morph: [0.0, 0.0, (sample.water_m - sample.height_m) as f32, 0.0],
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

#[derive(Default)]
struct Average {
    count: f64,
    height: f64,
    water: f64,
    temperature: f64,
    moisture: f64,
    river: f64,
    color: [f64; 3],
    ocean: bool,
}

/// Shared cube-vertex keys ensure all six faces use exactly the same boundary geometry.
fn world_mesh(world: &World, exaggeration: f64) -> Result<Mesh, String> {
    let n = world.config.resolution;
    if n < 2 || n > 2048 || world.cells.len() != 6 * n * n {
        return Err("Atlas invalide : six faces de resolution² cellules sont nécessaires".into());
    }
    let mut shared: HashMap<[i32; 3], Average> = HashMap::new();
    for face in 0..6 {
        for y in 0..n {
            for x in 0..n {
                let cell = &world.cells[face * n * n + y * n + x];
                let color = biome_color(&cell.biome);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let average = shared.entry(cube_key(face, x + dx, y + dy, n)).or_default();
                    average.count += 1.0;
                    average.height += cell.height_m;
                    average.water += cell.water_m;
                    average.temperature += cell.temperature_c;
                    average.moisture += cell.moisture;
                    average.river = average.river.max(cell.river).max(
                        if cell.discharge > 1.0 && !cell.ocean && !cell.lake {
                            (cell.discharge.ln() / 12.0).clamp(0.0, 1.0)
                        } else {
                            0.0
                        },
                    );
                    average.ocean |= cell.ocean;
                    for i in 0..3 {
                        average.color[i] += color[i] as f64;
                    }
                }
            }
        }
    }
    let mut mesh = Mesh::default();
    for water in [false, true] {
        for face in 0..6 {
            let start = mesh.vertices.len() as u32;
            for y in 0..=n {
                for x in 0..=n {
                    let key = cube_key(face, x, y, n);
                    let a = &shared[&key];
                    let sample = Sample {
                        direction: DVec3::new(key[0] as f64, key[1] as f64, key[2] as f64)
                            .normalize()
                            .to_array(),
                        height_m: a.height / a.count,
                        water_m: a.water / a.count,
                        temperature_c: a.temperature / a.count,
                        moisture: a.moisture / a.count,
                        river: a.river,
                        ocean: a.ocean,
                        ..Default::default()
                    };
                    let mut v = vertex(&sample, world.config.radius_m, exaggeration, water);
                    if !water {
                        for i in 0..3 {
                            v.color[i] = (a.color[i] / a.count) as f32;
                        }
                    }
                    mesh.vertices.push(v);
                }
            }
            for y in 0..n {
                for x in 0..n {
                    let a = start + (y * (n + 1) + x) as u32;
                    let b = a + 1;
                    let c = a + (n + 1) as u32;
                    let d = c + 1;
                    mesh.indices.extend([a, b, d, a, d, c]);
                }
            }
        }
    }
    smooth_mesh_normals(std::slice::from_mut(&mut mesh));
    Ok(mesh)
}

fn tile_mesh(tile: &Tile, radius: f64, exaggeration: f64) -> Result<Mesh, String> {
    if tile.vertices.is_empty()
        || tile
            .indices
            .iter()
            .any(|&i| i as usize >= tile.vertices.len())
    {
        return Err("Indices de tuile invalides".into());
    }
    let mut mesh = Mesh::default();
    for water in [false, true] {
        let start = mesh.vertices.len() as u32;
        mesh.vertices
            .extend(tile.vertices.iter().enumerate().map(|(index, s)| {
                let mut v = vertex(s, radius, exaggeration, water);
                v.fields[2] = s.river as f32;
                v.color[3] += 2.0 * (f32::from(tile.key.level) + 1.0);
                if let Some(offsets) = tile.morph_offsets.get(index) {
                    v.morph = [
                        offsets[usize::from(water)] as f32,
                        tile.morph_start as f32,
                        offsets[2] as f32,
                        0.0,
                    ];
                }
                v
            }));
        mesh.indices.extend(tile.indices.iter().map(|i| start + i));
    }
    smooth_mesh_normals(std::slice::from_mut(&mut mesh));
    Ok(mesh)
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
    ((1.0 / altitude).log2().floor() as i32).clamp(0, 18) as u8
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

fn mesh_surfaces(mesh: &Mesh) -> [Vec<DVec3>; 2] {
    let half = mesh.vertices.len() / 2;
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

fn neighborhood_keys(direction: DVec3, level: u8, aspect: f64) -> Vec<TileKey> {
    let (face, u, v) = face_uv(direction);
    let count = 1u32 << level;
    let step = 2.0 / f64::from(count);
    let cx = (((u + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1);
    let cy = (((v + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1);
    let mut keys = Vec::new();
    let b = BASES[face];
    let vec = |a: [i32; 3]| DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
    let rings = (1.5 * 22.5f64.to_radians().tan() * aspect.max(1.0))
        .ceil()
        .clamp(1.0, 3.0) as i32;
    let mut offsets: Vec<_> = (-rings..=rings)
        .flat_map(|y| (-rings..=rings).map(move |x| (x, y)))
        .collect();
    offsets.sort_by_key(|&(x, y)| x * x + y * y);
    for (dx, dy) in offsets {
        let u = -1.0 + (f64::from(cx) + f64::from(dx) + 0.5) * step;
        let v = -1.0 + (f64::from(cy) + f64::from(dy) + 0.5) * step;
        let (f, u, v) = face_uv((vec(b[0]) + vec(b[1]) * u + vec(b[2]) * v).normalize());
        let key = TileKey {
            face: f as u8,
            level,
            x: (((u + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
            y: (((v + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1),
        };
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
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
    use glam::{Mat4, Vec4};
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
                size: (mesh.vertices.len() * std::mem::size_of::<Vertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let indices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("terrain indices"),
                size: (mesh.indices.len() * std::mem::size_of::<u32>()) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&vertices, 0, bytemuck::cast_slice(&mesh.vertices));
            queue.write_buffer(&indices, 0, bytemuck::cast_slice(&mesh.indices));
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
        active_keys: Vec<TileKey>,
        rivers: RiverLayer,
        human: HumanLayer,
        objects: Option<GpuMesh>,
        show_rivers: bool,
        show_human: bool,
        yaw: f64,
        pitch: f64,
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
        fn aspect(&self) -> f64 {
            f64::from(self.config.width) / f64::from(self.config.height.max(1))
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
            let mut best: Option<(u8, f64)> = None;
            for (key, _, tile) in &self.tiles {
                if !self.active_keys.contains(key) {
                    continue;
                }
                if below_level.is_some_and(|level| key.level >= level) {
                    continue;
                }
                if best.is_some_and(|(level, _)| level > key.level) {
                    continue;
                }
                let side = (tile.vertices.len() as f64).sqrt() as usize;
                let progress = morph_progress(time, tile.morph_start);
                let hit = surface_radius(direction, *key, side, |index| {
                    let sample = &tile.vertices[index];
                    let height = if water {
                        sample.water_m
                    } else {
                        sample.height_m
                    };
                    let offset = tile
                        .morph_offsets
                        .get(index)
                        .map_or(0.0, |v| v[usize::from(water)]);
                    DVec3::from_array(sample.direction).normalize()
                        * (1.0
                            + height * self.exaggeration / world.config.radius_m
                            + offset * (1.0 - progress))
                });
                if let Some(hit) = hit {
                    best = Some((key.level, hit));
                }
            }
            if let Some((_, hit)) = best {
                return hit;
            }
            let (face, _, _) = face_uv(direction);
            let side = world.config.resolution + 1;
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
            let wanted = camera_level(self.altitude);
            if wanted < 2 {
                self.active_keys.clear();
                return;
            }
            let mut next = None;
            for level in (1..=wanted).rev() {
                let keys = neighborhood_keys(self.direction(), level, self.aspect());
                if keys
                    .iter()
                    .all(|key| self.tiles.iter().any(|(cached, _, _)| cached == key))
                {
                    next = Some(keys);
                    break;
                }
            }
            let Some(next) = next else {
                if let Some(key) = self.active_keys.first() {
                    let required = neighborhood_keys(self.direction(), key.level, self.aspect());
                    if key.level > wanted
                        || !required.iter().all(|key| self.active_keys.contains(key))
                    {
                        self.active_keys.clear();
                    }
                }
                return;
            };
            if next == self.active_keys {
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
                    let index = self.tiles.iter().position(|(cached, _, _)| cached == key)?;
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
                                water - (1.0 + sample.water_m * self.exaggeration / radius),
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
            let eye = dir * (1.0 + self.altitude + self.ground_height(f64::from(time)));
            let high = eye.as_vec3();
            let low = (eye - high.as_dvec3()).as_vec3();
            let rotation = Mat4::look_to_rh(Vec3::ZERO, -dir.as_vec3(), Vec3::Y);
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
                light: Vec4::new(-0.45, 0.80, 0.55, time).to_array(),
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
                active_keys: Vec::new(),
                rivers: RiverLayer::default(),
                human: HumanLayer::default(),
                objects: None,
                show_rivers: true,
                show_human: true,
                yaw: 0.65,
                pitch: 0.35,
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
            renderer.base_surfaces = mesh_surfaces(&mesh);
            renderer.world = Some(world);
            renderer.tiles.clear();
            renderer.active_keys.clear();
            renderer.objects = None;
            Ok(())
        })
    }

    #[wasm_bindgen]
    pub fn set_tile(json: &str) -> Result<String, JsValue> {
        let mut tile: Tile =
            serde_json::from_str(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        RENDERER.with(|state| {
            let mut state = state.borrow_mut();
            let renderer = state.as_mut().ok_or("Visualiseur absent")?;
            let radius = renderer
                .world
                .as_ref()
                .ok_or("Monde absent")?
                .config
                .radius_m;
            tile.morph_start = f64::from(js_now() as f32);
            tile.morph_offsets = tile
                .vertices
                .iter()
                .map(|sample| {
                    let direction = DVec3::from_array(sample.direction).normalize();
                    let ground = renderer.displayed_radius(
                        direction,
                        false,
                        Some(tile.key.level),
                        tile.morph_start,
                    );
                    let water = renderer.displayed_radius(
                        direction,
                        true,
                        Some(tile.key.level),
                        tile.morph_start,
                    );
                    [
                        ground - (1.0 + sample.height_m * renderer.exaggeration / radius),
                        water - (1.0 + sample.water_m * renderer.exaggeration / radius),
                        (water - ground) * radius / renderer.exaggeration,
                    ]
                })
                .collect();
            let mesh = tile_mesh(&tile, radius, renderer.exaggeration)
                .map_err(|e| JsValue::from_str(&e))?;
            if let Some(index) = renderer
                .tiles
                .iter()
                .position(|(key, _, _)| *key == tile.key)
            {
                renderer.tiles.remove(index);
            }
            if renderer.tiles.len() == 160 {
                let level = camera_level(renderer.altitude);
                let desired: Vec<_> = [
                    level.saturating_sub(1),
                    level,
                    level.saturating_add(1).min(18),
                ]
                .into_iter()
                .flat_map(|z| neighborhood_keys(renderer.direction(), z, renderer.aspect()))
                .collect();
                let remove = renderer
                    .tiles
                    .iter()
                    .position(|(key, _, _)| {
                        !renderer.active_keys.contains(key) && !desired.contains(key)
                    })
                    .or_else(|| {
                        renderer
                            .tiles
                            .iter()
                            .position(|(key, _, _)| !renderer.active_keys.contains(key))
                    })
                    .unwrap_or(0);
                renderer.tiles.remove(remove);
            }
            renderer.tiles.push((
                tile.key,
                GpuMesh::upload(&renderer.device, &renderer.queue, &mesh),
                tile,
            ));
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
                r.active_keys.clear();
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
                r.base_surfaces = mesh_surfaces(&mesh);
                r.globe = Some(GpuMesh::upload(&r.device, &r.queue, &mesh));
            }
            for (_, gpu, tile) in &mut r.tiles {
                tile.morph_offsets.clear();
                *gpu = GpuMesh::upload(
                    &r.device,
                    &r.queue,
                    &tile_mesh(
                        tile,
                        r.world
                            .as_ref()
                            .map_or(1_737_000.0, |world| world.config.radius_m),
                        r.exaggeration,
                    )
                    .map_err(|e| JsValue::from_str(&e))?,
                );
            }
            r.active_keys.clear();
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
            let level = u32::from(camera_level(r.altitude));
            let count = 1u32 << level;
            let x = (((u + 1.0) * 0.5 * count as f64) as u32).min(count - 1);
            let y = (((v + 1.0) * 0.5 * count as f64) as u32).min(count - 1);
            let requests: Vec<_> = [level, (level+1).min(18), level.saturating_sub(1)].into_iter().flat_map(|z| neighborhood_keys(r.direction(), z as u8, r.aspect())).collect();
            serde_json::json!({ "face": face, "level": level, "x": x, "y": y, "altitude_m": r.altitude * radius, "direction": r.direction().to_array(), "requests": requests }).to_string()
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
        assert_eq!(level, 10);
        let keys = neighborhood_keys(direction, level, 3.0);
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
