import init, * as viewer from '/pkg/world_viewer.js';

// Geometry and binary decoding run here, never in the animation callback.
const ready = init();
ready.catch(() => {}); // Individual requests report initialization failure.
const requests = new Map();
let surfaceVersion = 0;
const memory = new Map();
let memoryBytes = 0;
const MEMORY_LIMIT = 128 * 1024 * 1024;
const DISK_LIMIT = 192 * 1024 * 1024;
const DISK_ENTRIES = 1024;
const surfaceWorker = new URL(self.location.href).searchParams.get('role') === 'surface';
const cacheName = surfaceWorker ? 'world-builder-surface-tiles-v2' : 'world-builder-tiles-v1';
let disk;
let diskIndex;
let diskBytes = 0;
let diskWrites = Promise.resolve();
let diskPendingWrites = 0;
const metrics = { memory_hits: 0, disk_hits: 0, network: 0, aborted: 0, prepared: 0 };

function remember(url, bytes) {
  const previous = memory.get(url);
  if (previous) { memory.delete(url); memoryBytes -= previous.byteLength; }
  if (bytes.byteLength > MEMORY_LIMIT) return;
  memory.set(url, bytes); memoryBytes += bytes.byteLength;
  while (memoryBytes > MEMORY_LIMIT) {
    const oldest = memory.keys().next().value;
    memoryBytes -= memory.get(oldest).byteLength; memory.delete(oldest);
  }
}

async function openDisk() {
  if (diskIndex) return disk;
  diskIndex = new Map();
  if (!self.caches) return;
  try {
    disk = await caches.open(cacheName);
    for (const request of await disk.keys()) {
      const response = await disk.match(request);
      const size = Number(response?.headers.get('x-wb-bytes'));
      if (size > 0) { diskIndex.set(request.url, size); diskBytes += size; }
      else await disk.delete(request);
    }
    while (diskBytes > DISK_LIMIT || diskIndex.size > DISK_ENTRIES) {
      const oldest = diskIndex.keys().next().value;
      await disk.delete(oldest); diskBytes -= diskIndex.get(oldest); diskIndex.delete(oldest);
    }
  } catch { disk = undefined; }
  return disk;
}
const diskReady = openDisk();

function saveDisk(url, bytes) {
  if (diskPendingWrites >= 4) return;
  diskPendingWrites += 1;
  // Serialize index changes and prune by bytes; failure never blocks exploration.
  diskWrites = diskWrites.then(async () => {
    await diskReady;
    if (!disk || bytes.byteLength > DISK_LIMIT) return;
    try {
      await disk.put(url, new Response(bytes, { headers: { 'x-wb-bytes': String(bytes.byteLength) } }));
      diskBytes -= diskIndex.get(url) ?? 0;
      diskIndex.delete(url); diskIndex.set(url, bytes.byteLength); diskBytes += bytes.byteLength;
      while (diskBytes > DISK_LIMIT || diskIndex.size > DISK_ENTRIES) {
        const oldest = diskIndex.keys().next().value;
        await disk.delete(oldest); diskBytes -= diskIndex.get(oldest); diskIndex.delete(oldest);
      }
    } catch { /* Storage quota/private mode: retain the bounded memory cache. */ }
  }).catch(() => {}).finally(() => { diskPendingWrites -= 1; });
}

async function rawTile(url, signal) {
  if (memory.has(url)) {
    const bytes = memory.get(url); remember(url, bytes); metrics.memory_hits += 1;
    return bytes;
  }
  await diskReady;
  if (disk) {
    try {
      const response = await disk.match(url);
      if (response) {
        const bytes = await response.arrayBuffer();
        const size = diskIndex.get(url); diskIndex.delete(url); diskIndex.set(url, size ?? bytes.byteLength);
        remember(url, bytes); metrics.disk_hits += 1; return bytes;
      }
    } catch { /* Continue through the network. */ }
  }
  signal.throwIfAborted();
  metrics.network += 1;
  const response = await fetch(url, { signal });
  if (!response.ok) throw new Error(`Tuile ${response.status}`);
  const bytes = await response.arrayBuffer();
  remember(url, bytes); saveDisk(url, bytes);
  return bytes;
}

self.onmessage = async ({ data }) => {
  if (data.type === 'cancel') {
    requests.get(data.id)?.abort(); return;
  }
  if (data.type === 'hybrid-patch' || data.type === 'remove-patch' || data.type === 'clear-patches') {
    try {
      await ready;
      if (data.type === 'hybrid-patch') viewer.register_hybrid_patch(data.meta, new Float32Array(data.values));
      else if (data.type === 'remove-patch') viewer.remove_hybrid_patch(data.meta);
      else if (data.preserveSnapshots) viewer.clear_hybrid_patch_registry();
      else viewer.clear_hybrid_patches();
    } catch (error) { self.postMessage({ type: 'patch-error', error: String(error) }); }
    return;
  }
  if (data.type === 'set-surface') {
    try {
      await ready;
      if (data.world) viewer.set_worker_world(data.world);
      if (data.snapshot) viewer.select_worker_hybrid_field(data.field ?? '');
      else viewer.set_worker_hybrid_field(data.field ?? '');
      surfaceVersion = data.version;
      self.postMessage({ type: 'surface-ready', version: data.version });
    } catch (error) {
      self.postMessage({ type: 'surface-error', version: data.version, error: String(error) });
    }
    return;
  }
  if (data.type !== 'load' && data.type !== 'surface-load') return;
  const controller = new AbortController(); requests.set(data.id, controller);
  const timeout = setTimeout(() => controller.abort(new Error('Préparation expirée')), 15000);
  const started = performance.now();
  try {
    await ready;
    const requestedVersion = data.surfaceVersion ?? 0;
    if (requestedVersion !== surfaceVersion) throw new Error('Champ de terrain périmé');
    const bytes = data.rootFace === undefined ? await rawTile(data.url, controller.signal) : undefined;
    controller.signal.throwIfAborted();
    const preparationStarted = performance.now();
    if (requestedVersion !== surfaceVersion) throw new Error('Champ de terrain périmé');
    const packet = data.rootFace !== undefined
      ? viewer.prepare_root_tile_hybrid(data.rootFace, data.radius, data.exaggeration)
      : data.format === 'json'
        ? viewer.prepare_tile_json_hybrid(new TextDecoder().decode(bytes), data.radius, data.exaggeration)
        : viewer.prepare_tile_binary_hybrid(new Uint8Array(bytes), data.radius, data.exaggeration);
    controller.signal.throwIfAborted();
    metrics.prepared += 1;
    const result = packet.buffer;
    self.postMessage({ type: data.type === 'surface-load' ? 'surface-tile' : 'tile', id: data.id, key: data.key, epoch: data.epoch, surfaceVersion: requestedVersion, packet: result,
      fetch_ms: preparationStarted - started, prepare_ms: performance.now() - preparationStarted,
      metrics: { ...metrics, memory_bytes: memoryBytes, disk_bytes: diskBytes,
        worker_memory_capacity_bytes: MEMORY_LIMIT, worker_disk_capacity_bytes: DISK_LIMIT,
        worker_disk_entry_capacity: DISK_ENTRIES, worker_disk_entries: diskIndex?.size ?? 0 } }, [result]);
  } catch (error) {
    if (controller.signal.aborted) metrics.aborted += 1;
    self.postMessage({ type: 'error', id: data.id, epoch: data.epoch, surfaceVersion: data.surfaceVersion ?? 0,
      aborted: controller.signal.aborted, error: String(error) });
  } finally { clearTimeout(timeout); requests.delete(data.id); }
};
