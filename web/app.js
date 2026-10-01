import init, * as viewer from '/pkg/world_viewer.js';
import { TileStream } from './tile-stream.js';

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
let humanData;
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
  activeWorld = world;
  destinations.clear();
  $('destination').replaceChildren(new Option('Choisir un lieu', ''));
  let peak;
  let river;
  let barePeak;
  let coast;
  for (const cell of world.cells) {
    if (!cell.ocean && !cell.lake) {
      if (!peak || cell.height_m > peak.height_m) peak = cell;
      if (cell.biome !== 'Ice' && (!barePeak || cell.height_m > barePeak.height_m)) barePeak = cell;
      if (!river || cell.discharge > river.discharge) river = cell;
      if (cell.biome !== 'Ice' && cell.water_body_id === 0 && cell.height_m >= 0 && (!coast || cell.height_m < coast.height_m)) coast = cell;
    }
  }
  if (peak) addDestination('peak', 'Sommet · continent', peak.position ?? peak.direction);
  if (barePeak) addDestination('relief', 'Relief · sans glace', barePeak.position ?? barePeak.direction);
  if (coast) addDestination('coast', 'Côte', coast.position ?? coast.direction);
  if (river && river.discharge > 0) addDestination('river', 'Grande rivière', river.position ?? river.direction);
  lastTile = undefined;
  loadedTiles.clear();
  renderEpoch += 1;
  riverData = undefined;
  humanData = undefined;
  seasonPhase = undefined;
  $('season').value = 0;
  $('season-value').value = 'Moyenne annuelle';
  viewer.set_rivers('{"segments":[]}');
  viewer.set_human('{"settlements":[],"roads":[]}');
  loadOverlays(world.id);
  $('world-name').textContent = `Planète ${world.config.seed}`;
  $('world-meta').textContent = `${number(world.config.radius_m / 1000)} km · ${number(world.stats?.cells ?? 6 * world.config.resolution ** 2)} cellules d'atlas`;
  $('seed').value = world.config.seed;
  $('radius').value = world.config.radius_m / 1000;
  $('resolution').value = world.config.resolution;
  if (world.config.ocean_fraction !== undefined) { $('ocean-fraction').value = world.config.ocean_fraction * 100; $('ocean-value').value = `${number(world.config.ocean_fraction * 100)} %`; }
  if (world.config.relief_m !== undefined) $('relief').value = world.config.relief_m;
  if (world.config.gravity_m_s2 !== undefined) $('gravity').value = world.config.gravity_m_s2;
  if (world.config.axial_tilt_deg !== undefined) $('tilt').value = world.config.axial_tilt_deg;
  $('erosion-steps').value = world.config.erosion_steps ?? 24;
  $('geological-years').value = world.config.geological_years ?? 2000000;
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

function addDestination(id, label, direction) {
  destinations.set(id, direction);
  if (![...$('destination').options].some((option) => option.value === id)) $('destination').add(new Option(label, id));
}

async function loadOverlays(worldId) {
  const results = await Promise.allSettled([
    api(`/api/worlds/${worldId}/rivers`).then((data) => {
      if (activeWorld?.id !== worldId) return;
      riverData = JSON.stringify(data); viewer.set_rivers(riverData);
      const river = data.segments?.filter((s) => s.start_height_m > 80 && s.end_height_m > 20 && s.end_height_m < s.start_height_m)
        .sort((a, b) => b.discharge_m3_s - a.discharge_m3_s)[0];
      if (river) addDestination('river', 'Grande rivière', river.start);
      const valley = data.segments?.filter((s) => s.discharge_m3_s > 1000 && s.end_height_m > 20 && s.start_height_m > s.end_height_m)
        .sort((a, b) => (b.start_height_m - b.end_height_m) - (a.start_height_m - a.end_height_m))[0];
      if (valley) addDestination('valley', 'Vallée fluviale', valley.start);
    }),
    api(`/api/worlds/${worldId}/lakes`).then((data) => {
      if (activeWorld?.id !== worldId) return;
      const lake = data.lakes?.filter((l) => l.volume_m3 > 0).sort((a, b) => b.area_m2 - a.area_m2)[0];
      if (lake) addDestination('lake', 'Grand lac', lake.direction);
    }),
    api(`/api/worlds/${worldId}/human`).then((data) => {
      if (activeWorld?.id !== worldId) return;
      humanData = JSON.stringify(data);
      viewer.set_human(humanData);
      if (data.settlements?.length) addDestination('village', 'Première localité', data.settlements[0].direction);
      addMetric('Population implantée', number(data.population ?? 0));
      addMetric('Localités', number(data.settlements?.length ?? 0));
      addMetric('Routes', number(data.roads?.length ?? 0));
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
    if (humanData) viewer.set_human(humanData);
    seasonPhase = month === 0 ? undefined : (month - 1) / 12;
    renderEpoch += 1;
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
  const config = { seed: Number($('seed').value), resolution: Number($('resolution').value), radius_m: Number($('radius').value) * 1000, ocean_fraction: Number($('ocean-fraction').value) / 100, relief_m: Number($('relief').value), gravity_m_s2: Number($('gravity').value), axial_tilt_deg: Number($('tilt').value), erosion_steps: Number($('erosion-steps').value), geological_years: Number($('geological-years').value) };
  try {
    $('status').textContent = 'Compilation native : relief, climat et hydrologie…';
    const result = await api('/api/worlds', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(config) });
    if (result.cells) {
      loadWorld(result);
    } else {
      const id = result.job_id ?? result.id;
      for (;;) {
        await new Promise((resolve) => setTimeout(resolve, 600));
        const job = await api(`/api/jobs/${id}`);
        $('status').textContent = job.message ?? (job.status === 'queued' ? 'Génération dans la file…' : 'Compilation native du monde…');
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
  const priority = (tile) => tile.level === info.level ? 0 : tile.level < info.level ? 1 : 2;
  const memoryPressure = info.cpu_bytes >= (info.cpu_budget_bytes ?? Infinity) * 0.8;
  const requests = info.level < 2 ? [] : [...(info.requests ?? [info])]
    .filter((tile) => !memoryPressure || tile.level === info.level || loadedTiles.has(keyOf(tile)))
    .sort((a, b) => priority(a) - priority(b));
  tileStream.update(requests, {
    epoch: renderEpoch, keyOf, radius: activeWorld.config.radius_m,
    exaggeration: Number($('exaggeration').value), format: conservative ? 'json' : 'postcard',
    urlOf: (tile) => new URL(`/api/worlds/${worldId}/tiles/${tile.face}/${tile.level}/${tile.x}/${tile.y}?segments=128&format=${conservative ? 'json' : 'postcard'}${phase === undefined ? '' : `&phase=${phase}`}${conservative ? '&generator=contracts' : ''}`, location.origin).href,
  });
  if (info.level >= 2) {
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
$('randomize').addEventListener('click', () => { $('seed').value = crypto.getRandomValues(new Uint32Array(1))[0]; });
$('ocean-fraction').addEventListener('input', () => { $('ocean-value').value = `${$('ocean-fraction').value} %`; });
$('reset').addEventListener('click', () => {
  viewer.reset_view();
  $('view-tilt').value = '0';
  $('view-tilt-value').value = '0°';
});
$('fly').addEventListener('click', () => {
  const direction = destinations.get($('destination').value);
  if (!direction) return;
  viewer.fly_to(JSON.stringify(direction), 30000);
  $('status').textContent = 'Arrivée à 30 km au-dessus du lieu · molette pour descendre';
});
$('layer').addEventListener('change', () => viewer.set_mode(Number($('layer').value)));
$('season').addEventListener('input', () => { $('season-value').value = Number($('season').value) === 0 ? 'Moyenne annuelle' : `Saison ${$('season').value} / 12`; });
$('season').addEventListener('change', changeSeason);
for (const id of ['show-rivers', 'show-human']) $(id).addEventListener('change', () => viewer.set_overlays($('show-rivers').checked, $('show-human').checked));
$('conservative-refinement').addEventListener('change', () => {
  renderEpoch += 1;
  viewer.clear_tiles();
  loadedTiles.clear();
  $('contract-status').hidden = !$('conservative-refinement').checked;
  $('contract-status').textContent = 'Conservation des altitudes uniquement. Descendre pour vérifier le raffinement.';
});
$('exaggeration').addEventListener('input', () => { $('exaggeration-value').value = `×${$('exaggeration').value}`; });
$('exaggeration').addEventListener('change', () => { viewer.set_exaggeration(Number($('exaggeration').value)); viewer.clear_tiles(); loadedTiles.clear(); renderEpoch += 1; lastTile = undefined; updateDetail(); });
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
  setInterval(updateDetail, 100);
  try { loadWorld(await api('/api/world/latest/preview?resolution=128')); } catch { await generate(); }
} catch (error) {
  $('backend').textContent = 'Moteur indisponible';
  $('status').textContent = 'Initialisation impossible';
  message(`Le moteur Rust/WebAssembly ne démarre pas : ${error}. Lancez scripts/build-web.sh, puis utilisez un navigateur récent avec WebGPU ou WebGL2 sur localhost.`);
}
