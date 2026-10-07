// The original procedural renderer never waits for neural inference. These
// fixed, metre-based conformal charts only supply an additive 30 m residual.
export const CHART_BASES = [
  [[1, 0, 0], [0, 0, -1], [0, 1, 0]],
  [[-1, 0, 0], [0, 0, 1], [0, 1, 0]],
  [[0, 1, 0], [1, 0, 0], [0, 0, -1]],
  [[0, -1, 0], [1, 0, 0], [0, 0, 1]],
  [[0, 0, 1], [1, 0, 0], [0, 1, 0]],
  [[0, 0, -1], [-1, 0, 0], [0, 1, 0]],
];
const dot = (a, b) => a.reduce((sum, value, index) => sum + value * b[index], 0);
const patchKey = (patch) => `${patch.face}/${patch.x}/${patch.y}${patch.level ? `/L${patch.level}` : ''}`;
const SOURCE_MPP = 30;
const CORE = 256;
const MAX_PATCHES = 48;

export function createClientId(cryptoProvider = globalThis.crypto) {
  if (typeof cryptoProvider?.randomUUID === 'function') return cryptoProvider.randomUUID().replaceAll('-', '');
  if (typeof cryptoProvider?.getRandomValues !== 'function') throw new Error('Génération cryptographique de l’identifiant client indisponible');
  const bytes = cryptoProvider.getRandomValues(new Uint8Array(16));
  return [...bytes].map((value) => value.toString(16).padStart(2, '0')).join('');
}

export function chartPoint(direction, face, radius) {
  const basis = CHART_BASES[face];
  const cosine = dot(direction, basis[0]);
  const scale = 2 / (1 + cosine);
  return { x: radius * scale * dot(direction, basis[1]),
    y: radius * scale * dot(direction, basis[2]), cosine, scale };
}

export function wantedPatches(direction, radius, altitude, aspect = 1, limit = 16, height = 820) {
  const metresPerPixel = altitude * 2 * Math.tan(Math.PI / 8) / Math.max(1, height);
  if (!direction?.every(Number.isFinite) || !Number.isFinite(metresPerPixel) || metresPerPixel > 1920) return [];
  const length = Math.hypot(...direction);
  if (!(length > 0)) return [];
  const point = direction.map((value) => value / length);
  const level = metresPerPixel <= 60 ? 0 : metresPerPixel <= 480 ? 3 : 6;
  const gather = (lod) => {
    const mpp = SOURCE_MPP * 2 ** lod;
    const reach = Math.max(mpp, altitude * Math.tan(Math.PI / 8) * Math.hypot(1, aspect) * 1.35);
    const candidates = [];
    for (let face = 0; face < 6; face += 1) {
      const chart = chartPoint(point, face, radius);
      if (chart.cosine <= 0.55) continue;
      const spacing = CORE * mpp;
      const extent = reach * chart.scale + 25 * mpp;
      const cx = Math.floor(chart.x / spacing); const cy = Math.floor(chart.y / spacing);
      const x0 = Math.max(cx - 3, Math.floor((chart.x - extent) / spacing));
      const x1 = Math.min(cx + 3, Math.floor((chart.x + extent) / spacing));
      const y0 = Math.max(cy - 3, Math.floor((chart.y - extent) / spacing));
      const y1 = Math.min(cy + 3, Math.floor((chart.y + extent) / spacing));
      for (let y = y0; y <= y1; y += 1) for (let x = x0; x <= x1; x += 1) {
        const distance = Math.hypot((x + 0.5) * spacing - chart.x, (y + 0.5) * spacing - chart.y);
        const center = x === cx && y === cy;
        candidates.push({ face, level: lod, x, y,
          priority: center ? (lod === 3 && level === 0 ? -20 : -19) : Math.min(20, Math.floor(distance / spacing)),
          score: (center ? -100 : 0) + distance / spacing - (chart.cosine - 0.55) });
      }
    }
    return candidates.sort((a, b) => a.score - b.score || a.face - b.face || a.y - b.y || a.x - b.x);
  };
  // A small intermediate field appears before native decoding. Native and
  // preview patches share one pipeline context, so this work is reused.
  const preview = level === 0 ? gather(3).filter((patch) => patch.priority === -20) : [];
  return [...preview, ...gather(level)].slice(0, limit);
}

export class HybridStream {
  constructor({ viewer, tileStream, worker, fetcher = globalThis.fetch.bind(globalThis), endpoint, onStatus = () => {}, cryptoProvider = globalThis.crypto }) {
    this.viewer = viewer; this.tileStream = tileStream; this.worker = worker;
    this.fetcher = fetcher; this.endpoint = endpoint; this.onStatus = onStatus;
    this.clientId = createClientId(cryptoProvider);
    this.viewVersion = 0; this.registeredView = -1; this.viewSignature = '';
    this.registration = undefined; this.worldFingerprint = undefined; this.hybridSessionSent = false;
    this.mode = 'procedural'; this.patches = new Map(); this.pending = new Map(); this.wanted = new Map();
    this.failures = new Map(); this.sequence = 0; this.targetVersion = 0; this.appliedVersion = 0;
    this.appliedField = ''; this.appliedPatchKeys = new Set(); this.dirtySince = 0; this.surfaceJobs = new Map(); this.surfaceReady = [];
    this.metrics = { errors: 0, completed: 0, stale: 0, last_generation_ms: 0 };
    worker.onmessage = ({ data }) => this.receive(data);
    worker.onerror = (error) => {
      this.failPreparation(error.message || 'Worker de surface interrompu', true);
    };
    tileStream.onSurfaceError = (error) => this.failPreparation(error);
  }

  reset(world) {
    this.cancelTransaction();
    for (const job of this.pending.values()) job.controller.abort();
    this.pending.clear(); this.wanted.clear(); this.patches.clear(); this.failures.clear();
    this.world = world; this.worldFingerprint = undefined; this.publishView(); this.targetVersion = 0; this.appliedVersion = 0; this.appliedField = ''; this.dirtySince = 0;
    this.appliedPatchKeys.clear();
    this.viewer.clear_hybrid_patches();
    const json = JSON.stringify(world);
    this.tileStream.worker.postMessage({ type: 'clear-patches' });
    this.tileStream.setSurface(0, '', json);
    this.worker.postMessage({ type: 'clear-patches' });
    this.worker.postMessage({ type: 'set-surface', version: 0, field: '', world: json });
    if (this.mode === 'hybrid') this.markDirty();
  }

  setMode(mode) {
    if (!['procedural', 'hybrid'].includes(mode) || mode === this.mode) return;
    this.mode = mode; this.cancelTransaction();
    if (mode === 'procedural') {
      for (const job of this.pending.values()) job.controller.abort();
      this.pending.clear(); this.wanted.clear(); this.publishView();
    }
    this.markDirty();
  }

  markDirty() { this.targetVersion += 1; this.dirtySince = performance.now(); }

  contextField(version) {
    return this.mode === 'hybrid' ? JSON.stringify({ world_id: String(this.world.id),
      revision: version, radius_m: this.world.config.radius_m, kind: 'height' }) : '';
  }

  update(view, context) {
    if (!this.world) return;
    this.view = view; this.context = context;
    if (this.mode === 'hybrid') {
      const desired = wantedPatches(view.direction, this.world.config.radius_m, view.altitude_m, context.aspect, 16, context.height);
      this.wanted = new Map(desired.map((patch) => [patchKey(patch), patch]));
      for (const [key, job] of this.pending) if (!this.wanted.has(key)) {
        job.controller.abort(); this.pending.delete(key);
      }
      for (const key of this.wanted.keys()) if (this.patches.has(key)) {
        const patch = this.patches.get(key); this.patches.delete(key); this.patches.set(key, patch);
      }
      this.publishView(); this.dispatchPatches();
    }
    if (this.transaction && view.surface_version === this.transaction.version) {
      this.commitTransaction();
    }
    const elapsed = this.transaction && Number.isFinite(context.activeMs)
      ? context.activeMs - this.transaction.startedActiveMs
      : this.transaction && performance.now() - this.transaction.started;
    if (this.transaction && elapsed > 15000) {
      this.failPreparation('La préparation de surface a dépassé 15 secondes');
    }
    this.maybeBegin(); this.dispatchSurface();
  }

  publishView() {
    if (!this.world || (this.mode === 'procedural' && !this.hybridSessionSent)) return;
    const tiles = this.mode === 'hybrid' ? [...this.wanted.values()].map(({ face, level, x, y, priority }) => ({ face, level, x, y, priority })) : [];
    const signature = JSON.stringify([String(this.world.id), tiles]);
    if (signature !== this.viewSignature) {
      this.viewSignature = signature; this.viewVersion += 1;
      this.registrationRequest = { client: this.clientId, version: this.viewVersion,
        world_id: String(this.world.id), tiles };
    }
    if (!this.registration && this.registeredView !== this.viewVersion && this.registrationRequest) {
      const request = this.registrationRequest;
      if (this.mode === 'hybrid') this.hybridSessionSent = true;
      this.registration = this.fetcher(new URL('/api/planet/view', this.endpoint), {
        method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(request),
      }).then(async (response) => {
        if (!response.ok) throw new Error(`Registre NN ${response.status}`);
        const data = await response.json();
        if (request.version === this.viewVersion && request.world_id === String(this.world?.id)) {
          if (this.worldFingerprint && this.worldFingerprint !== data.world_fingerprint) {
            this.cancelTransaction();
            for (const job of this.pending.values()) job.controller.abort();
            this.pending.clear(); this.patches.clear(); this.failures.clear();
            this.viewer.clear_hybrid_patch_registry();
            for (const worker of [this.worker, this.tileStream.worker]) worker.postMessage({ type: 'clear-patches', preserveSnapshots: true });
            this.markDirty();
          }
          this.registeredView = request.version; this.worldFingerprint = data.world_fingerprint;
        }
      }).catch((error) => {
        this.metrics.errors += 1; this.metrics.last_error = String(error);
      }).finally(() => {
        this.registration = undefined;
        if (this.registeredView === this.viewVersion) this.dispatchPatches();
        else if (request.version !== this.viewVersion) queueMicrotask(() => this.publishView());
        else setTimeout(() => this.publishView(), 300);
      });
    }
  }

  dispatchPatches() {
    if (this.mode !== 'hybrid' || this.registeredView !== this.viewVersion) return;
    for (const [key, patch] of this.wanted) {
      if (this.pending.size >= 2) break;
      if (this.patches.has(key) || this.pending.has(key) || (this.failures.get(key) ?? 0) > Date.now()) continue;
      const controller = new AbortController(); const worldId = String(this.world.id);
      const job = { controller, worldId };
      this.pending.set(key, job);
      const url = new URL('/api/planet/tile', this.endpoint);
      for (const [name, value] of Object.entries({ world_id: worldId, face: patch.face,
        level: patch.level ?? 0, x: patch.x, y: patch.y, priority: patch.priority })) url.searchParams.set(name, String(value));
      this.fetchPatch(url, controller.signal).then(async (response) => {
        const buffer = await response.arrayBuffer();
        if (controller.signal.aborted || String(this.world?.id) !== worldId || this.pending.get(key) !== job) return;
        if (response.headers.get('X-World-Id') !== worldId
          || !this.worldFingerprint || response.headers.get('X-World-Fingerprint') !== this.worldFingerprint
          || response.headers.get('X-Projection') !== 'stereo-six-v1'
          || Number(response.headers.get('X-Tile-Size')) !== 258
          || Number(response.headers.get('X-Core-Size')) !== 256
          || Number(response.headers.get('X-Halo')) !== 1
          || Number(response.headers.get('X-Chart-Mpp')) !== SOURCE_MPP * 2 ** (patch.level ?? 0)
          || Number(response.headers.get('X-Tile-Level')) !== (patch.level ?? 0)
          || response.headers.get('X-Surface-Kind') !== 'height'
          || Number(response.headers.get('X-Height-Limit-M')) !== 20000) throw new Error('Contrat de patch NN incompatible');
        if (buffer.byteLength !== 258 * 258 * 4) throw new Error('Dimensions du patch NN invalides');
        const values = new Float32Array(buffer);
        if (values.some((value) => !Number.isFinite(value) || Math.abs(value) > 20000)) throw new Error('Altitude NN non finie ou hors limite');
        const meta = JSON.stringify({ world_id: worldId, face: patch.face, level: patch.level ?? 0, x: patch.x, y: patch.y });
        this.viewer.register_hybrid_patch(meta, values);
        this.patches.set(key, { ...patch, meta, values });
        // Copy/transfer typed data; there is no large JSON stringify on the UI.
        for (const worker of [this.worker, this.tileStream.worker]) {
          const copy = values.slice().buffer;
          worker.postMessage({ type: 'hybrid-patch', meta, values: copy }, [copy]);
        }
        this.metrics.last_error = undefined;
        this.metrics.completed += 1;
        this.metrics.last_generation_ms = Number(response.headers.get('X-Generation-Ms')) || 0;
        this.markDirty(); this.prunePatches();
      }).catch((error) => {
        if (!controller.signal.aborted) {
          this.metrics.errors += 1; this.metrics.last_error = String(error);
          this.failures.set(key, Date.now() + 2000);
        }
      }).finally(() => {
        if (this.pending.get(key) === job) this.pending.delete(key);
        this.dispatchPatches();
      });
    }
  }

  async fetchPatch(url, signal) {
    for (;;) {
      signal.throwIfAborted();
      url.searchParams.set('client', this.clientId); url.searchParams.set('view', String(this.viewVersion));
      const response = await this.fetcher(url, { signal });
      if (response.status !== 202) {
        if (!response.ok) throw new Error(`NN ${response.status}: ${(await response.text()).slice(0, 160)}`);
        return response;
      }
      const body = await response.json().catch(() => ({}));
      if (body.state === 'stale') { this.registeredView = -1; this.publishView(); }
      const delay = Math.max(100, Math.min(1000, (Number(response.headers.get('Retry-After')) || 0.15) * 1000));
      await new Promise((resolve, reject) => {
        const timer = setTimeout(() => { signal.removeEventListener('abort', abort); resolve(); }, delay);
        const abort = () => { clearTimeout(timer); reject(new DOMException('Aborted', 'AbortError')); };
        signal.addEventListener('abort', abort, { once: true });
      });
    }
  }

  prunePatches() {
    while (this.patches.size > MAX_PATCHES) {
      const victim = [...this.patches.keys()].find((key) => !this.wanted.has(key));
      if (!victim) break;
      const patch = this.patches.get(victim); this.patches.delete(victim);
      this.viewer.remove_hybrid_patch(patch.meta);
      for (const worker of [this.worker, this.tileStream.worker]) worker.postMessage({ type: 'remove-patch', meta: patch.meta });
    }
  }

  maybeBegin() {
    if (this.preparationDisabled || !this.context || this.transaction || this.targetVersion === this.appliedVersion
      || performance.now() - this.dirtySince < 250) return;
    const version = this.targetVersion; const field = this.contextField(version);
    try {
      const result = JSON.parse(this.viewer.begin_surface_update(version, field));
      this.metrics.surface_wait_state = result.state;
      for (const key of ['required_cpu_bytes','required_gpu_bytes','current_cpu_bytes','current_gpu_bytes','affected_tiles','scratch_cpu_bytes']) {
        this.metrics[key] = result[key];
      }
      if (result.state === 'busy' || result.state === 'memory') return;
      this.transaction = { version, field, patchKeys: field ? new Set(this.patches.keys()) : new Set(),
        keys: result.keys, remaining: [...result.keys], ready: false, started: performance.now(),
        startedActiveMs: this.context.activeMs ?? 0 };
      this.tileStream.setSurface(version, field, undefined, { paused: true });
      this.worker.postMessage({ type: 'set-surface', version, field });
    } catch (error) { this.metrics.errors += 1; this.metrics.last_error = String(error); this.dirtySince = performance.now() + 1500; }
  }

  dispatchSurface() {
    const transaction = this.transaction;
    if (!transaction?.ready || !this.context) return;
    while (transaction.remaining.length && this.surfaceJobs.size < 4
      && this.surfaceReady.length + this.surfaceJobs.size < 16) {
      const tile = transaction.remaining.shift(); const id = ++this.sequence;
      this.surfaceJobs.set(id, tile);
      this.worker.postMessage({ type: 'surface-load', id, key: this.context.keyOf(tile),
        epoch: this.context.epoch, surfaceVersion: transaction.version,
        rootFace: tile.level === 0 ? tile.face : undefined,
        url: this.context.urlOf(tile), radius: this.context.radius,
        exaggeration: this.context.exaggeration, format: this.context.format });
    }
  }

  receive(data) {
    if (data.type === 'surface-ready') {
      if (this.transaction?.version === data.version) { this.transaction.ready = true; this.dispatchSurface(); }
      return;
    }
    if (data.type === 'surface-error' || data.type === 'patch-error') {
      this.metrics.errors += 1; this.metrics.last_error = data.error;
      this.cancelTransaction(); this.dirtySince = performance.now() + 1000; return;
    }
    const tile = this.surfaceJobs.get(data.id); this.surfaceJobs.delete(data.id);
    if (!tile || !this.transaction || data.surfaceVersion !== this.transaction.version) {
      this.metrics.stale += 1; return;
    }
    if (data.type === 'surface-tile') this.surfaceReady.push(data);
    else if (!data.aborted) {
      this.metrics.errors += 1; this.metrics.last_error = data.error;
      this.cancelTransaction(); this.dirtySince = performance.now() + 1000;
    }
    this.dispatchSurface();
  }

  installNext() {
    const started = performance.now();
    let installed = 0;
    while (this.surfaceReady.length && installed < 8 && performance.now() - started < 2) {
      const tile = this.surfaceReady.shift();
      if (!this.transaction || tile.surfaceVersion !== this.transaction.version) continue;
      try { this.viewer.stage_surface_tile(new Uint8Array(tile.packet), tile.surfaceVersion); }
      catch (error) { this.failPreparation(String(error)); break; }
      installed += 1;
    }
    this.metrics.last_stage_count = installed;
    this.metrics.max_stage_count = Math.max(this.metrics.max_stage_count ?? 0, installed);
    this.metrics.last_stage_ms = performance.now() - started;
    this.dispatchSurface();
  }

  failPreparation(error, fatal = false) {
    this.metrics.errors += 1; this.metrics.last_error = String(error);
    this.cancelTransaction();
    this.dirtySince = performance.now() + 1000;
    if (fatal) { this.preparationDisabled = true; this.worker.terminate?.(); }
    this.onStatus(`Préparation NN : ${error}`);
  }

  commitTransaction() {
    this.appliedVersion = this.transaction.version; this.appliedField = this.transaction.field;
    this.appliedPatchKeys = this.transaction.patchKeys ?? new Set();
    this.transaction = undefined; this.surfaceJobs.clear(); this.surfaceReady.length = 0;
    this.tileStream.setSurface(this.appliedVersion, this.appliedField, undefined, { snapshot: true });
    this.worker.postMessage({ type: 'set-surface', version: this.appliedVersion, field: this.appliedField, snapshot: true });
  }

  cancelTransaction() {
    if (!this.transaction) return;
    // A click can arrive after the renderer committed, before the next 50 ms
    // status update. In that case B is already committed, so never restore A.
    if (this.viewer.view_info && JSON.parse(this.viewer.view_info()).surface_version === this.transaction.version) {
      this.commitTransaction(); return;
    }
    this.viewer.cancel_surface_update();
    for (const [id] of this.surfaceJobs) this.worker.postMessage({ type: 'cancel', id });
    this.surfaceJobs.clear(); this.surfaceReady.length = 0; this.transaction = undefined;
    this.tileStream.setSurface(this.appliedVersion, this.appliedField, undefined, { snapshot: true });
    this.worker.postMessage({ type: 'set-surface', version: this.appliedVersion, field: this.appliedField, snapshot: true });
  }

  snapshot() {
    return { ...this.metrics, mode: this.mode, applied_mode: this.appliedField && this.appliedPatchKeys.size ? 'hybrid' : 'procedural', source_mpp: this.wanted.size ? Math.min(...[...this.wanted.values()].map((p) => p.level <= 2 ? 30 : p.level <= 4 ? 240 : 7680)) : null,
      applied_patches: this.appliedPatchKeys.size, applied_wanted: [...this.wanted.keys()].filter((key) => this.appliedPatchKeys.has(key)).length,
      ready_patches: this.patches.size, ready_wanted: [...this.wanted.keys()].filter((key) => this.patches.has(key)).length, pending: this.pending.size, desired: this.wanted.size,
      version: this.targetVersion, applied_version: this.appliedVersion,
      client_id: this.clientId, view_version: this.viewVersion, registered_view: this.registeredView, world_fingerprint: this.worldFingerprint,
      preparation_disabled: Boolean(this.preparationDisabled), updating: Boolean(this.transaction), queued_surface: this.transaction?.remaining.length ?? 0,
      surface_jobs: this.surfaceJobs.size, ready_surface: this.surfaceReady.length };
  }
}
