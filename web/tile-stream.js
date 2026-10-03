// Fetch/decode/mesh preparation is off the UI thread. Only one prepared upload
// is installed per frame, and obsolete camera requests cannot stall new ones.
export class TileStream {
  constructor(worker, install, onCache) {
    this.worker = worker;
    this.install = install;
    this.onCache = onCache;
    this.loaded = new Set();
    this.jobs = new Map();
    this.ready = [];
    this.desired = new Map();
    this.failures = new Map();
    this.sequence = 0;
    this.epoch = undefined;
    this.metrics = { installed: 0, stale: 0, max_install_ms: 0, ready_bytes: 0 };
    worker.onmessage = ({ data }) => this.receive(data);
    worker.onerror = (error) => { console.error('Worker de terrain', error.message); };
  }

  reset(epoch) {
    for (const [id] of this.jobs) this.worker.postMessage({ type: 'cancel', id });
    this.jobs.clear();
    this.loaded.clear(); this.ready.length = 0; this.failures.clear();
    this.desired.clear(); this.context = undefined;
    this.metrics.ready_bytes = 0; this.epoch = epoch;
  }

  update(requests, context) {
    if (this.epoch !== context.epoch) this.reset(context.epoch);
    if (context.residentKeys) this.loaded = new Set(context.residentKeys.map(context.keyOf));
    this.context = context;
    this.desired = new Map(requests.map((tile) => [context.keyOf(tile), tile]));
    this.priority = new Map([...this.desired.keys()].map((key, index) => [key, index]));
    for (const [id, job] of this.jobs) {
      if (job.epoch !== this.epoch || !this.desired.has(job.key) || this.loaded.has(job.key)) {
        this.worker.postMessage({ type: 'cancel', id });
        // Unique job IDs let late responses be rejected without retaining a
        // cancelled job in one of the three slots needed by the new camera.
        this.jobs.delete(id);
      }
    }
    this.ready = this.ready.filter((tile) => tile.epoch === this.epoch && this.desired.has(tile.key) && !this.loaded.has(tile.key));
    this.metrics.ready_bytes = this.ready.reduce((n, tile) => n + tile.packet.byteLength, 0);
    this.prioritizeReady();
    this.dispatch();
  }

  prioritizeReady() {
    this.ready.sort((a, b) => (this.priority?.get(a.key) ?? Infinity) - (this.priority?.get(b.key) ?? Infinity));
  }

  dispatch() {
    if (!this.context) return;
    const queued = new Set([...this.jobs.values()].filter((job) => !job.cancelled && job.epoch === this.epoch).map((job) => job.key));
    for (const tile of this.ready) queued.add(tile.key);
    for (const [key, tile] of this.desired) {
      if (this.jobs.size >= 3 || this.ready.length + this.jobs.size >= 6 || this.metrics.ready_bytes >= 48 * 1024 * 1024) break;
      if (this.loaded.has(key) || queued.has(key) || (this.failures.get(key) ?? 0) > Date.now()) continue;
      const id = ++this.sequence;
      const job = { key, epoch: this.epoch, cancelled: false };
      this.jobs.set(id, job); queued.add(key);
      this.worker.postMessage({ type: 'load', id, key, epoch: this.epoch,
        url: this.context.urlOf(tile), radius: this.context.radius, exaggeration: this.context.exaggeration,
        format: this.context.format });
    }
  }

  receive(data) {
    const job = this.jobs.get(data.id);
    this.jobs.delete(data.id);
    if (!job || job.cancelled || data.epoch !== this.epoch || !this.desired.has(job.key) || this.loaded.has(job.key)) {
      this.metrics.stale += 1;
    } else if (data.type === 'tile') {
      this.ready.push(data); this.metrics.ready_bytes += data.packet.byteLength;
      this.prioritizeReady();
      Object.assign(this.metrics, data.metrics, { last_fetch_ms: data.fetch_ms, last_prepare_ms: data.prepare_ms });
    } else if (!data.aborted) {
      this.failures.set(job.key, Date.now() + 5000);
      console.warn('Tuile indisponible', data.error);
    }
    this.dispatch();
  }

  installNext() {
    const tile = this.ready.shift();
    if (!tile) return;
    this.metrics.ready_bytes -= tile.packet.byteLength;
    if (tile.epoch !== this.epoch || !this.desired.has(tile.key) || this.loaded.has(tile.key)) { this.dispatch(); return; }
    const started = performance.now();
    try {
      const keys = JSON.parse(this.install(new Uint8Array(tile.packet)));
      this.loaded = new Set(keys.map(this.context.keyOf));
      this.onCache(keys);
      this.metrics.installed += 1;
      this.metrics.last_install_ms = performance.now() - started;
      this.metrics.max_install_ms = Math.max(this.metrics.max_install_ms, this.metrics.last_install_ms);
    } catch (error) {
      this.failures.set(tile.key, Date.now() + 5000);
      console.warn('Installation de tuile', error);
    }
    this.dispatch();
  }
}
