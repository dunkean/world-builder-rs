use anyhow::{Context, Result};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path as FsPath, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Weak,
    },
};
use tokio::sync::{RwLock, Semaphore};
use tower_http::{compression::CompressionLayer, services::ServeDir};
use world_core::{TileKey, World, WorldConfig};

mod families;
mod hierarchy;
mod preview;
mod regions;

// Render-field revision changes tile caches without rewriting physical atlases.
const SURFACE_REVISION: &str = "spherical-coast-7";

#[derive(Parser)]
#[command(
    name = "worldctl",
    about = "Compilateur et explorateur de planètes — prototype Rust"
)]
struct Args {
    #[arg(long, default_value = "data", global = true)]
    data: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Générer et enregistrer un atlas indépendant de la caméra.
    Generate {
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long, default_value_t = 64)]
        resolution: u32,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Alias de génération pour choisir une graine.
    Preview {
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long, default_value_t = 32)]
        resolution: u32,
    },
    /// Servir le visualiseur WASM et l'API locale.
    Serve {
        #[arg(long, default_value_t = 8001)]
        port: u16,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value = "web")]
        web: PathBuf,
    },
    /// Lire le monde sauvegardé et vérifier drainage et bilans.
    Validate {
        #[arg(long)]
        world: Option<String>,
    },
    /// Démontrer la subdivision conservatrice et son certificat.
    RefineDemo {
        #[arg(long, default_value_t = 42)]
        seed: u64,
    },
}

#[derive(Clone)]
struct App {
    data: PathBuf,
    world: Arc<RwLock<Option<Arc<World>>>>,
    jobs: Arc<RwLock<HashMap<String, Job>>>,
    generation: Arc<Semaphore>,
    counter: Arc<AtomicU64>,
    tiles: Arc<RwLock<TileCache>>,
    tile_workers: Arc<Semaphore>,
    tile_flights: Arc<tokio::sync::Mutex<TileFlights>>,
    tile_metrics: Arc<TileMetrics>,
}

type TileFlights = HashMap<String, Weak<tokio::sync::Mutex<()>>>;

#[derive(Default)]
struct TileMetrics {
    requests: AtomicU64,
    memory_hits: AtomicU64,
    disk_hits: AtomicU64,
    computations: AtomicU64,
    coalesced: AtomicU64,
    queue_us: AtomicU64,
    compute_us: AtomicU64,
    corrupt_disk_entries: AtomicU64,
}

struct TileCache {
    entries: HashMap<String, bytes::Bytes>,
    order: VecDeque<String>,
    size: usize,
    capacity: usize,
    evictions: u64,
}
impl Default for TileCache {
    fn default() -> Self {
        let mib = std::env::var("WB_TILE_CACHE_MIB")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(256)
            .clamp(16, 2048);
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            size: 0,
            capacity: mib * 1024 * 1024,
            evictions: 0,
        }
    }
}
impl TileCache {
    fn get(&mut self, key: &str) -> Option<bytes::Bytes> {
        let value = self.entries.get(key)?.clone();
        self.order.retain(|k| k != key);
        self.order.push_back(key.to_owned());
        Some(value)
    }
    fn insert(&mut self, key: String, value: bytes::Bytes) {
        if self.entries.contains_key(&key) {
            return;
        }
        self.size += value.len();
        self.order.push_back(key.clone());
        self.entries.insert(key, value);
        while self.size > self.capacity || self.entries.len() > 2048 {
            if let Some(key) = self.order.pop_front() {
                if let Some(value) = self.entries.remove(&key) {
                    self.size -= value.len();
                    self.evictions += 1;
                }
            } else {
                break;
            }
        }
    }
}

#[derive(Clone, Serialize)]
struct Job {
    id: String,
    status: String,
    world_id: Option<String>,
    error: Option<String>,
    elapsed_ms: u128,
}

struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error":self.1}))).into_response()
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, format!("{value:#}"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "worldctl=info,tower_http=warn".into()),
        )
        .init();
    let args = Args::parse();
    match args.command {
        Command::Generate {
            seed,
            resolution,
            config,
        } => {
            let config = if let Some(path) = config {
                serde_json::from_slice(
                    &std::fs::read(&path).with_context(|| format!("lecture {}", path.display()))?,
                )?
            } else {
                WorldConfig {
                    seed,
                    resolution,
                    ..Default::default()
                }
            };
            generate_cli(&args.data, config)?;
        }
        Command::Preview { seed, resolution } => {
            generate_cli(
                &args.data,
                WorldConfig {
                    seed,
                    resolution,
                    ..Default::default()
                },
            )?;
        }
        Command::Validate { world } => {
            let world = load_world(&args.data, world.as_deref())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&validate_world(&world)?)?
            );
        }
        Command::RefineDemo { seed } => {
            let size = 9;
            let parent = world_contracts::Grid {
                size,
                heights_m: (0..size * size)
                    .map(|k| {
                        let x = (k % size) as f64;
                        let y = (k / size) as f64;
                        200.0 + x * 4.0 + y * 2.0 + (x * 0.5).sin() * 12.0
                    })
                    .collect(),
                weights: vec![1.0; size * size],
            };
            let family = world_contracts::refine(&parent, seed, 20.0)?;
            println!("{}", serde_json::to_string_pretty(&family.certificate)?);
        }
        Command::Serve { host, port, web } => {
            std::fs::create_dir_all(&args.data)?;
            let loaded = load_world(&args.data, None).ok().map(Arc::new);
            let app = App {
                data: args.data,
                world: Arc::new(RwLock::new(loaded)),
                jobs: Default::default(),
                generation: Arc::new(Semaphore::new(1)),
                counter: Arc::new(AtomicU64::new(1)),
                tiles: Default::default(),
                tile_workers: Arc::new(Semaphore::new(4)),
                tile_flights: Default::default(),
                tile_metrics: Default::default(),
            };
            let router = Router::new()
                .route("/api/health", get(|| async { Json(serde_json::json!({"ok":true,"backend":"rust","global_compute":"cpu","regional_compute":"cuda-with-cpu-fallback","generator_version":world_core::GENERATOR_VERSION,"version":env!("CARGO_PKG_VERSION")})) }))
                .route("/api/performance", get(performance))
                .route("/api/worlds", post(create_world))
                .route("/api/jobs/{id}", get(get_job))
                .route("/api/world/latest", get(latest))
                .route("/api/world/latest/preview", get(latest_preview))
                .route("/api/worlds/{id}", get(by_id))
                .route("/api/worlds/{id}/preview", get(world_preview))
                .route("/api/worlds/{id}/manifest", get(manifest))
                .route("/api/worlds/{id}/season", get(season))
                .route("/api/worlds/{id}/tiles/{face}/{level}/{x}/{y}", get(tile))
                .route("/api/worlds/{id}/families/{face}/{level}/{x}/{y}", get(family))
                .route("/api/worlds/{id}/sample", get(sample))
                .route("/api/worlds/{id}/rivers", get(rivers))
                .route("/api/worlds/{id}/lakes", get(lakes))
                .route("/api/worlds/{id}/regions/reports", get(region_reports))
                .route("/api/worlds/{id}/human", get(human))
                .route("/api/worlds/{id}/reports", get(report))
                .fallback_service(ServeDir::new(web).append_index_html_on_directories(true))
                .layer(CompressionLayer::new())
                .with_state(app);
            let listener = tokio::net::TcpListener::bind(format!("{host}:{port}")).await?;
            tracing::info!("Explorateur http://{host}:{port}");
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
        }
    }
    Ok(())
}

fn generate_cli(data: &FsPath, config: WorldConfig) -> Result<()> {
    let start = std::time::Instant::now();
    let world = World::generate(config)?;
    let report = validate_world(&world)?;
    save_world(data, &world)?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"world_id":world.id,"elapsed_ms":start.elapsed().as_millis(),"stats":world.stats,"validation":report})
        )?
    );
    Ok(())
}

fn save_world(data: &FsPath, world: &World) -> Result<()> {
    let dir = data.join("worlds").join(&world.id);
    std::fs::create_dir_all(&dir)?;
    let bytes = serde_json::to_vec(world)?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    atomic_write(&dir.join("atlas.json"), &bytes)?;
    atomic_write(&dir.join("checksum.blake3"), hash.as_bytes())?;
    let report = validate_world(world)?;
    atomic_write(
        &dir.join("report.json"),
        &serde_json::to_vec_pretty(&report)?,
    )?;
    atomic_write(&data.join("latest"), world.id.as_bytes())?;
    Ok(())
}

fn atomic_write(path: &FsPath, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    // Windows rename ne remplace pas un fichier. Le contenu est immuable,
    // latest est un petit pointeur reconstructible.
    if cfg!(windows) && path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn checked_id(id: &str) -> Result<()> {
    anyhow::ensure!(
        !id.is_empty() && id.len() <= 128 && id.bytes().all(|c| c.is_ascii_hexdigit()),
        "identifiant de monde invalide"
    );
    Ok(())
}

fn load_world(data: &FsPath, id: Option<&str>) -> Result<World> {
    let id = match id {
        Some(s) => s.to_owned(),
        None => std::fs::read_to_string(data.join("latest"))
            .context("aucun monde; lancez generate ou utilisez le bouton Générer")?,
    };
    let id = id.trim();
    checked_id(id)?;
    let dir = data.join("worlds").join(id);
    let bytes = std::fs::read(dir.join("atlas.json"))?;
    let expected = std::fs::read_to_string(dir.join("checksum.blake3"))?;
    anyhow::ensure!(
        blake3::hash(&bytes).to_hex().as_str() == expected.trim(),
        "atlas corrompu : checksum différent"
    );
    let world: World = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(world.id == id, "identifiant atlas différent du répertoire");
    let mut identity = blake3::Hasher::new();
    identity.update(world_core::GENERATOR_VERSION.as_bytes());
    identity.update(&serde_json::to_vec(&world.config)?);
    anyhow::ensure!(
        world.id == identity.finalize().to_hex()[..20],
        "ce monde provient d'une ancienne version du générateur; régénérez la même graine pour créer une nouvelle révision"
    );
    Ok(world)
}

async fn create_world(
    State(app): State<App>,
    Json(config): Json<WorldConfig>,
) -> Result<Json<serde_json::Value>, ApiError> {
    config
        .validate()
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    let permit = app.generation.clone().try_acquire_owned().map_err(|_| {
        ApiError(
            StatusCode::CONFLICT,
            "une génération est déjà en cours".into(),
        )
    })?;
    let id = format!("job-{}", app.counter.fetch_add(1, Ordering::Relaxed));
    {
        let mut jobs = app.jobs.write().await;
        if jobs.len() >= 64 {
            jobs.retain(|_, j| j.status == "running" || j.status == "queued");
        }
        jobs.insert(
            id.clone(),
            Job {
                id: id.clone(),
                status: "queued".into(),
                world_id: None,
                error: None,
                elapsed_ms: 0,
            },
        );
    }
    let job_id = id.clone();
    tokio::spawn(async move {
        let start = std::time::Instant::now();
        if let Some(job) = app.jobs.write().await.get_mut(&job_id) {
            job.status = "running".into();
        }
        let data = app.data.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<World> {
            let world = World::generate(config)?;
            validate_world(&world)?;
            save_world(&data, &world)?;
            Ok(world)
        })
        .await;
        match result {
            Ok(Ok(world)) => {
                let world_id = world.id.clone();
                *app.world.write().await = Some(Arc::new(world));
                if let Some(job) = app.jobs.write().await.get_mut(&job_id) {
                    job.status = "ready".into();
                    job.world_id = Some(world_id);
                    job.elapsed_ms = start.elapsed().as_millis();
                }
            }
            other => {
                let message = match other {
                    Ok(Err(e)) => format!("{e:#}"),
                    Err(e) => e.to_string(),
                    _ => unreachable!(),
                };
                if let Some(job) = app.jobs.write().await.get_mut(&job_id) {
                    job.status = "failed".into();
                    job.error = Some(message);
                    job.elapsed_ms = start.elapsed().as_millis();
                }
            }
        }
        drop(permit);
    });
    Ok(Json(serde_json::json!({"job_id":id})))
}

async fn get_job(State(app): State<App>, Path(id): Path<String>) -> Result<Json<Job>, ApiError> {
    app.jobs
        .read()
        .await
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or(ApiError(StatusCode::NOT_FOUND, "tâche inconnue".into()))
}

async fn current(app: &App) -> Result<Arc<World>, ApiError> {
    app.world
        .read()
        .await
        .clone()
        .ok_or(ApiError(StatusCode::NOT_FOUND, "aucun monde généré".into()))
}
async fn find_world(app: &App, id: &str) -> Result<Arc<World>, ApiError> {
    checked_id(id).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    if let Some(world) = app.world.read().await.clone() {
        if world.id == id {
            return Ok(world);
        }
    }
    let data = app.data.clone();
    let id = id.to_owned();
    let world = tokio::task::spawn_blocking(move || load_world(&data, Some(&id)))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok(Arc::new(world))
}
async fn latest(State(app): State<App>) -> Result<Json<World>, ApiError> {
    Ok(Json((*current(&app).await?).clone()))
}
async fn by_id(State(app): State<App>, Path(id): Path<String>) -> Result<Json<World>, ApiError> {
    Ok(Json((*find_world(&app, &id).await?).clone()))
}
async fn latest_preview(
    State(app): State<App>,
    Query(query): Query<preview::PreviewQuery>,
) -> Result<Json<preview::WorldPreview>, ApiError> {
    let world = current(&app).await?;
    compile_preview(world, query).await
}
async fn world_preview(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<preview::PreviewQuery>,
) -> Result<Json<preview::WorldPreview>, ApiError> {
    let world = find_world(&app, &id).await?;
    compile_preview(world, query).await
}
async fn compile_preview(
    world: Arc<World>,
    query: preview::PreviewQuery,
) -> Result<Json<preview::WorldPreview>, ApiError> {
    let preview = tokio::task::spawn_blocking(move || preview::build(&world, query))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(preview))
}
async fn manifest(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let w = find_world(&app, &id).await?;
    Ok(Json(
        serde_json::json!({"id":w.id,"config":w.config,"stats":w.stats,"sea_level_m":w.sea_level_m,"tile_segments":64,"max_tile_level":24,"numeric_profile":"cpu-f64-global/cuda-or-cpu-f32-regional-v1","generator_version":world_core::GENERATOR_VERSION,"regional_erosion_levels":[],"experimental_spatial_erosion_levels":[6,10],"viewer_generator":"coherent","surface_revision":SURFACE_REVISION}),
    ))
}
#[derive(Deserialize)]
struct TileQuery {
    #[serde(default = "default_segments")]
    segments: u32,
    phase: Option<f64>,
    generator: Option<String>,
    #[serde(default)]
    format: TileFormat,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum TileFormat {
    #[default]
    Json,
    Postcard,
}

impl TileFormat {
    fn content_type(self) -> &'static str {
        match self {
            Self::Json => "application/json",
            Self::Postcard => world_core::tile_wire::CONTENT_TYPE,
        }
    }
}
fn default_segments() -> u32 {
    64
}

const TILE_DISK_CAPACITY: u64 = 1024 * 1024 * 1024;
static TILE_DISK_WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn tile_disk_path(data: &FsPath, key: &str) -> PathBuf {
    let revision = format!(
        "tile-cache-2/{}/{}/{}/{key}",
        world_core::GENERATOR_VERSION,
        regions::VERSION,
        SURFACE_REVISION
    );
    data.join("tile-cache").join(format!(
        "{}.bin",
        blake3::hash(revision.as_bytes()).to_hex()
    ))
}

fn read_cached_tile(path: &FsPath, metrics: &TileMetrics) -> Option<bytes::Bytes> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 32 || blake3::hash(&data[32..]).as_bytes() != &data[..32] {
        metrics.corrupt_disk_entries.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
    Some(bytes::Bytes::from(data).slice(32..))
}

fn write_cached_tile(path: &FsPath, value: &[u8]) -> Result<()> {
    let _guard = TILE_DISK_WRITES
        .lock()
        .map_err(|_| anyhow::anyhow!("tile disk cache poisoned"))?;
    let root = path.parent().unwrap();
    std::fs::create_dir_all(root)?;
    let mut data = Vec::with_capacity(32 + value.len());
    data.extend_from_slice(blake3::hash(value).as_bytes());
    data.extend_from_slice(value);
    let tmp = path.with_extension("tmp");
    // Reconstructible cache: atomic rename and content verification suffice;
    // atlas and regional artifacts retain their separately fsynced publication.
    std::fs::write(&tmp, data)?;
    if cfg!(windows) && path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(tmp, path)?;
    let mut files = disk_tile_files(root);
    let mut size: u64 = files.iter().map(|(_, _, bytes)| bytes).sum();
    files.sort_by_key(|(_, modified, _)| *modified);
    for (file, _, bytes) in files {
        if size <= TILE_DISK_CAPACITY {
            break;
        }
        if std::fs::remove_file(file).is_ok() {
            size -= bytes;
        }
    }
    Ok(())
}

fn disk_tile_files(root: &FsPath) -> Vec<(PathBuf, std::time::SystemTime, u64)> {
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()?.to_str()? != "bin" {
                return None;
            }
            let metadata = path.metadata().ok()?;
            Some((
                path,
                metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                metadata.len(),
            ))
        })
        .collect()
}

async fn performance(State(app): State<App>) -> Json<serde_json::Value> {
    let metrics = &app.tile_metrics;
    let load = |v: &AtomicU64| v.load(Ordering::Relaxed);
    let cache = app.tiles.read().await;
    let tiles = serde_json::json!({
        "requests":load(&metrics.requests),"memory_hits":load(&metrics.memory_hits),
        "disk_hits":load(&metrics.disk_hits),"computations":load(&metrics.computations),
        "coalesced":load(&metrics.coalesced),"queue_ms":load(&metrics.queue_us) as f64/1000.0,
        "compute_ms":load(&metrics.compute_us) as f64/1000.0,
        "entries":cache.entries.len(),"bytes":cache.size,"capacity_bytes":cache.capacity,
        "evictions":cache.evictions,"corrupt_disk_entries":load(&metrics.corrupt_disk_entries),
        "workers":4,"available_workers":app.tile_workers.available_permits()
    });
    drop(cache);
    let root = app.data.join("tile-cache");
    let disk = tokio::task::spawn_blocking(move || {
        let files = disk_tile_files(&root);
        serde_json::json!({"entries":files.len(),"bytes":files.iter().map(|(_,_,size)|size).sum::<u64>(),"capacity_bytes":TILE_DISK_CAPACITY})
    }).await.unwrap_or(serde_json::Value::Null);
    Json(serde_json::json!({"tiles":tiles,"disk":disk,"regions":regions::performance()}))
}

async fn tile(
    State(app): State<App>,
    Path((id, face, level, x, y)): Path<(String, u8, u8, u32, u32)>,
    Query(q): Query<TileQuery>,
) -> Result<Response, ApiError> {
    let started = std::time::Instant::now();
    if face >= 6 || level > 24 || x >= 1u32 << level || y >= 1u32 << level {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "clé de tuile invalide".into(),
        ));
    }
    if !(2..=128).contains(&q.segments) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "segments doit être entre 2 et 128".into(),
        ));
    }
    if q.phase.is_some_and(|phase| !phase.is_finite()) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "phase invalide".into()));
    }
    let generator = q.generator.as_deref().unwrap_or("coherent");
    if !["spatial", "analytic", "coherent", "contracts"].contains(&generator) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "générateur inconnu".into(),
        ));
    }
    let contracts = generator == "contracts";
    let analytic = generator == "analytic" || generator == "coherent";
    let format = q.format;
    if contracts && matches!(format, TileFormat::Postcard) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Postcard n’est pas disponible pour les contrats".into(),
        ));
    }
    let key = format!(
        "{SURFACE_REVISION}/{id}/{face}/{level}/{x}/{y}/{}/{:?}/{generator}/{format:?}",
        q.segments, q.phase
    );
    app.tile_metrics.requests.fetch_add(1, Ordering::Relaxed);
    if let Some(bytes) = app.tiles.write().await.get(&key) {
        app.tile_metrics.memory_hits.fetch_add(1, Ordering::Relaxed);
        return Ok(tile_response(
            bytes,
            format,
            "memory",
            0.0,
            0.0,
            started.elapsed().as_secs_f64() * 1000.0,
        ));
    }
    let gate = {
        let mut flights = app.tile_flights.lock().await;
        if flights.len() > 512 {
            flights.retain(|_, gate| gate.strong_count() != 0);
        }
        if let Some(gate) = flights.get(&key).and_then(Weak::upgrade) {
            gate
        } else {
            let gate = Arc::new(tokio::sync::Mutex::new(()));
            flights.insert(key.clone(), Arc::downgrade(&gate));
            gate
        }
    };
    let waiting = gate.try_lock().is_err();
    if waiting {
        app.tile_metrics.coalesced.fetch_add(1, Ordering::Relaxed);
    }
    let flight = gate.lock_owned().await;
    if let Some(bytes) = app.tiles.write().await.get(&key) {
        app.tile_metrics.memory_hits.fetch_add(1, Ordering::Relaxed);
        app.tile_metrics
            .queue_us
            .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
        return Ok(tile_response(
            bytes,
            format,
            "coalesced",
            started.elapsed().as_secs_f64() * 1000.0,
            0.0,
            started.elapsed().as_secs_f64() * 1000.0,
        ));
    }
    let w = find_world(&app, &id).await?;
    let permit = app
        .tile_workers
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| ApiError(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    let queue_ms = started.elapsed().as_secs_f64() * 1000.0;
    app.tile_metrics
        .queue_us
        .fetch_add((queue_ms * 1000.0) as u64, Ordering::Relaxed);
    let region_data = app.data.clone();
    let disk_path = tile_disk_path(&app.data, &key);
    let cache = app.tiles.clone();
    let metrics = app.tile_metrics.clone();
    let result =
        tokio::task::spawn_blocking(move || -> Result<(bytes::Bytes, &'static str, f64)> {
            // Ownership outlives the HTTP future: cancelling a stale browser request
            // must not release its worker while its CPU/CUDA computation still runs.
            let (_flight, _permit) = (flight, permit);
            if let Some(bytes) = read_cached_tile(&disk_path, &metrics) {
                metrics.disk_hits.fetch_add(1, Ordering::Relaxed);
                cache.blocking_write().insert(key, bytes.clone());
                return Ok((bytes, "disk", 0.0));
            }
            let compute_started = std::time::Instant::now();
            metrics.computations.fetch_add(1, Ordering::Relaxed);
            let tile_key = TileKey { face, level, x, y };
            let compile = || -> Result<Vec<u8>> {
                if contracts {
                    let mut packet = hierarchy::compile_tile(&w, tile_key, q.segments)?;
                    if let Some(phase) = q.phase {
                        if let Some(vertices) = packet["vertices"].as_array_mut() {
                            for vertex in vertices {
                                let direction: [f64; 3] =
                                    serde_json::from_value(vertex["direction"].clone())?;
                                let s = w.seasonal_sample(direction, phase);
                                vertex["temperature_c"] = serde_json::json!(s.temperature_c);
                                vertex["moisture"] = serde_json::json!(s.moisture);
                                vertex["biome"] = serde_json::to_value(s.biome)?;
                                vertex["biome_mix"] = serde_json::to_value(s.biome_mix)?;
                            }
                        }
                    }
                    return Ok(serde_json::to_vec(&packet)?);
                }
                let mut tile = if analytic {
                    w.tile(tile_key, q.segments)?
                } else {
                    regions::compile_tile(&region_data, &w, tile_key, q.segments)?
                };
                if let Some(phase) = q.phase {
                    for vertex in &mut tile.vertices {
                        let seasonal = w.seasonal_sample(vertex.direction, phase);
                        vertex.temperature_c = seasonal.temperature_c;
                        vertex.moisture = seasonal.moisture;
                        vertex.biome = seasonal.biome;
                        vertex.biome_mix = seasonal.biome_mix;
                    }
                }
                match format {
                    TileFormat::Postcard => world_core::tile_wire::encode_tile(&tile),
                    TileFormat::Json => Ok(serde_json::to_vec(&tile)?),
                }
            };
            let bytes = bytes::Bytes::from(compile()?);
            let compute_ms = compute_started.elapsed().as_secs_f64() * 1000.0;
            metrics
                .compute_us
                .fetch_add((compute_ms * 1000.0) as u64, Ordering::Relaxed);
            if let Err(error) = write_cached_tile(&disk_path, &bytes) {
                tracing::warn!(%error,"tile disk cache write skipped");
            }
            cache.blocking_write().insert(key, bytes.clone());
            Ok((bytes, "generated", compute_ms))
        })
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(tile_response(
        result.0,
        format,
        result.1,
        queue_ms,
        result.2,
        started.elapsed().as_secs_f64() * 1000.0,
    ))
}
fn tile_response(
    bytes: bytes::Bytes,
    format: TileFormat,
    cache: &str,
    queue_ms: f64,
    compute_ms: f64,
    total_ms: f64,
) -> Response {
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        format.content_type().parse().unwrap(),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        "private, max-age=31536000, immutable".parse().unwrap(),
    );
    headers.insert("x-world-tile-cache", cache.parse().unwrap());
    headers.insert(
        "server-timing",
        format!("queue;dur={queue_ms:.3}, compute;dur={compute_ms:.3}, total;dur={total_ms:.3}")
            .parse()
            .unwrap(),
    );
    response
}
async fn family(
    State(app): State<App>,
    Path((id, face, level, x, y)): Path<(String, u8, u8, u32, u32)>,
    Query(q): Query<TileQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !(2..=128).contains(&q.segments) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "segments doit être entre 2 et 128".into(),
        ));
    }
    let world = find_world(&app, &id).await?;
    let result = tokio::task::spawn_blocking(move || {
        families::compile_family(&world, TileKey { face, level, x, y }, q.segments)
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(result))
}
#[derive(Deserialize)]
struct SampleQuery {
    lat: f64,
    lon: f64,
    phase: Option<f64>,
}
#[derive(Deserialize)]
struct SeasonQuery {
    phase: f64,
}
async fn season(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(q): Query<SeasonQuery>,
) -> Result<Json<World>, ApiError> {
    if !q.phase.is_finite() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "phase invalide".into()));
    }
    let world = find_world(&app, &id).await?;
    let result = tokio::task::spawn_blocking(move || world.seasonal_world(q.phase))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok(Json(result))
}
async fn sample(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(q): Query<SampleQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !q.lat.is_finite() || !q.lon.is_finite() || q.lat.abs() > 90.0 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "latitude ou longitude invalide".into(),
        ));
    }
    let lat = q.lat.to_radians();
    let lon = q.lon.to_radians();
    let w = find_world(&app, &id).await?;
    let direction = [lat.cos() * lon.cos(), lat.sin(), lat.cos() * lon.sin()];
    let s = if let Some(phase) = q.phase {
        if !phase.is_finite() {
            return Err(ApiError(StatusCode::BAD_REQUEST, "phase invalide".into()));
        }
        w.seasonal_sample(direction, phase)
    } else {
        w.sample(direction)
    };
    Ok(Json(serde_json::to_value(s).map_err(anyhow::Error::from)?))
}
async fn rivers(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let world = find_world(&app, &id).await?;
    Ok(Json(
        serde_json::json!({"world_id":world.id,"segments":world.river_segments(256),"display_reach_limit":256,"discharge_model":"annual mean runoff"}),
    ))
}
async fn lakes(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let world = find_world(&app, &id).await?;
    let lakes=world.lakes.iter().map(|lake|serde_json::json!({
        "id":lake.id,"level_m":lake.level_m,"area_m2":lake.area_m2,"volume_m3":lake.volume_m3,
        "inflow_m3_s":lake.inflow_m3_s,"outflow_m3_s":lake.outflow_m3_s,
        "direction":world.cells[lake.control_cell].position,"cell_count":lake.cells.len()
    })).collect::<Vec<_>>();
    Ok(Json(serde_json::json!({"world_id":world.id,"lakes":lakes})))
}
async fn region_reports(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let world = find_world(&app, &id).await?;
    let data = app.data.clone();
    let reports = tokio::task::spawn_blocking(move || regions::reports(&data, &world))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok(Json(serde_json::json!({"reports":reports})))
}
async fn human(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let world = find_world(&app, &id).await?;
    let layer = tokio::task::spawn_blocking(move || {
        world_human::HumanLayer::generate(&world, Default::default())
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    Ok(Json(
        serde_json::to_value(layer).map_err(anyhow::Error::from)?,
    ))
}
async fn report(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let world = find_world(&app, &id).await?;
    Ok(Json(validate_world(&world)?))
}

fn validate_world(w: &World) -> Result<serde_json::Value> {
    w.validate()?;
    anyhow::ensure!(
        w.cells.len() == 6 * w.config.resolution as usize * w.config.resolution as usize,
        "taille atlas incorrecte"
    );
    let mut uphill = 0usize;
    let mut outlets = 0.0;
    for (i, c) in w.cells.iter().enumerate() {
        anyhow::ensure!(
            c.height_m.is_finite() && c.water_m.is_finite() && c.discharge.is_finite(),
            "champ non fini cellule {i}"
        );
        if let Some(r) = c.receiver {
            anyhow::ensure!(
                r < w.cells.len() && r != i,
                "récepteur invalide cellule {i}"
            );
            if c.routing_height_m + 0.02 < w.cells[r].routing_height_m {
                uphill += 1;
            }
        } else {
            outlets += c.discharge as f64;
        }
    }
    // Détection indépendante des cycles par états, linéaire en nombre de cellules.
    let mut state = vec![0u8; w.cells.len()];
    for start in 0..w.cells.len() {
        if state[start] != 0 {
            continue;
        }
        let mut path = Vec::new();
        let mut cursor = Some(start);
        while let Some(i) = cursor {
            if state[i] != 0 {
                anyhow::ensure!(state[i] != 1, "cycle de drainage à {i}");
                break;
            }
            state[i] = 1;
            path.push(i);
            cursor = w.cells[i].receiver;
        }
        for i in path {
            state[i] = 2;
        }
    }
    anyhow::ensure!(
        uphill == 0,
        "{uphill} récepteurs remontent le potentiel de drainage"
    );
    let runoff = w.stats.total_runoff_m3_s;
    let losses = w
        .lakes
        .iter()
        .map(|l| l.evaporation_m3_s + l.infiltration_m3_s + l.storage_change_m3_s)
        .sum::<f64>();
    let relative = (outlets + losses - runoff).abs() / runoff.abs().max(1.0);
    anyhow::ensure!(relative < 1e-4, "bilan de débit non conservé ({relative})");
    Ok(
        serde_json::json!({"valid":true,"cells":w.cells.len(),"drainage_cycles":0,"uphill_potential_receivers":uphill,"runoff_m3_s":runoff,"outlets_m3_s":outlets,"lake_losses_and_storage_m3_s":losses,"water_relative_residual":relative,"erosion":w.erosion,"lake_count":w.lakes.len(),"scope":"atlas physique; érosion fluviale, transport sédimentaire et réservoirs; climat simplifié"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_cache_keeps_recently_used_entries_and_rejects_corrupt_disk_data() {
        let mut cache = TileCache {
            capacity: 6,
            ..Default::default()
        };
        cache.insert("a".into(), bytes::Bytes::from_static(b"aaa"));
        cache.insert("b".into(), bytes::Bytes::from_static(b"bbb"));
        assert!(cache.get("a").is_some());
        cache.insert("c".into(), bytes::Bytes::from_static(b"ccc"));
        assert!(cache.get("a").is_some() && cache.get("b").is_none());
        assert_eq!(cache.size, 6);
        let root =
            std::env::temp_dir().join(format!("world-cache-checksum-{}", std::process::id()));
        let path = tile_disk_path(&root, "test-entry");
        let metrics = TileMetrics::default();
        write_cached_tile(&path, b"original").unwrap();
        assert_eq!(&read_cached_tile(&path, &metrics).unwrap()[..], b"original");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[32] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        assert!(read_cached_tile(&path, &metrics).is_none());
        assert_eq!(metrics.corrupt_disk_entries.load(Ordering::Relaxed), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn concurrent_requests_and_disk_reuse_preserve_exact_tile_bytes() {
        let world = Arc::new(
            World::generate(WorldConfig {
                seed: 239991,
                resolution: 8,
                erosion_steps: 0,
                ..Default::default()
            })
            .unwrap(),
        );
        let data = std::env::temp_dir().join(format!(
            "world-cache-order-{}-{}",
            std::process::id(),
            world.id
        ));
        let app = App {
            data: data.clone(),
            world: Arc::new(RwLock::new(Some(world.clone()))),
            jobs: Default::default(),
            generation: Arc::new(Semaphore::new(1)),
            counter: Default::default(),
            tiles: Default::default(),
            tile_workers: Arc::new(Semaphore::new(4)),
            tile_flights: Default::default(),
            tile_metrics: Default::default(),
        };
        async fn request(app: &App, world: &World, x: u32) -> Response {
            tile(
                State(app.clone()),
                Path((world.id.clone(), 5, 2, x, 2)),
                Query(TileQuery {
                    segments: 32,
                    phase: None,
                    generator: None,
                    format: TileFormat::Postcard,
                }),
            )
            .await
            .unwrap_or_else(|e| panic!("{}", e.1))
        }
        // Keep the first request waiting for a worker until both peers have
        // joined its flight; a fast cached-field computation must not make the
        // coalescing assertion depend on scheduler timing.
        let held_workers = app
            .tile_workers
            .clone()
            .acquire_many_owned(4)
            .await
            .unwrap();
        let release_workers = async {
            while app.tile_metrics.requests.load(Ordering::Relaxed) < 3 {
                tokio::task::yield_now().await;
            }
            drop(held_workers);
        };
        let (_, a, b, c) = tokio::join!(
            release_workers,
            request(&app, &world, 3),
            request(&app, &world, 3),
            request(&app, &world, 3)
        );
        let a = axum::body::to_bytes(a.into_body(), usize::MAX)
            .await
            .unwrap();
        let b = axum::body::to_bytes(b.into_body(), usize::MAX)
            .await
            .unwrap();
        let c = axum::body::to_bytes(c.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(app.tile_metrics.computations.load(Ordering::Relaxed), 1);
        assert!(app.tile_metrics.coalesced.load(Ordering::Relaxed) >= 1);
        *app.tiles.write().await = TileCache::default();
        let _other = request(&app, &world, 2).await;
        let disk = request(&app, &world, 3).await;
        assert_eq!(disk.headers()["x-world-tile-cache"], "disk");
        let disk = axum::body::to_bytes(disk.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(a, disk);
        *app.tiles.write().await = TileCache::default();
        std::fs::remove_dir_all(data.join("tile-cache")).unwrap();
        let recomputed = request(&app, &world, 3).await;
        let recomputed = axum::body::to_bytes(recomputed.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(a, recomputed); // independent request order, without either cache.
        let Json(stats) = performance(State(app)).await;
        assert_eq!(stats["tiles"]["computations"], 3);
        assert_eq!(stats["tiles"]["disk_hits"], 1);
        std::fs::remove_dir_all(data).unwrap();
    }

    #[tokio::test]
    async fn cancelled_request_finishes_caching_without_releasing_its_worker_early() {
        let world = Arc::new(
            World::generate(WorldConfig {
                seed: 917551,
                resolution: 8,
                erosion_steps: 0,
                ..Default::default()
            })
            .unwrap(),
        );
        let data = std::env::temp_dir().join(format!("world-cache-cancel-{}", std::process::id()));
        let app = App {
            data: data.clone(),
            world: Arc::new(RwLock::new(Some(world.clone()))),
            jobs: Default::default(),
            generation: Arc::new(Semaphore::new(1)),
            counter: Default::default(),
            tiles: Default::default(),
            tile_workers: Arc::new(Semaphore::new(1)),
            tile_flights: Default::default(),
            tile_metrics: Default::default(),
        };
        // Hold publication to make cancellation during a running blocking job
        // deterministic, instead of depending on how fast this machine samples.
        let publication = TILE_DISK_WRITES.lock().unwrap();
        let request_app = app.clone();
        let request = tokio::spawn(async move {
            tile(
                State(request_app),
                Path((world.id.clone(), 5, 2, 3, 2)),
                Query(TileQuery {
                    segments: 32,
                    phase: None,
                    generator: None,
                    format: TileFormat::Postcard,
                }),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while app.tile_metrics.computations.load(Ordering::Relaxed) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        request.abort();
        let _ = request.await;
        assert_eq!(app.tile_workers.available_permits(), 0);
        drop(publication);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while app.tiles.read().await.entries.is_empty()
                || app.tile_workers.available_permits() != 1
            {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(app.tile_workers.available_permits(), 1);
        std::fs::remove_dir_all(data).unwrap();
    }

    #[tokio::test]
    async fn coherent_tiles_keep_the_same_field_across_regional_thresholds() {
        let world = Arc::new(
            World::generate(WorldConfig {
                resolution: 8,
                erosion_steps: 0,
                ..Default::default()
            })
            .unwrap(),
        );
        let data =
            std::env::temp_dir().join(format!("world-coherent-field-{}", std::process::id()));
        let app = App {
            data: data.clone(),
            world: Arc::new(RwLock::new(Some(world.clone()))),
            jobs: Default::default(),
            generation: Arc::new(Semaphore::new(1)),
            counter: Default::default(),
            tiles: Default::default(),
            tile_workers: Arc::new(Semaphore::new(4)),
            tile_flights: Default::default(),
            tile_metrics: Default::default(),
        };
        for level in [0, 1, 5, 6, 9, 10] {
            let key = TileKey {
                face: 4,
                level,
                x: 0,
                y: 0,
            };
            let response = tile(
                State(app.clone()),
                Path((world.id.clone(), 4, level, 0, 0)),
                Query(TileQuery {
                    segments: 8,
                    phase: None,
                    generator: Some("coherent".into()),
                    format: TileFormat::Postcard,
                }),
            )
            .await
            .unwrap_or_else(|error| panic!("{}", error.1));
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let received = world_core::tile_wire::decode_tile(&bytes).unwrap();
            let expected = world.tile(key, 8).unwrap();
            assert_eq!(
                &bytes[..],
                world_core::tile_wire::encode_tile(&expected).unwrap()
            );
            assert_eq!(
                received.vertices[0].height_m,
                world.sample(received.vertices[0].direction).height_m
            );
        }
        std::fs::remove_dir_all(data).unwrap();
    }

    #[tokio::test]
    async fn binary_tiles_match_json_with_seasons_and_separate_cache_entries() {
        let world = Arc::new(
            World::generate(WorldConfig {
                resolution: 8,
                erosion_steps: 0,
                ..Default::default()
            })
            .unwrap(),
        );
        let app = App {
            data: std::env::temp_dir(),
            world: Arc::new(RwLock::new(Some(world.clone()))),
            jobs: Arc::new(RwLock::new(HashMap::new())),
            generation: Arc::new(Semaphore::new(1)),
            counter: Arc::new(AtomicU64::new(0)),
            tiles: Arc::new(RwLock::new(TileCache::default())),
            tile_workers: Arc::new(Semaphore::new(4)),
            tile_flights: Default::default(),
            tile_metrics: Default::default(),
        };
        let query = |format| TileQuery {
            segments: 128,
            phase: Some(0.25),
            generator: None,
            format,
        };
        let binary = tile(
            State(app.clone()),
            Path((world.id.clone(), 4, 0, 0, 0)),
            Query(query(TileFormat::Postcard)),
        )
        .await
        .unwrap_or_else(|error| panic!("{}", error.1));
        assert_eq!(
            binary.headers()[axum::http::header::CONTENT_TYPE],
            world_core::tile_wire::CONTENT_TYPE
        );
        let binary = axum::body::to_bytes(binary.into_body(), usize::MAX)
            .await
            .unwrap();
        let decoded = world_core::tile_wire::decode_tile(&binary).unwrap();
        let seasonal = world.seasonal_sample(decoded.vertices[0].direction, 0.25);
        assert_eq!(decoded.vertices[0].temperature_c, seasonal.temperature_c);
        assert_eq!(decoded.vertices[0].biome_mix, seasonal.biome_mix);
        let json = tile(
            State(app.clone()),
            Path((world.id.clone(), 4, 0, 0, 0)),
            Query(query(TileFormat::Json)),
        )
        .await
        .unwrap_or_else(|error| panic!("{}", error.1));
        assert_eq!(
            json.headers()[axum::http::header::CONTENT_TYPE],
            "application/json"
        );
        let json = axum::body::to_bytes(json.into_body(), usize::MAX)
            .await
            .unwrap();
        let mut decoded_json: world_core::Tile = serde_json::from_slice(&json).unwrap();
        for (json_sample, binary_sample) in decoded_json.vertices.iter_mut().zip(&decoded.vertices)
        {
            // JSON's decimal f64 parser may change the final bit. Postcard
            // preserves the original direction exactly; compare geometric
            // agreement before normalizing those parser-only differences.
            for i in 0..3 {
                assert!((json_sample.direction[i] - binary_sample.direction[i]).abs() < 1e-14);
            }
            json_sample.direction = binary_sample.direction;
        }
        assert!(
            world_core::tile_wire::encode_tile(&decoded_json)
                .unwrap()
                .as_slice()
                == &binary[..]
        );
        assert_eq!(app.tiles.read().await.entries.len(), 2);
        assert!(binary.len() * 3 < json.len());
        eprintln!(
            "128-segment tile: JSON {} bytes, Postcard {} bytes",
            json.len(),
            binary.len()
        );
        let mut contracts = query(TileFormat::Postcard);
        contracts.generator = Some("contracts".into());
        let Err(error) = tile(
            State(app),
            Path((world.id.clone(), 4, 0, 0, 0)),
            Query(contracts),
        )
        .await
        else {
            panic!("binary contracts must be rejected");
        };
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn stored_world_round_trip_and_corruption_detection() {
        let world = World::generate(WorldConfig {
            resolution: 8,
            ..Default::default()
        })
        .unwrap();
        let dir = std::env::temp_dir().join(format!(
            "world-store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        save_world(&dir, &world).unwrap();
        let loaded = load_world(&dir, None).unwrap();
        assert_eq!(loaded.id, world.id);
        assert_eq!(loaded.cells.len(), world.cells.len());
        assert!(validate_world(&loaded).is_ok());
        std::fs::write(
            dir.join("worlds").join(&world.id).join("atlas.json"),
            b"corrupt",
        )
        .unwrap();
        assert!(load_world(&dir, None)
            .unwrap_err()
            .to_string()
            .contains("checksum"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unsafe_world_paths_are_rejected() {
        for id in ["../latest", "/tmp/atlas", "", "world-id", "a/b"] {
            assert!(checked_id(id).is_err());
        }
        assert!(checked_id("abcdef0123456789").is_ok());
    }
}
