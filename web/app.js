import init, * as viewer from '/pkg/world_viewer.js';
import { TileStream } from './tile-stream.js';
import { CameraPredictor } from './camera-predictor.js';
import { setupSettings } from './settings.js';
const predictor = new CameraPredictor();

const $ = (id) => document.getElementById(id);
const canvas = $('planet');
$('conservative-refinement').checked = false;
let activeWorld;
let frameCount = 0;
let lastFps = performance.now();
let lastTile;
const loadedTiles = new Map();
let tileStream;
const renderMetrics = { frames: 0, max_render_ms: 0, render_ms: 0 };
let generationBusy = false;
let rendererFailed = false;
let riverData;
let seasonPhase;
let renderEpoch = 0;
const destinations = new Map();

function message(text) {
  $('message').textContent = text;
  $('message').hidden = !text;
}

async function api(path, options = {}) {
  const response = await fetch(path, options);
  if (!response.ok) {
    const body = await response.text();
    throw new Error(`${response.status} · ${body.slice(0, 220)}`);
  }
  return response.json();
}

const number = (value, digits = 0) => Number(value).toLocaleString('fr-FR', { maximumFractionDigits: digits });
const settings = setupSettings(document, message);

function addMetric(label, value) {
  const row = document.createElement('div');
  row.className = 'metric';
  const name = document.createElement('span');
  name.textContent = label;
  const content = document.createElement('strong');
  content.textContent = value;
  row.append(name, content);
  $('stats').append(row);
}

function loadWorld(world) {
  viewer.set_world(JSON.stringify(world));
  viewer.set_overlays($('show-rivers').checked, false);
  activeWorld = world;
  destinations.clear();
  predictor.reset();
  $('destination').replaceChildren(new Option('Choisir un lieu', ''));
  $('fly').disabled = true;
  let peak;
  let barePeak;
  let coast;
  for (const cell of world.cells) {
    if (!cell.ocean && !cell.lake) {
      if (!peak || cell.height_m > peak.height_m) peak = cell;
      if (cell.biome !== 'Ice' && (!barePeak || cell.height_m > barePeak.height_m)) barePeak = cell;
      if (cell.biome !== 'Ice' && cell.water_body_id === 0 && cell.height_m >= 0 && (!coast || cell.height_m < coast.height_m)) coast = cell;
    }
  }
  if (peak) addDestination('peak', 'Sommet · continent', peak.position ?? peak.direction, 15000);
  if (barePeak) addDestination('relief', 'Relief · sans glace', barePeak.position ?? barePeak.direction, 5000);
  if (coast) addDestination('coast', 'Côte', coast.position ?? coast.direction, 1500);
  lastTile = undefined;
  loadedTiles.clear();
  renderEpoch += 1; tileStream?.reset(renderEpoch);
  riverData = undefined;
  seasonPhase = undefined;
  $('season').value = 0;
  $('season-value').value = 'Moyenne annuelle';
  viewer.set_rivers('{"segments":[]}');
  viewer.set_human('{"settlements":[],"roads":[]}');
  loadOverlays(world.id);
  $('world-name').textContent = `Planète ${world.config.seed}`;
  $('world-meta').textContent = `${number(world.config.radius_m / 1000)} km · ${number(world.stats?.cells ?? 6 * world.config.resolution ** 2)} cellules d'atlas`;
  settings.apply(world.config, { applied: true });
  $('visit-altitude').max = String(world.config.radius_m * 7);
  $('stats').replaceChildren();
  addMetric('Identifiant', String(world.id).slice(0, 16));
  addMetric('Rayon', `${number(world.config.radius_m / 1000)} km`);
  addMetric('Atlas', `${world.config.resolution}² × 6 faces`);
  const stats = world.stats ?? {};
  for (const [key, label, unit] of [
    ['ocean_fraction', 'Surface océanique', '%'], ['land_fraction', 'Terres émergées', '%'],
    ['min_height_m', 'Altitude minimale', 'm'], ['max_height_m', 'Altitude maximale', 'm'],
    ['river_cells', 'Cellules de rivière', ''], ['lake_count', 'Lacs', ''], ['lake_cells', 'Cellules de lac', ''],
    ['max_discharge_m3_s', 'Débit maximal', 'm³/s'], ['total_runoff_m3_s', 'Ruissellement total', 'm³/s'],
  ]) {
    if (typeof stats[key] === 'number') addMetric(label, `${number(unit === '%' ? stats[key] * 100 : stats[key], unit === '%' ? 1 : 0)} ${unit}`.trim());
  }
  if (stats.ocean_volume_m3) addMetric('Volume océanique', `${number(stats.ocean_volume_m3 / 1e9)} km³`);
  if (typeof stats.water_balance_relative_error === 'number') addMetric('Erreur du bilan hydrique', `${number(stats.water_balance_relative_error * 100, 5)} %`);
  $('status').textContent = 'Monde chargé · exploration disponible';
}

function addDestination(id, label, direction, altitude_m = 30000, kind = 'terrain') {
  if (typeof id !== 'string' || !id || typeof label !== 'string'
    || !Array.isArray(direction) || direction.length !== 3 || !direction.every(Number.isFinite)
    || direction.every((value) => value === 0)) return;
  const altitude = Number.isFinite(altitude_m) && altitude_m > 0 ? altitude_m : 30000;
  destinations.set(id, { direction, altitude_m: altitude, kind });
  const option = [...$('destination').options].find((entry) => entry.value === id);
  if (option) option.textContent = label;
  else $('destination').add(new Option(label, id));
}

async function loadOverlays(worldId) {
  const results = await Promise.allSettled([
    api(`/api/worlds/${worldId}/rivers`).then((data) => {
      if (activeWorld?.id !== worldId) return;
      riverData = JSON.stringify(data); viewer.set_rivers(riverData);
      // Canonical interior points are verified wet by the server. An atlas cell
      // or the start of a segment can lie beside the visible, warped channel.
      for (const place of data.destinations ?? []) {
        addDestination(place.id, place.label, place.direction, place.altitude_m, 'river');
      }
    }),
    api(`/api/worlds/${worldId}/lakes`).then((data) => {
      if (activeWorld?.id !== worldId) return;
      const lake = data.lakes?.filter((l) => l.volume_m3 > 0).sort((a, b) => b.area_m2 - a.area_m2)[0];
      if (lake) addDestination('lake', 'Grand lac', lake.direction, Math.max(1200, Math.min(30000, Math.sqrt(lake.area_m2) * 1.8)), 'lake');
    }),
    api(`/api/worlds/${worldId}/landforms`).then((data) => {
      if (activeWorld?.id !== worldId || (data.world_id && data.world_id !== worldId)) return;
      for (const place of data.features ?? []) {
        addDestination(place.id, place.label, place.direction, place.altitude_m, place.kind);
      }
    }),
  ]);
  for (const result of results) if (result.status === 'rejected') console.warn('Couche indisponible', result.reason);
}

async function changeSeason() {
  if (!activeWorld) return;
  const month = Number($('season').value);
  const worldId = activeWorld.id;
  try {
    const world = await api(month === 0 ? `/api/worlds/${worldId}/preview?resolution=128` : `/api/worlds/${worldId}/preview?resolution=128&phase=${(month - 1) / 12}`);
    if (activeWorld?.id !== worldId || Number($('season').value) !== month) return;
    viewer.set_world(JSON.stringify(world));
    if (riverData) viewer.set_rivers(riverData);
    seasonPhase = month === 0 ? undefined : (month - 1) / 12;
    renderEpoch += 1; tileStream?.reset(renderEpoch);
    loadedTiles.clear();
    $('status').textContent = month === 0 ? 'Climat moyen annuel' : `Saison ${month}/12 · forçage solaire réduit, débits annuels`;
  } catch (error) { message(`Saison : ${error.message}`); }
}

async function generate() {
  if (generationBusy) return;
  generationBusy = true;
  $('generate').disabled = true;
  $('generate').firstElementChild.textContent = 'Calcul en cours…';
  message('');
  try {
    const config = settings.read();
    settings.setBusy(true);
    $('status').textContent = 'Calcul du relief, du climat et de l’hydrologie…';
    const result = await api('/api/worlds', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(config) });
    if (result.cells) {
      loadWorld(result);
    } else {
      const id = result.job_id ?? result.id;
      for (;;) {
        await new Promise((resolve) => setTimeout(resolve, 600));
        const job = await api(`/api/jobs/${id}`);
        $('status').textContent = job.message ?? (job.status === 'queued' ? 'Génération dans la file…' : 'Calcul du monde…');
        if (job.status === 'failed') throw new Error(job.error ?? 'La génération a échoué');
        if (job.status === 'ready' || job.status === 'complete' || job.status === 'completed') {
          loadWorld(await api(job.world_id ? `/api/worlds/${job.world_id}/preview?resolution=128` : '/api/world/latest/preview?resolution=128'));
          break;
        }
      }
    }
  } catch (error) {
    message(`Génération : ${error.message}`);
    $('status').textContent = 'Génération interrompue';
  } finally {
    generationBusy = false;
    settings.setBusy(false);
    $('generate').disabled = false;
    $('generate').firstElementChild.textContent = 'Générer la planète';
  }
}

function updateDetail() {
  if (!activeWorld || !tileStream || rendererFailed) return;
  const info = JSON.parse(viewer.view_info());
  const altitude = info.altitude_m;
  $('camera').textContent = altitude >= 1000 ? `Altitude ${number(altitude / 1000, 1)} km` : `Altitude ${number(altitude)} m`;
  const worldId = activeWorld.id;
  const phase = seasonPhase;
  const conservative = $('conservative-refinement').checked;
  const keyOf = (tile) => `${worldId}/${tile.face}/${tile.level}/${tile.x}/${tile.y}`;
  // The viewer orders essential ancestors and visible refinement before
  // future detail. Preserve that order: a finest tile is useless without its
  // resident ancestry. Prediction never modifies the camera/render cut.
  const ordered = info.requests ?? [info];
  const critical = new Set(ordered.slice(0, info.essential_count ?? ordered.length).map(keyOf));
  const memoryPressure = info.cpu_bytes >= (info.cpu_budget_bytes ?? Infinity) * 0.8
    || info.gpu_bytes >= (info.gpu_budget_bytes ?? Infinity) * 0.8;
  // Coarsening can require an evicted parent above the new target level.
  // Keep every cut-maintenance request even under memory pressure.
  const requests = [...ordered];
  renderMetrics.forecast_requests = 0;
  const forecast = predictor.sample(info, performance.now(), activeWorld.config.radius_m);
  if (forecast && !memoryPressure && typeof viewer.prediction_requests === 'function') {
    const future = JSON.parse(viewer.prediction_requests(JSON.stringify(forecast.direction), forecast.altitude_m));
    const seen = new Set(requests.map(keyOf));
    let added = 0;
    const allowance = Math.max(0, 64 - (ordered.length - critical.size));
    for (const tile of future) {
      if (added >= allowance) break;
      const key = keyOf(tile);
      if (!seen.has(key) && !loadedTiles.has(key)) { requests.push(tile); seen.add(key); added += 1; }
    }
    renderMetrics.forecast_requests = added;
  }
  tileStream.update(requests, {
    epoch: renderEpoch, keyOf, critical, residentKeys: info.resident_keys, radius: activeWorld.config.radius_m,
    exaggeration: Number($('exaggeration').value), format: conservative ? 'json' : 'postcard',
    urlOf: (tile) => new URL(`/api/worlds/${worldId}/tiles/${tile.face}/${tile.level}/${tile.x}/${tile.y}?segments=64&format=${conservative ? 'json' : 'postcard'}${phase === undefined ? '' : `&phase=${phase}`}${conservative ? '&generator=contracts' : '&generator=coherent'}&surface_revision=${activeWorld.surface_revision ?? 'spherical-coast-9'}`, location.origin).href,
  });
  if (info.level >= 0) {
    $('status').textContent = `Exploration · niveau ${info.active_level ?? '…'} / ${info.level} · ${loadedTiles.size} tuiles en cache`;
  }
}

let pointer;
canvas.addEventListener('pointerdown', (event) => {
  pointer = { x: event.clientX, y: event.clientY };
  canvas.setPointerCapture(event.pointerId);
});
canvas.addEventListener('pointermove', (event) => {
  if (!pointer) return;
  viewer.rotate(event.clientX - pointer.x, event.clientY - pointer.y);
  pointer = { x: event.clientX, y: event.clientY };
});
for (const event of ['pointerup', 'pointercancel']) canvas.addEventListener(event, () => { pointer = undefined; });
canvas.addEventListener('wheel', (event) => { event.preventDefault(); viewer.zoom(event.deltaY); }, { passive: false });
$('generator').addEventListener('submit', (event) => { event.preventDefault(); generate(); });
$('reset').addEventListener('click', () => {
  viewer.reset_view();
  predictor.reset();
  $('view-tilt').value = '0';
  $('view-tilt-value').value = '0°';
});
function visit(direction) {
  if (!activeWorld || !direction) return;
  const altitude = Number($('visit-altitude').value);
  if (!Number.isFinite(altitude) || altitude < 1 || altitude > activeWorld.config.radius_m * 7) {
    message(`L’altitude doit être comprise entre 1 et ${number(activeWorld.config.radius_m * 7)} m.`);
    return;
  }
  viewer.fly_to(JSON.stringify(direction), altitude);
  predictor.reset();
  message('');
  $('status').textContent = `Arrivée à ${number(altitude)} m au-dessus du terrain`;
}
function selectDestination() {
  const place = destinations.get($('destination').value);
  $('fly').disabled = !place;
  if (place) {
    $('visit-altitude').value = String(Math.min(place.altitude_m, activeWorld.config.radius_m * 7));
    if (place.kind === 'river') {
      $('show-rivers').checked = true;
      viewer.set_overlays(true, false);
    }
  }
}
$('destination').addEventListener('change', selectDestination);
$('fly').addEventListener('click', () => {
  const place = destinations.get($('destination').value);
  if (place?.kind === 'river') {
    $('show-rivers').checked = true;
    viewer.set_overlays(true, false);
  }
  visit(place?.direction);
});
$('go-altitude').addEventListener('click', () => {
  if (!activeWorld) return;
  visit(JSON.parse(viewer.view_info()).direction);
});
$('layer').addEventListener('change', () => viewer.set_mode(Number($('layer').value)));
$('season').addEventListener('input', () => { $('season-value').value = Number($('season').value) === 0 ? 'Moyenne annuelle' : `Saison ${$('season').value} / 12`; });
$('season').addEventListener('change', changeSeason);
$('show-rivers').addEventListener('change', () => viewer.set_overlays($('show-rivers').checked, false));
$('conservative-refinement').addEventListener('change', () => {
  renderEpoch += 1; tileStream?.reset(renderEpoch);
  viewer.clear_tiles();
  loadedTiles.clear();
  $('contract-status').hidden = !$('conservative-refinement').checked;
  $('contract-status').textContent = 'Conservation des altitudes uniquement. Descendre pour vérifier le raffinement.';
});
$('exaggeration').addEventListener('input', () => { $('exaggeration-value').value = `×${$('exaggeration').value}`; });
$('exaggeration').addEventListener('change', () => { viewer.set_exaggeration(Number($('exaggeration').value)); viewer.clear_tiles(); loadedTiles.clear(); renderEpoch += 1; tileStream?.reset(renderEpoch); lastTile = undefined; updateDetail(); });
$('view-tilt').addEventListener('input', () => { $('view-tilt-value').value = `${$('view-tilt').value}°`; viewer.set_view_tilt(Number($('view-tilt').value)); });

function frame(now) {
  const ratio = Math.min(devicePixelRatio || 1, 2);
  const width = Math.round(canvas.clientWidth * ratio);
  const height = Math.round(canvas.clientHeight * ratio);
  if (canvas.width !== width || canvas.height !== height) { canvas.width = width; canvas.height = height; }
  const renderStarted = performance.now();
  tileStream?.installNext();
  try { viewer.render(width, height); } catch (error) { rendererFailed = true; tileStream?.worker.terminate(); message(`Rendu : ${error}`); return; }
  renderMetrics.render_ms = performance.now() - renderStarted;
  renderMetrics.max_render_ms = Math.max(renderMetrics.max_render_ms, renderMetrics.render_ms);
  renderMetrics.frames += 1;
  frameCount += 1;
  if (now - lastFps > 1000) { $('fps').textContent = `${Math.round(frameCount * 1000 / (now - lastFps))} fps`; frameCount = 0; lastFps = now; }
  requestAnimationFrame(frame);
}

try {
  await init();
  const backend = await viewer.start('planet', new URLSearchParams(location.search).get('backend') === 'webgl');
  const worker = new Worker(new URL('./tile-worker.js', import.meta.url), { type: 'module' });
  tileStream = new TileStream(worker, (packet) => viewer.install_prepared_tile(packet), (keys) => {
    loadedTiles.clear();
    for (const tile of keys) loadedTiles.set(`${activeWorld.id}/${tile.face}/${tile.level}/${tile.x}/${tile.y}`, true);
  });
  window.worldBuilderPerf = () => ({ ...renderMetrics, ...tileStream.metrics,
    view: JSON.parse(viewer.view_info()), in_flight: tileStream.jobs.size, queued: tileStream.ready.length });
  $('backend').textContent = `Rust / WASM · ${backend}`;
  $('runtime-dot').classList.add('ready');
  $('generate').disabled = false;
  requestAnimationFrame(frame);
  setInterval(updateDetail, 50);
  try { loadWorld(await api('/api/world/latest/preview?resolution=128')); } catch { await generate(); }
} catch (error) {
  $('backend').textContent = 'Moteur indisponible';
  $('status').textContent = 'Initialisation impossible';
  message(`Le moteur Rust/WebAssembly ne démarre pas : ${error}. Lancez scripts/build-web.sh, puis utilisez un navigateur récent avec WebGPU ou WebGL2 sur localhost.`);
}
