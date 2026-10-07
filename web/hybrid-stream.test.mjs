import assert from 'node:assert/strict';
import test from 'node:test';
import { CHART_BASES, chartPoint, wantedPatches, HybridStream } from './hybrid-stream.js';
const flush = () => new Promise((resolve) => setImmediate(resolve));
const key = (tile) => `${tile.face}/${tile.x}/${tile.y}`;
const normalize = (point) => point.map((value) => value / Math.hypot(...point));
const direction = (face, x, y, radius) => {
  const [n, u, v] = CHART_BASES[face]; const a = x / (2 * radius); const b = y / (2 * radius);
  return n.map((value, i) => (value * (1 - a * a - b * b) + 2 * a * u[i] + 2 * b * v[i]) / (1 + a * a + b * b));
};
const context = { epoch: 1, radius: 1737000, aspect: 1, exaggeration: 1, format: 'postcard',
  keyOf: (tile) => `${tile.face}/${tile.level}/${tile.x}/${tile.y}`, urlOf: (tile) => `https://proc.invalid/${tile.face}/${tile.level}/${tile.x}/${tile.y}` };

function setup(fetcher, options = {}) {
  const events = []; const messages = []; const ordinary = [];
  const viewer = {
    clear_hybrid_patches: () => events.push('clear'), clear_hybrid_patch_registry: () => events.push('clear'), register_hybrid_patch: () => events.push('patch'),
    remove_hybrid_patch: () => {}, cancel_surface_update: () => events.push('cancel'),
    begin_surface_update: (version, field) => {
      events.push(['begin', version, field]);
      return JSON.stringify({ state: 'preparing', keys: [{ face: 4, level: 12, x: 100, y: 100 }] });
    }, stage_surface_tile: (packet, version) => events.push(['stage', version, packet.byteLength]),
  };
  const worker = { postMessage: (message) => messages.push(message) };
  const tileStream = { worker: { postMessage: (message) => ordinary.push(message) }, setSurface: (...args) => events.push(['set', ...args]) };
  const stream = new HybridStream({ viewer, tileStream, worker, fetcher, endpoint: 'https://nn.invalid', ...options });
  stream.reset({ id: 'world-a', config: { radius_m: 1737000 } });
  return { stream, viewer, worker, events, messages, ordinary };
}

function replyFingerprint() { return Response.json({ world_fingerprint: 'model-and-world' }, { status: 202 }); }

test('plain HTTP without randomUUID still registers a stable128bit cryptographic client identifier', async () => {
  const posts = []; let randomCalls = 0;
  const cryptoProvider = { getRandomValues: (bytes) => { randomCalls += 1; bytes.set(Array.from({ length: 16 }, (_, i) => i * 17)); return bytes; } };
  const { stream } = setup(async (url, options) => {
    assert.equal(new URL(url).protocol, 'http:');
    if (options?.method === 'POST') posts.push(JSON.parse(options.body));
    return replyFingerprint();
  }, { cryptoProvider, endpoint: 'http://192.0.2.20:8767' });
  stream.setMode('hybrid');
  stream.update({ direction: [0, 0, 1], altitude_m: 2000000, surface_version: 0 }, context);
  await flush();
  assert.equal(stream.clientId, '00112233445566778899aabbccddeeff');
  assert.match(posts[0].client, /^[0-9a-f]{32}$/);
  stream.registeredView = -1; stream.publishView(); await flush();
  assert.ok(posts.every((post) => post.client === stream.clientId));
  assert.equal(randomCalls, 1);
});

test('chart bases match Rust and neither pole has a longitude singularity', () => {
  assert.deepEqual(CHART_BASES[2], [[0, 1, 0], [1, 0, 0], [0, 0, -1]]);
  assert.deepEqual(CHART_BASES[3], [[0, -1, 0], [1, 0, 0], [0, 0, 1]]);
  for (const [point, face] of [[[0, 1, 0], 2], [[0, -1, 0], 3]]) {
    const chart = chartPoint(point, face, 1737000);
    assert.equal(chart.x, 0); assert.equal(chart.y, 0); assert.equal(chart.scale, 1);
    assert.ok(wantedPatches(point, 1737000, 5).every((patch) => patch.face === face));
  }
});

test('20 cm inspection keeps the connected coverage feather across patch borders', () => {
  const point = direction(4, 7500, 3840, 1737000);
  const desired = wantedPatches(point, 1737000, 0.2, 1);
  assert.ok(desired.some((patch) => key(patch) === '4/0/0'));
  assert.ok(desired.some((patch) => key(patch) === '4/1/0'));
  const corner = wantedPatches(normalize([1, 1, 1]), 1737000, 5000, 2);
  assert.ok(corner.length <= 16);
  assert.deepEqual(new Set(corner.filter((patch) => patch.priority === -20).map((patch) => patch.face)), new Set([0, 2, 4]));
  assert.ok(corner.every((patch) => patch.priority >= -20 && patch.priority <= 20));
  assert.equal(wantedPatches([0, 0, 1], 1737000, 2000000).length, 0);
  assert.ok(wantedPatches([0, 0, 1], 1737000, 100000, 1, 16, 5000).length > 0);
});

test('procedural startup performs no request to the neural bridge', async () => {
  const calls = [];
  const { stream } = setup(async (...args) => { calls.push(args); return replyFingerprint(); });
  stream.update({ direction: [0, 0, 1], altitude_m: 5, surface_version: 0 }, context);
  await flush();
  assert.equal(calls.length, 0); assert.equal(stream.snapshot().errors, 0);
});

test('registration coalesces to the latest viewport before starting tile polls', async () => {
  const posts = []; const gets = []; const resolvers = [];
  const fetcher = (url, options) => {
    if (options?.method === 'POST') {
      posts.push(JSON.parse(options.body)); return new Promise((resolve) => resolvers.push(resolve));
    }
    gets.push(String(url)); return new Promise(() => {});
  };
  const { stream } = setup(fetcher);
  stream.setMode('hybrid');
  stream.update({ direction: [0, 0, 1], altitude_m: 1000, surface_version: 0 }, context);
  stream.update({ direction: [1, 0, 0], altitude_m: 1000, surface_version: 0 }, context);
  assert.equal(posts.length, 1); assert.equal(gets.length, 0);
  resolvers.shift()(replyFingerprint()); await flush();
  assert.equal(posts.length, 2); assert.equal(gets.length, 0);
  assert.ok(posts[1].version > posts[0].version);
  assert.ok(posts[1].tiles.every((tile) => tile.face === 0));
  resolvers.shift()(replyFingerprint()); await flush();
  assert.equal(gets.length, 2);
  for (const url of gets) {
    assert.equal(new URL(url).searchParams.get('client'), stream.clientId);
    assert.equal(Number(new URL(url).searchParams.get('view')), stream.viewVersion);
  }
  stream.setMode('procedural'); await flush();
  assert.deepEqual(posts.at(-1).tiles, []);
  resolvers.shift()(replyFingerprint()); await flush();
});

test('field packets remain in a staged transaction until the renderer commits its version', async () => {
  const { stream, worker, events, messages } = setup(async () => replyFingerprint());
  await flush();
  stream.setMode('hybrid');
  stream.dirtySince = -1000;
  stream.update({ direction: [0, 0, 1], altitude_m: 2000000, surface_version: 0 }, context);
  const version = stream.transaction.version;
  const begin = events.find((event) => Array.isArray(event) && event[0] === 'begin');
  assert.equal(JSON.parse(begin[2]).world_id, 'world-a');
  assert.equal(JSON.parse(begin[2]).patches, undefined); // Typed registry, never big value JSON.
  worker.onmessage({ data: { type: 'surface-ready', version } });
  const job = messages.find((message) => message.type === 'surface-load');
  worker.onmessage({ data: { type: 'surface-tile', id: job.id, surfaceVersion: version,
    packet: new Uint8Array(8).buffer } });
  assert.equal(events.filter((event) => Array.isArray(event) && event[0] === 'stage').length, 0);
  stream.installNext();
  assert.deepEqual(events.find((event) => Array.isArray(event) && event[0] === 'stage'), ['stage', version, 8]);
  assert.equal(stream.appliedVersion, 0);
  stream.update({ direction: [0, 0, 1], altitude_m: 2000000, surface_version: version }, context);
  assert.equal(stream.appliedVersion, version); assert.equal(stream.transaction, undefined);
  await flush();
});

test('an empty committed hybrid field is reported as procedural, not NN coverage', () => {
  const { stream } = setup(async () => replyFingerprint());
  stream.mode = 'hybrid';
  stream.transaction = { version: 1, field: '{"revision":1}', patchKeys: new Set() };
  stream.commitTransaction();
  const state = stream.snapshot();
  assert.equal(state.applied_version, 1);
  assert.equal(state.applied_mode, 'procedural');
  assert.equal(state.applied_patches, 0);
  assert.equal(state.applied_wanted, 0);
});

test('applied coverage uses the admitted immutable field rather than newly cached patches', () => {
  const { stream } = setup(async () => replyFingerprint());
  stream.mode = 'hybrid'; stream.context = context;
  stream.targetVersion = 1; stream.dirtySince = -1000;
  stream.patches.set('4/0/0', {}); stream.wanted.set('4/0/0', {});
  stream.maybeBegin();
  stream.patches.delete('4/0/0'); stream.patches.set('4/1/0', {});
  stream.wanted.set('4/1/0', {});
  stream.commitTransaction();
  const state = stream.snapshot();
  assert.equal(state.applied_mode, 'hybrid');
  assert.equal(state.ready_wanted, 1);
  assert.equal(state.applied_wanted, 1);
  assert.deepEqual([...stream.appliedPatchKeys], ['4/0/0']);
  stream.wanted.delete('4/0/0');
  assert.equal(stream.snapshot().applied_wanted, 0);
});

test('a hidden renderer does not expire prepared neural work before the next active frame', () => {
  const { stream } = setup(async () => replyFingerprint());
  stream.mode = 'procedural'; stream.targetVersion = 1;
  stream.transaction = { version: 1, remaining: [], started: performance.now()-30000, startedActiveMs: 0 };
  stream.update({ surface_version: 0 }, { ...context, activeMs: 50 });
  assert.ok(stream.transaction); assert.equal(stream.metrics.errors, 0);
  stream.update({ surface_version: 0 }, { ...context, activeMs: 15001 });
  assert.equal(stream.transaction, undefined); assert.equal(stream.metrics.errors, 1);
});

test('field worker failure cancels the transaction and restores the last applied procedural field', async () => {
  const { stream, worker, events, messages } = setup(async () => replyFingerprint());
  await flush(); stream.setMode('hybrid'); stream.dirtySince = -1000;
  stream.update({ direction: [0, 0, 1], altitude_m: 2000000, surface_version: 0 }, context);
  const version = stream.transaction.version;
  worker.onmessage({ data: { type: 'surface-ready', version } });
  const job = messages.find((message) => message.type === 'surface-load');
  worker.onmessage({ data: { type: 'error', id: job.id, surfaceVersion: version, error: 'network failed' } });
  assert.equal(stream.transaction, undefined); assert.equal(stream.snapshot().errors, 1);
  assert.ok(events.includes('cancel'));
  assert.deepEqual(events.filter((event) => Array.isArray(event) && event[0] === 'set').at(-1).slice(0, 3), ['set', 0, '']);
  await flush();
});

test('a changed model/world fingerprint clears every typed patch cache before reuse', async () => {
  let fingerprint = 'original-model';
  const { stream, events, messages } = setup(async () => Response.json({ world_fingerprint: fingerprint }, { status: 202 }));
  stream.setMode('hybrid');
  stream.update({ direction: [0, 0, 1], altitude_m: 2000000, surface_version: 0 }, context);
  await flush();
  stream.patches.set('4/0/0', { meta: '{}', values: new Float32Array(258 * 258) });
  const version = stream.targetVersion;
  fingerprint = 'new-model'; stream.registeredView = -1; stream.publishView();
  await flush();
  assert.equal(stream.patches.size, 0); assert.equal(stream.worldFingerprint, fingerprint);
  assert.ok(stream.targetVersion > version);
  assert.equal(events.filter((event) => event === 'clear').length, 2);
  assert.equal(messages.filter((message) => message.type === 'clear-patches').length, 2);
});


test('reset clears both worker registries before the next surface context', () => {
  const { stream, messages, ordinary } = setup(async () => replyFingerprint());
  stream.reset({ id: 'world-b', config: { radius_m: 6371000 } });
  assert.equal(ordinary.filter((message) => message.type === 'clear-patches').length, 2);
  assert.equal(messages.filter((message) => message.type === 'clear-patches').length, 2);
  assert.equal(messages.at(-1).type, 'set-surface');
});

test('cancelled field revisions restore worker snapshots instead of reparsing mutable registry', () => {
  const { stream, messages, events } = setup(async () => replyFingerprint());
  stream.appliedVersion = 7;
  stream.appliedField = JSON.stringify({ world_id: 'world-a', revision: 7, radius_m: 1737000 });
  stream.transaction = { version: 8 };
  stream.cancelTransaction();
  assert.equal(messages.at(-1).snapshot, true);
  const selected = events.filter((event) => Array.isArray(event) && event[0] === 'set').at(-1);
  assert.deepEqual(selected[4], { snapshot: true });
});

test('unrecoverable surface worker errors release staging and disable repeated preparation', () => {
  const { stream, worker, events } = setup(async () => replyFingerprint());
  stream.transaction = { version: 2 };
  stream.surfaceJobs.set(3, {});
  worker.onerror({ message: 'worker crashed' });
  assert.equal(stream.transaction, undefined);
  assert.equal(stream.surfaceJobs.size, 0);
  assert.ok(events.includes('cancel'));
  assert.equal(stream.snapshot().preparation_disabled, true);
});

test('memory admission waits without creating a broken transaction', () => {
  const { stream, viewer } = setup(async () => replyFingerprint());
  viewer.begin_surface_update = () => JSON.stringify({ state: 'memory' });
  stream.context = context; stream.targetVersion = 1; stream.dirtySince = -1000;
  stream.maybeBegin();
  assert.equal(stream.transaction, undefined);
  assert.equal(stream.snapshot().surface_wait_state, 'memory');
  assert.equal(stream.snapshot().errors, 0);
});

test('bounded staging drains several packets in one frame instead of one per frame', () => {
  const { stream, events } = setup(async () => replyFingerprint());
  stream.transaction = { version: 1 };
  stream.surfaceReady = Array.from({ length: 16 }, () => ({ surfaceVersion: 1, packet: new ArrayBuffer(4) }));
  stream.installNext();
  const staged = events.filter((event) => Array.isArray(event) && event[0] === 'stage').length;
  assert.ok(staged > 1 && staged <= 8);
  assert.equal(stream.surfaceReady.length, 16 - staged);
});


test('a user cancellation between GPU commit and status polling keeps the newly committed snapshot', () => {
  const { stream, viewer, events, messages } = setup(async () => replyFingerprint());
  stream.appliedVersion = 1; stream.appliedField = 'old';
  stream.transaction = { version: 2, field: 'new' };
  viewer.view_info = () => JSON.stringify({ surface_version: 2 });
  stream.cancelTransaction();
  assert.equal(stream.appliedVersion, 2); assert.equal(stream.appliedField, 'new');
  assert.equal(events.includes('cancel'), false);
  assert.equal(messages.at(-1).snapshot, true); assert.equal(messages.at(-1).field, 'new');
});
