//! Browser explorer: all geometry, precision handling, camera and rendering are Rust.
use bytemuck::{Pod, Zeroable};
use glam::{DVec3, Vec3};
use serde::Deserialize;
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
    ocean: bool,
    lake: bool,
}

#[derive(Deserialize)]
struct World {
    config: Config,
    cells: Vec<Sample>,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct TileKey {
    face: u8,
    level: u8,
    x: u32,
    y: u32,
}

#[derive(Deserialize)]
struct Tile {
    #[serde(default)]
    key: TileKey,
    vertices: Vec<Sample>,
    indices: Vec<u32>,
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
        biome_color(&sample.biome)
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
                    mesh.indices.extend([a, b, c, b, d, c]);
                }
            }
        }
    }
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
        mesh.vertices.extend(tile.vertices.iter().map(|s| {
            let mut v = vertex(s, radius, exaggeration, water);
            v.color[3] += 2.0 * (f32::from(tile.key.level) + 1.0);
            v
        }));
        mesh.indices.extend(tile.indices.iter().map(|i| start + i));
    }
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
            height_m: heights[0] * (1.0 - t) + heights[1] * t + 25.0,
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
            river.width_m.max(150.0),
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
                250.0,
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
        let width = ((town.population as f64).sqrt() * 30.0).clamp(800.0, 6000.0) / radius;
        let base = direction * (1.0 + (town.altitude_m + 40.0) * exaggeration / radius);
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
        tiles: Vec<(TileKey, GpuMesh, Tile)>,
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
        fn direction(&self) -> DVec3 {
            DVec3::new(
                self.pitch.cos() * self.yaw.sin(),
                self.pitch.sin(),
                self.pitch.cos() * self.yaw.cos(),
            )
        }

        fn ground_height(&self) -> f64 {
            let Some(world) = &self.world else {
                return 0.0;
            };
            let (face, u, v) = face_uv(self.direction());
            let mut tile_match: Option<(u8, f64)> = None;
            for (key, _, tile) in &self.tiles {
                if usize::from(key.face) != face {
                    continue;
                }
                let count = f64::from(1u32 << key.level);
                let local_x = (u + 1.0) * 0.5 * count - f64::from(key.x);
                let local_y = (v + 1.0) * 0.5 * count - f64::from(key.y);
                if !(0.0..=1.0).contains(&local_x) || !(0.0..=1.0).contains(&local_y) {
                    continue;
                }
                if tile_match.is_some_and(|(level, _)| level > key.level) {
                    continue;
                }
                let side = (tile.vertices.len() as f64).sqrt() as usize;
                if side < 2 || side * side != tile.vertices.len() {
                    continue;
                }
                let x = local_x * (side - 1) as f64;
                let y = local_y * (side - 1) as f64;
                let i = (x.floor() as usize).min(side - 2);
                let j = (y.floor() as usize).min(side - 2);
                let a = (x - i as f64).clamp(0.0, 1.0);
                let b = (y - j as f64).clamp(0.0, 1.0);
                let at = |x, y| {
                    let s: &Sample = &tile.vertices[y * side + x];
                    s.height_m.max(s.water_m)
                };
                let height = (at(i, j) * (1.0 - a) + at(i + 1, j) * a) * (1.0 - b)
                    + (at(i, j + 1) * (1.0 - a) + at(i + 1, j + 1) * a) * b;
                tile_match = Some((key.level, height));
            }
            if let Some((_, height)) = tile_match {
                return height.max(0.0) * self.exaggeration / world.config.radius_m;
            }
            let n = world.config.resolution;
            let x = (((u + 1.0) * 0.5 * n as f64) as usize).min(n - 1);
            let y = (((v + 1.0) * 0.5 * n as f64) as usize).min(n - 1);
            let s = &world.cells[face * n * n + y * n + x];
            s.height_m.max(s.water_m).max(0.0) * self.exaggeration / world.config.radius_m
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
            let dir = self.direction();
            let eye = dir * (1.0 + self.altitude + self.ground_height());
            let high = eye.as_vec3();
            let low = (eye - high.as_dvec3()).as_vec3();
            let rotation = Mat4::look_to_rh(Vec3::ZERO, -dir.as_vec3(), Vec3::Y);
            let projection = Mat4::perspective_rh(
                45f32.to_radians(),
                width as f32 / height as f32,
                (self.altitude as f32 * 0.008).max(0.00000001),
                5.0,
            );
            let mut clips = [TileClip::zeroed(); 32];
            for (index, (key, _, _)) in self.tiles.iter().enumerate() {
                let step = 2.0 / f64::from(1u32 << key.level);
                let u0 = -1.0 + f64::from(key.x) * step;
                let v0 = -1.0 + f64::from(key.y) * step;
                clips[index] = TileClip {
                    bounds: [u0 as f32, v0 as f32, (u0 + step) as f32, (v0 + step) as f32],
                    face_level: [f32::from(key.face), f32::from(key.level) + 1.0, 0.0, 0.0],
                };
            }
            let uniforms = Uniforms {
                view_projection: (projection * rotation).to_cols_array_2d(),
                eye: high.extend(0.0).to_array(),
                eye_low: low.extend(0.0).to_array(),
                light: Vec4::new(-0.45, 0.80, 0.55, 0.0).to_array(),
                options: [
                    self.mode,
                    self.tiles.len() as f32,
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
                            load: wgpu::LoadOp::Clear(1.0),
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
                    .chain(self.tiles.iter().map(|(_, mesh, _)| mesh))
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
        let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4];
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
                depth_compare: wgpu::CompareFunction::LessEqual,
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
                tiles: Vec::new(),
                rivers: RiverLayer::default(),
                human: HumanLayer::default(),
                objects: None,
                show_rivers: true,
                show_human: true,
                yaw: 0.65,
                pitch: 0.35,
                altitude: 2.2,
                exaggeration: 12.0,
                mode: 0.0,
            })
        });
        Ok(label)
    }

    // Avoid requiring unstable web-sys WebGPU bindings merely to choose the backend.
    #[wasm_bindgen(inline_js = "export function js_sys_gpu_exists() { return !!navigator.gpu; }")]
    extern "C" {
        fn js_sys_gpu_exists() -> bool;
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
            renderer.world = Some(world);
            renderer.tiles.clear();
            renderer.objects = None;
            Ok(())
        })
    }

    #[wasm_bindgen]
    pub fn set_tile(json: &str) -> Result<(), JsValue> {
        let tile: Tile =
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
            let mesh = tile_mesh(&tile, radius, renderer.exaggeration)
                .map_err(|e| JsValue::from_str(&e))?;
            if let Some(index) = renderer
                .tiles
                .iter()
                .position(|(key, _, _)| *key == tile.key)
            {
                renderer.tiles.remove(index);
            }
            if renderer.tiles.len() == 32 {
                renderer.tiles.remove(0);
            }
            renderer.tiles.push((
                tile.key,
                GpuMesh::upload(&renderer.device, &renderer.queue, &mesh),
                tile,
            ));
            Ok(())
        })
    }

    #[wasm_bindgen]
    pub fn clear_tiles() {
        RENDERER.with(|state| {
            if let Some(r) = state.borrow_mut().as_mut() {
                r.tiles.clear();
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
                r.globe = Some(GpuMesh::upload(
                    &r.device,
                    &r.queue,
                    &world_mesh(world, r.exaggeration).map_err(|e| JsValue::from_str(&e))?,
                ));
            }
            for (_, gpu, tile) in &mut r.tiles {
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
            let level = ((1.0 / r.altitude).log2().floor() as i32 - 1).clamp(0, 18) as u32;
            let count = 1u32 << level;
            let x = (((u + 1.0) * 0.5 * count as f64) as u32).min(count - 1);
            let y = (((v + 1.0) * 0.5 * count as f64) as u32).min(count - 1);
            let mut requests = Vec::new();
            for z in [level.saturating_sub(1), level] {
                let count = 1u32 << z;
                let step = 2.0 / f64::from(count);
                let center_x = (((u + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1);
                let center_y = (((v + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1);
                for (dx, dy) in [(0,0),(-1,0),(1,0),(0,-1),(0,1),(-1,-1),(1,-1),(-1,1),(1,1)] {
                    let u = -1.0 + (f64::from(center_x) + f64::from(dx) + 0.5) * step;
                    let v = -1.0 + (f64::from(center_y) + f64::from(dy) + 0.5) * step;
                    let b = BASES[face];
                    let raw = DVec3::new(b[0][0] as f64 + u * b[1][0] as f64 + v * b[2][0] as f64, b[0][1] as f64 + u * b[1][1] as f64 + v * b[2][1] as f64, b[0][2] as f64 + u * b[1][2] as f64 + v * b[2][2] as f64);
                    let (f, a, b) = face_uv(raw.normalize());
                    let x = (((a + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1);
                    let y = (((b + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 1);
                    requests.push(serde_json::json!({"face": f, "level": z, "x": x, "y": y}));
                }
            }
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
                indices: vec![5]
            },
            100.0,
            1.0
        )
        .is_err());
    }
}
