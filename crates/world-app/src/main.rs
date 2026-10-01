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
        Arc,
    },
};
use tokio::sync::{RwLock, Semaphore};
use tower_http::{compression::CompressionLayer, services::ServeDir};
use world_core::{TileKey, World, WorldConfig};

mod families;
mod hierarchy;

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
}

#[derive(Default)]
struct TileCache {
    entries: HashMap<String, bytes::Bytes>,
    order: VecDeque<String>,
    size: usize,
}
impl TileCache {
    fn insert(&mut self, key: String, value: bytes::Bytes) {
        if self.entries.contains_key(&key) {
            return;
        }
        self.size += value.len();
        self.order.push_back(key.clone());
        self.entries.insert(key, value);
        while self.size > 64 * 1024 * 1024 || self.entries.len() > 128 {
            if let Some(key) = self.order.pop_front() {
                if let Some(value) = self.entries.remove(&key) {
                    self.size -= value.len();
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
            };
            let router = Router::new()
                .route("/api/health", get(|| async { Json(serde_json::json!({"ok":true,"backend":"rust-cpu","version":env!("CARGO_PKG_VERSION")})) }))
                .route("/api/worlds", post(create_world))
                .route("/api/jobs/{id}", get(get_job))
                .route("/api/world/latest", get(latest))
                .route("/api/worlds/{id}", get(by_id))
                .route("/api/worlds/{id}/manifest", get(manifest))
                .route("/api/worlds/{id}/season", get(season))
                .route("/api/worlds/{id}/tiles/{face}/{level}/{x}/{y}", get(tile))
                .route("/api/worlds/{id}/families/{face}/{level}/{x}/{y}", get(family))
                .route("/api/worlds/{id}/sample", get(sample))
                .route("/api/worlds/{id}/rivers", get(rivers))
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
async fn manifest(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let w = find_world(&app, &id).await?;
    Ok(Json(
        serde_json::json!({"id":w.id,"config":w.config,"stats":w.stats,"sea_level_m":w.sea_level_m,"tile_segments":32,"max_tile_level":22,"numeric_profile":"cpu-f64-v1"}),
    ))
}
#[derive(Deserialize)]
struct TileQuery {
    #[serde(default = "default_segments")]
    segments: u32,
    phase: Option<f64>,
    generator: Option<String>,
}
fn default_segments() -> u32 {
    32
}
async fn tile(
    State(app): State<App>,
    Path((id, face, level, x, y)): Path<(String, u8, u8, u32, u32)>,
    Query(q): Query<TileQuery>,
) -> Result<Response, ApiError> {
    if !(2..=128).contains(&q.segments) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "segments doit être entre 2 et 128".into(),
        ));
    }
    if q.phase.is_some_and(|phase| !phase.is_finite()) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "phase invalide".into()));
    }
    let generator = q.generator.as_deref().unwrap_or("spatial");
    if !["spatial", "contracts"].contains(&generator) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "générateur inconnu".into(),
        ));
    }
    let contracts = generator == "contracts";
    let key = format!(
        "{id}/{face}/{level}/{x}/{y}/{}/{:?}/{generator}",
        q.segments, q.phase
    );
    if let Some(bytes) = app.tiles.read().await.entries.get(&key).cloned() {
        return Ok(tile_response(bytes));
    }
    let _permit = app
        .tile_workers
        .acquire()
        .await
        .map_err(|e| ApiError(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    let w = find_world(&app, &id).await?;
    let result = tokio::task::spawn_blocking(move || -> Result<serde_json::Value> {
        let key = TileKey { face, level, x, y };
        let mut packet = if contracts {
            hierarchy::compile_tile(&w, key, q.segments)?
        } else {
            serde_json::to_value(w.tile(key, q.segments)?)?
        };
        if let Some(phase) = q.phase {
            if let Some(vertices) = packet["vertices"].as_array_mut() {
                for vertex in vertices {
                    let direction: [f64; 3] = serde_json::from_value(vertex["direction"].clone())?;
                    let s = w.seasonal_sample(direction, phase);
                    vertex["temperature_c"] = serde_json::json!(s.temperature_c);
                    vertex["moisture"] = serde_json::json!(s.moisture);
                    vertex["biome"] = serde_json::to_value(s.biome)?;
                }
            }
        }
        Ok(packet)
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    let bytes = bytes::Bytes::from(serde_json::to_vec(&result).map_err(anyhow::Error::from)?);
    app.tiles.write().await.insert(key, bytes.clone());
    Ok(tile_response(bytes))
}
fn tile_response(bytes: bytes::Bytes) -> Response {
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/json"),
            (
                axum::http::header::CACHE_CONTROL,
                "private, max-age=31536000, immutable",
            ),
        ],
        bytes,
    )
        .into_response()
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
        serde_json::json!({"world_id":world.id,"segments":world.rivers(),"discharge_model":"annual mean runoff"}),
    ))
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
            if c.water_m + 0.02 < w.cells[r].water_m {
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
        "{uphill} récepteurs remontent la surface d'eau"
    );
    let runoff = w.stats.total_runoff_m3_s;
    let relative = (outlets - runoff).abs() / runoff.abs().max(1.0);
    anyhow::ensure!(relative < 1e-4, "bilan de débit non conservé ({relative})");
    Ok(
        serde_json::json!({"valid":true,"cells":w.cells.len(),"drainage_cycles":0,"uphill_receivers":uphill,"runoff_m3_s":runoff,"outlets_m3_s":outlets,"water_relative_residual":relative,"scope":"atlas; détails locaux et climat simplifiés"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
