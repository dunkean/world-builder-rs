//! Fixed geographic erosion patches. Rendering children share the same saved
//! field; no solver is seeded by the camera, tile segment count or request order.
use anyhow::{ensure, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
};
use world_core::{
    geometry::{direction, dot},
    RegionPatch, Tile, TileKey, World,
};
use world_erosion::{erode, Backend, ErosionConfig, ErosionReport, HeightField};

const REGION_LEVELS: [u8; 2] = [6, 10];
const SEGMENTS: u32 = 128;
const HALO: u32 = 8;
pub const VERSION: &str = "regional-finite-volume-2-multiscale6";

#[derive(Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub patch: RegionPatch,
    pub report: ErosionReport,
    /// Full solver displacement, before extracting the published detail band.
    pub raw_residual_m: Vec<f32>,
    pub water_m: Vec<f32>,
    pub suspended_sediment_m: Vec<f32>,
}
#[derive(Default)]
struct Cache {
    entries: HashMap<String, Arc<Artifact>>,
    order: VecDeque<String>,
    flights: HashMap<String, Weak<Mutex<()>>>,
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

#[derive(Default)]
struct Metrics {
    hits: AtomicU64,
    disk_reads: AtomicU64,
    builds: AtomicU64,
    wait_us: AtomicU64,
    build_us: AtomicU64,
}
static METRICS: OnceLock<Metrics> = OnceLock::new();

pub fn performance() -> serde_json::Value {
    let metrics = METRICS.get_or_init(Default::default);
    let cache = CACHE.get_or_init(Default::default).lock().unwrap();
    serde_json::json!({"entries":cache.entries.len(),"capacity_entries":128,
        "memory_hits":metrics.hits.load(Ordering::Relaxed),"disk_reads":metrics.disk_reads.load(Ordering::Relaxed),
        "builds":metrics.builds.load(Ordering::Relaxed),"wait_ms":metrics.wait_us.load(Ordering::Relaxed) as f64/1000.0,
        "build_ms":metrics.build_us.load(Ordering::Relaxed) as f64/1000.0})
}

impl Cache {
    fn get(&mut self, key: &str) -> Option<Arc<Artifact>> {
        let artifact = self.entries.get(key)?.clone();
        self.order.retain(|k| k != key);
        self.order.push_back(key.to_owned());
        Some(artifact)
    }
}

pub fn compile_tile(data: &Path, world: &World, key: TileKey, segments: u32) -> Result<Tile> {
    // Lower levels omit this fine residual, as they omit subpixel relief. The
    // same macro region is used at every finer level, including prefetches.
    if key.level < REGION_LEVELS[0] {
        return world.tile(key, segments);
    }
    ensure!(
        key.face < 6 && key.level <= 24 && key.x < 1u32 << key.level && key.y < 1u32 << key.level,
        "invalid tile key"
    );
    let mut patches = Vec::new();
    for level in REGION_LEVELS
        .into_iter()
        .filter(|level| *level <= key.level)
    {
        let shift = key.level - level;
        let region = TileKey {
            face: key.face,
            level,
            x: key.x >> shift,
            y: key.y >> shift,
        };
        patches.push(get(data, world, region, &patches)?.patch.clone());
    }
    world.tile_with_patches(key, segments, &patches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use world_core::{geometry::face_uv, WorldConfig};

    #[test]
    fn erosion_reaches_publication_collar_with_a_smooth_transition() {
        let mut world = World::generate(WorldConfig {
            resolution: 8,
            erosion_steps: 0,
            detail_m: 0.0,
            ..Default::default()
        })
        .unwrap();
        for cell in &mut world.cells {
            cell.height_m = 5000.0 + 2000.0 * cell.position[1] as f32;
            cell.water_m = cell.height_m;
            cell.ocean = false;
            cell.lake = false;
            cell.receiver = None;
            cell.discharge = 0.0;
        }
        let key = TileKey {
            face: 4,
            level: 10,
            x: 512,
            y: 512,
        };
        let artifact = build(&world, key, &[]).unwrap();
        let patch = &artifact.patch;
        let n = (SEGMENTS + 1 + 2 * HALO) as usize;
        let row = (HALO + SEGMENTS / 2) as usize;
        let delta = |x: u32| artifact.raw_residual_m[row * n + (HALO + x) as usize];
        assert!(delta(0).abs() > 1e-4);
        assert!(delta(1).abs() > 1e-4);
        assert!(delta(16).abs() > 1e-4);
        let mut profile = Vec::new();
        for x in 0..=32 {
            let p = patch.direction_at(HALO + x, row as u32);
            let base = world.sample(p).height_m;
            let published = world
                .sample_with_patches(p, std::slice::from_ref(patch))
                .height_m;
            profile.push(f64::from(published) - f64::from(base));
        }
        let raw_amplitude = (0..=32).fold(0.0_f64, |a, x| a.max(f64::from(delta(x)).abs()));
        let largest_step = profile
            .windows(2)
            .fold(0.0_f64, |a, h| a.max((h[1] - h[0]).abs()));
        assert!(
            largest_step < raw_amplitude * 0.25,
            "published step comparable to the raw lowering: {largest_step}/{raw_amplitude}"
        );
        assert!(artifact.report.solid_relative_error <= 5e-5);
        assert!(artifact.report.water_relative_error <= 5e-5);
    }

    #[test]
    fn constant_displacement_creates_no_published_frame_even_near_ports() {
        let n = 33;
        let mut protected = vec![0; n * n];
        let mut raw = vec![-40.0; n * n];
        for y in 0..n {
            for x in 0..n {
                if x < 2 || y < 2 || x + 2 >= n || y + 2 >= n || x == 16 {
                    protected[y * n + x] = 1;
                    raw[y * n + x] = 0.0;
                }
            }
        }
        let detail = detail_band(&raw, &protected, n, n);
        assert!(detail.iter().all(|h| h.abs() < 1e-6));
    }

    #[test]
    fn detail_band_retains_differential_incision_and_deposition() {
        let n = 33;
        let mut raw = vec![-40.0; n * n];
        let protected = vec![0; n * n];
        let incision = 16 * n + 12;
        let deposit = 16 * n + 22;
        raw[incision] -= 8.0;
        raw[deposit] += 6.0;
        let detail = detail_band(&raw, &protected, n, n);
        assert!(detail[incision] < -5.0);
        assert!(detail[deposit] > 4.0);
        assert_eq!(detail[0], 0.0);
        assert!(detail.iter().all(|h| h.is_finite()));
    }

    #[test]
    fn children_share_saved_erosion_and_borders_are_identical() {
        let world = World::generate(WorldConfig {
            seed: 831052,
            resolution: 8,
            erosion_steps: 2,
            ..Default::default()
        })
        .unwrap();
        let peak = world
            .cells
            .iter()
            .filter(|c| !c.ocean && !c.lake)
            .max_by(|a, b| a.height_m.total_cmp(&b.height_m))
            .unwrap();
        let (face, u, v) = face_uv(peak.position);
        let count = 1u32 << 10;
        let x = (((u + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 2);
        let y = (((v + 1.0) * 0.5 * f64::from(count)) as u32).min(count - 2);
        let path = std::env::temp_dir().join(format!(
            "world-regional-test-{}-{}",
            std::process::id(),
            world.id
        ));
        std::fs::create_dir_all(&path).unwrap();
        let key = TileKey {
            face,
            level: 10,
            x,
            y,
        };
        let coarse = compile_tile(&path, &world, key, 16).unwrap();
        let child = compile_tile(
            &path,
            &world,
            TileKey {
                face,
                level: 11,
                x: x * 2,
                y: y * 2,
            },
            16,
        )
        .unwrap();
        for row in 0..=8 {
            for col in 0..=8 {
                assert_eq!(
                    coarse.vertices[row * 17 + col].height_m.to_bits(),
                    child.vertices[row * 2 * 17 + col * 2].height_m.to_bits()
                );
            }
        }
        let neighbor = compile_tile(&path, &world, TileKey { x: x + 1, ..key }, 16).unwrap();
        for row in 0..=16 {
            assert_eq!(
                coarse.vertices[row * 17 + 16].height_m.to_bits(),
                neighbor.vertices[row * 17].height_m.to_bits()
            );
            assert_eq!(
                coarse.vertices[row * 17 + 16].water_m.to_bits(),
                neighbor.vertices[row * 17].water_m.to_bits()
            );
        }
        let reports = reports(&path, &world).unwrap();
        assert!(reports.len() >= 3); // parent6 + two children10, independently persisted.
        for report in reports {
            assert!(report.solid_relative_error <= 5e-5 && report.water_relative_error <= 5e-5);
        }
        let region = path
            .join("worlds")
            .join(&world.id)
            .join("regions")
            .join(VERSION)
            .join(format!("{face}-10-{x}-{y}.json"));
        let mut bytes = std::fs::read(&region).unwrap();
        bytes.push(b' ');
        std::fs::write(&region, &bytes).unwrap();
        assert!(read(&region, &world).is_err());
        std::fs::remove_dir_all(&path).unwrap();
    }
}

pub fn reports(data: &Path, world: &World) -> Result<Vec<ErosionReport>> {
    let root = data
        .join("worlds")
        .join(&world.id)
        .join("regions")
        .join(VERSION);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut paths = std::fs::read_dir(root)?
        .filter_map(|r| r.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|s| s == "json"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|p| read(&p, world).map(|a| a.report))
        .collect()
}

fn get(data: &Path, world: &World, key: TileKey, parents: &[RegionPatch]) -> Result<Arc<Artifact>> {
    let cache_key = format!(
        "{}/{}/{}/{}/{}/{}/{}",
        data.display(),
        VERSION,
        world.id,
        key.face,
        key.level,
        key.x,
        key.y
    );
    let metrics = METRICS.get_or_init(Default::default);
    let cache = CACHE.get_or_init(Default::default);
    let gate = {
        let mut cache = cache
            .lock()
            .map_err(|_| anyhow::anyhow!("region cache poisoned"))?;
        if let Some(a) = cache.get(&cache_key) {
            metrics.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(a);
        }
        if cache.flights.len() > 512 {
            cache.flights.retain(|_, gate| gate.strong_count() != 0);
        }
        if let Some(gate) = cache.flights.get(&cache_key).and_then(Weak::upgrade) {
            gate
        } else {
            let gate = Arc::new(Mutex::new(()));
            cache
                .flights
                .insert(cache_key.clone(), Arc::downgrade(&gate));
            gate
        }
    };
    // Wait only for this geographic region. A cold calculation must not block
    // cache hits or independent regions elsewhere in the planet.
    let waiting = std::time::Instant::now();
    let _flight = gate
        .lock()
        .map_err(|_| anyhow::anyhow!("region computation poisoned"))?;
    metrics
        .wait_us
        .fetch_add(waiting.elapsed().as_micros() as u64, Ordering::Relaxed);
    if let Some(a) = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("region cache poisoned"))?
        .get(&cache_key)
    {
        metrics.hits.fetch_add(1, Ordering::Relaxed);
        return Ok(a);
    }
    let path = data
        .join("worlds")
        .join(&world.id)
        .join("regions")
        .join(VERSION)
        .join(format!(
            "{}-{}-{}-{}.json",
            key.face, key.level, key.x, key.y
        ));
    let artifact = if path.exists() {
        metrics.disk_reads.fetch_add(1, Ordering::Relaxed);
        read(&path, world)?
    } else {
        let started = std::time::Instant::now();
        metrics.builds.fetch_add(1, Ordering::Relaxed);
        let artifact = build(world, key, parents)?;
        metrics
            .build_us
            .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
        std::fs::create_dir_all(path.parent().unwrap())?;
        let bytes = serde_json::to_vec(&artifact)?;
        // Reports enumerate JSON files; publish that file only after its
        // checksum is ready, so concurrent readers never see a partial pair.
        super::atomic_write(
            &path.with_extension("blake3"),
            blake3::hash(&bytes).to_hex().as_bytes(),
        )?;
        super::atomic_write(&path, &bytes)?;
        tracing::info!(world=%world.id,?key,backend=%artifact.report.backend,ms=artifact.report.elapsed_ms,
            change_m=artifact.report.max_height_change_m,"regional erosion saved");
        artifact
    };
    let artifact = Arc::new(artifact);
    let mut cache = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("region cache poisoned"))?;
    cache.entries.insert(cache_key.clone(), artifact.clone());
    cache.order.push_back(cache_key);
    while cache.entries.len() > 128 {
        if let Some(key) = cache.order.pop_front() {
            cache.entries.remove(&key);
        }
    }
    Ok(artifact)
}
fn read(path: &Path, world: &World) -> Result<Artifact> {
    let bytes = std::fs::read(path)?;
    ensure!(
        blake3::hash(&bytes).to_hex().as_str()
            == std::fs::read_to_string(path.with_extension("blake3"))?.trim(),
        "region checksum mismatch"
    );
    let a: Artifact = serde_json::from_slice(&bytes)?;
    a.patch.validate(world)?;
    ensure!(
        a.raw_residual_m.len() == a.patch.heights_m.len()
            && a.raw_residual_m.iter().all(|v| v.is_finite()),
        "invalid persisted raw erosion field"
    );
    ensure!(
        a.patch.source_world_id == world.id && a.patch.algorithm_version == VERSION,
        "region belongs to another world or algorithm"
    );
    ensure!(
        a.patch.segments == SEGMENTS
            && a.patch.halo_cells == HALO
            && REGION_LEVELS.contains(&a.patch.key.level),
        "unexpected region grid"
    );
    ensure!(
        a.patch.heights_m.len() == ((SEGMENTS + 1 + 2 * HALO) as usize).pow(2)
            && a.patch.heights_m.iter().all(|v| v.is_finite()),
        "invalid persisted heights"
    );
    ensure!(
        a.report.water_relative_error <= 5e-5 && a.report.solid_relative_error <= 5e-5,
        "invalid erosion budgets"
    );
    Ok(a)
}
fn build(world: &World, key: TileKey, parents: &[RegionPatch]) -> Result<Artifact> {
    let n = (SEGMENTS + 1 + 2 * HALO) as usize;
    let count = 1u32 << key.level;
    let position = |x: f64, y: f64| {
        direction(
            key.face,
            2.0 * (f64::from(key.x) + (x - f64::from(HALO)) / f64::from(SEGMENTS))
                / f64::from(count)
                - 1.0,
            2.0 * (f64::from(key.y) + (y - f64::from(HALO)) / f64::from(SEGMENTS))
                / f64::from(count)
                - 1.0,
        )
    };
    let samples = (0..n * n)
        .into_par_iter()
        .map(|i| world.sample_with_patches(position((i % n) as f64, (i / n) as f64), parents))
        .collect::<Vec<_>>();
    let a = position(n as f64 * 0.5, n as f64 * 0.5);
    let b = position(n as f64 * 0.5 + 1.0, n as f64 * 0.5);
    let spacing = ((2.0 * (1.0 - dot(a, b))).max(0.0)).sqrt() * world.config.radius_m;
    let protected = samples
        .iter()
        .map(|s| {
            u8::from(
                s.ocean
                    || s.lake
                    || s.river > 0.015
                    || s.water_level_m
                        .is_some_and(|w| (s.height_m - w).abs() < 8.0),
            )
        })
        .collect();
    let field = HeightField {
        width: n,
        height: n,
        spacing_m: spacing as f32,
        heights_m: samples.iter().map(|s| s.height_m).collect(),
        protected,
    };
    let config = ErosionConfig {
        steps: 256,
        rainfall_m_per_step: 0.08,
        conductivity_m_s: 200.0,
        capacity: 300.0,
        max_erosion_m_per_step: (spacing as f32 * 0.0005).min(0.35),
        ..Default::default()
    };
    let result = erode(&field, config, Backend::Auto)?;
    // Let the halo evolve too: fixed halo altitudes become an artificial dam
    // after interior incision. The solver keeps only its external export edge
    // unmodified, with a Neumann displacement ghost; physical ports stay fixed.
    // Preserve the macro envelope: a uniform lowering of independent regions
    // would become a square basin after fading, even with a smooth boundary.
    // Publish only the differential band; keep the full solver result/budgets.
    let detail = detail_band(&result.residual_m, &field.protected, n, n);
    Ok(Artifact {
        patch: RegionPatch {
            key,
            segments: SEGMENTS,
            halo_cells: HALO,
            heights_m: result.heights_m,
            residual_m: detail,
            source_world_id: world.id.clone(),
            algorithm_version: VERSION.into(),
        },
        water_m: result.water_m,
        raw_residual_m: result.residual_m,
        suspended_sediment_m: result.suspended_sediment_m,
        report: result.report,
    })
}

/// One Laplacian-style detail band, using normalized masked convolution.
/// Frozen cells never enter the local mean: an otherwise constant erosion
/// field remains exactly constant up to a shoreline, river port or halo.
fn detail_band(raw: &[f32], protected: &[u8], width: usize, height: usize) -> Vec<f32> {
    const KERNEL: [f64; 9] = [1.0, 8.0, 28.0, 56.0, 70.0, 56.0, 28.0, 8.0, 1.0];
    let mut horizontal_sum = vec![0.0; raw.len()];
    let mut horizontal_weight = vec![0.0; raw.len()];
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            for (offset, weight) in KERNEL.iter().enumerate() {
                let sx = x as isize + offset as isize - 4;
                if sx < 0 || sx >= width as isize {
                    continue;
                }
                let source = y * width + sx as usize;
                if protected[source] == 0 {
                    horizontal_sum[i] += weight * f64::from(raw[source]);
                    horizontal_weight[i] += weight;
                }
            }
        }
    }
    let mut detail = vec![0.0; raw.len()];
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            if protected[i] != 0 {
                continue;
            }
            let mut sum = 0.0;
            let mut support = 0.0;
            for (offset, weight) in KERNEL.iter().enumerate() {
                let sy = y as isize + offset as isize - 4;
                if sy >= 0 && sy < height as isize {
                    let source = sy as usize * width + x;
                    sum += weight * horizontal_sum[source];
                    support += weight * horizontal_weight[source];
                }
            }
            if support > 0.0 {
                detail[i] = (f64::from(raw[i]) - sum / support) as f32;
            }
        }
    }
    detail
}
