import assert from 'node:assert/strict';
import test from 'node:test';
import { startupDiagnostics, startupFailureMessage, startRenderer, attachNavigation, applyRendererControls } from './renderer-startup.js';

class Canvas {
  constructor(root, attributes = { id: 'planet', 'aria-label': 'Planète', width: 1280, height: 720, class: 'terrain' }) {
    this.root = root; this.attributes = { ...attributes }; this.id = attributes.id;
    this.width = attributes.width; this.height = attributes.height;
    this.listeners = new Map(); this.context = undefined;
  }
  addEventListener(name, callback) {
    if (!this.listeners.has(name)) this.listeners.set(name, new Set());
    this.listeners.get(name).add(callback);
  }
  removeEventListener(name, callback) { this.listeners.get(name)?.delete(callback); }
  emit(name, event = {}) { for (const callback of this.listeners.get(name) ?? []) callback(event); }
  getContext(kind) { if (this.context && this.context !== kind) return null; this.context = kind; return {}; }
  get isConnected() { return this.root.canvas === this; }
  cloneNode() { return new Canvas(this.root, this.attributes); }
  replaceWith(fresh) { assert.equal(this.root.canvas, this); this.root.canvas = fresh; }
  setPointerCapture(id) { this.capture = id; }
}

function setup(preference = 'auto', agent = 'Chrome') {
  const root = {}; root.canvas = new Canvas(root);
  const environment = { isSecureContext: true, navigator: { userAgent: agent },
    location: { origin: 'http://localhost:8001', pathname: '/world/demo' } };
  return { root, canvas: root.canvas, diagnostics: startupDiagnostics(preference, environment) };
}

test('Firefox automatically prefers its verified native GL path without a GPU preflight', async () => {
  const state = setup('auto', 'Firefox'); let probes = 0;
  const result = await startRenderer({ ...state,
    gpu: { requestAdapter: async () => { probes += 1; return {}; } },
    viewer: { start: async (id, gl) => { assert.equal(gl, true); return 'Gl'; } } });
  assert.equal(result.backend, 'Gl'); assert.equal(probes, 0);
});

test('present WebGPU with no adapter falls back before consuming the display canvas', async () => {
  const state = setup(); const calls = [];
  const result = await startRenderer({ ...state, gpu: { requestAdapter: async () => null },
    viewer: { start: async (id, gl) => { calls.push([id, gl]); return 'Gl'; } } });
  assert.deepEqual(calls, [['planet', true]]);
  assert.equal(result.canvas, state.canvas);
  assert.equal(state.diagnostics.canvas_replacements, 0);
  assert.equal(state.diagnostics.attempts[0].status, 'unavailable');
  assert.equal(state.diagnostics.pathname, '/world/demo');
});

test('a GPU context claimed before adapter failure cannot poison the GL fallback', async () => {
  const state = setup(); const seen = [];
  const result = await startRenderer({ ...state, gpu: { requestAdapter: async () => ({}) }, viewer: {
    start: async (id, gl) => {
      const canvas = state.root.canvas; seen.push(canvas);
      assert.ok(canvas.getContext(gl ? 'webgl2' : 'webgpu'));
      if (!gl) throw new Error('webgpu found no adapters after surface creation');
      return 'Gl';
    },
  } });
  assert.notEqual(seen[0], seen[1]);
  assert.equal(result.canvas, state.root.canvas);
  assert.deepEqual(result.canvas.attributes, state.canvas.attributes);
  assert.equal(result.canvas.width, 1280); assert.equal(result.canvas.height, 720);
  assert.equal(state.diagnostics.canvas_replacements, 1);
  assert.equal(state.diagnostics.selected, 'Gl');
});

test('GL preference attempts GPU on a fresh canvas when GL fails', async () => {
  const state = setup('webgl'); const order = [];
  await startRenderer({ ...state, gpu: { requestAdapter: async () => ({}) }, viewer: {
    start: async (id, gl) => {
      order.push(gl); assert.ok(state.root.canvas.getContext(gl ? 'webgl2' : 'webgpu'));
      if (gl) throw new Error('GL surface rejected');
      return 'BrowserWebGpu';
    },
  } });
  assert.deepEqual(order, [true, false]);
  assert.equal(state.diagnostics.canvas_replacements, 1);
  assert.equal(state.diagnostics.selected, 'BrowserWebGpu');
});

test('a working GL preference creates no unnecessary WebGPU adapter or device', async () => {
  const state = setup('webgl');
  const result = await startRenderer({ ...state, gpu: { requestAdapter: () => { throw new Error('must not probe'); } },
    viewer: { start: async () => 'Gl' } });
  assert.equal(result.backend, 'Gl');
  assert.equal(state.diagnostics.attempts.length, 1);
});

test('both blocked backends preserve the real GL creation reason and Firefox guidance', async () => {
  const state = setup('webgpu', 'Firefox');
  await assert.rejects(startRenderer({ ...state, gpu: { requestAdapter: async () => ({}) }, viewer: {
    start: async (id, gl) => {
      if (gl) state.root.canvas.emit('webglcontextcreationerror', { statusMessage: 'WebGL2 blocked by the graphics driver' });
      throw new Error(gl ? 'canvas.getContext() returned null' : 'GPU surface failed');
    },
  } }), /Initialisation WebGPU et WebGL2 échouée/);
  const message = startupFailureMessage('surface failure', state.diagnostics);
  assert.match(message, /about:support/);
  assert.match(message, /WebGL2 blocked by the graphics driver/);
  assert.doesNotMatch(message, /build-web|WebAssembly/);
  assert.equal(state.diagnostics.phase, 'gpu-failed');
  assert.equal(state.root.canvas.listeners.get('webglcontextcreationerror').size, 0);
});

test('WASM loading failures identify the package paths instead of blaming graphics', () => {
  const { diagnostics } = setup();
  const message = startupFailureMessage('HTTP404', diagnostics);
  assert.match(message, /\/pkg\/world_viewer\.js/);
  assert.match(message, /\/pkg\/world_viewer_bg\.wasm/);
  assert.doesNotMatch(message, /about:support|accélération matérielle/);
});

test('interface startup errors after GPU success preserve their cause and selected backend', () => {
  const { diagnostics } = setup();
  diagnostics.phase = 'stream-init'; diagnostics.selected = 'Gl';
  const message = startupFailureMessage('Worker unavailable', diagnostics);
  assert.match(message, /Worker unavailable/);
  assert.doesNotMatch(message, /contexte 3D|about:support|WebAssembly/);
  assert.equal(diagnostics.selected, 'Gl');
});

test('navigation is attached only to the successful replacement canvas', async () => {
  const state = setup(); const rotations = []; const zooms = [];
  const viewer = {
    start: async (id, gl) => { if (!gl) throw new Error('GPU failed'); return 'Gl'; },
    rotate: (...args) => rotations.push(args), zoom: (delta) => zooms.push(delta),
  };
  const { canvas } = await startRenderer({ ...state, viewer, gpu: { requestAdapter: async () => ({}) } });
  attachNavigation(canvas, viewer);
  state.canvas.emit('pointerdown', { clientX: 0, clientY: 0, pointerId: 1 });
  state.canvas.emit('pointermove', { clientX: 30, clientY: 40 });
  assert.deepEqual(rotations, []);
  canvas.emit('pointerdown', { clientX: 4, clientY: 7, pointerId: 8 });
  canvas.emit('pointermove', { clientX: 14, clientY: 22 });
  canvas.emit('pointercancel');
  canvas.emit('pointermove', { clientX: 99, clientY: 99 });
  let prevented = false;
  canvas.emit('wheel', { deltaY: -80, preventDefault: () => { prevented = true; } });
  assert.deepEqual(rotations, [[10, 15]]); assert.deepEqual(zooms, [-80]);
  assert.equal(prevented, true); assert.equal(canvas.capture, 8);
});

test('a hanging adapter check is bounded and still allows GL startup', async () => {
  const state = setup(); const calls = [];
  await startRenderer({ ...state, adapterTimeoutMs: 5, gpu: { requestAdapter: () => new Promise(() => {}) },
    viewer: { start: async (id, gl) => { calls.push(gl); return 'Gl'; } } });
  assert.deepEqual(calls, [true]);
  assert.match(state.diagnostics.attempts[0].error, /délai dépassé/);
});

test('a nonsettling GPU start detaches its canvas before a successful GL fallback and late completion', async () => {
  const state = setup(); let release;
  const staleCanvas = state.canvas;
  const result = await startRenderer({ ...state, surfaceTimeoutMs: 5, gpu: { requestAdapter: async () => ({}) }, viewer: {
    start: async (id, gl) => {
      const attemptCanvas = state.root.canvas;
      if (!gl) {
        attemptCanvas.getContext('webgpu');
        await new Promise((resolve) => { release = resolve; });
        // Mirrors the Rust guard at renderer installation, not cancellation
        // of a browser promise that may already have created GPU resources.
        if (!attemptCanvas.isConnected) throw new Error('Canvas disconnected');
        state.root.renderer = 'BrowserWebGpu';
        return 'BrowserWebGpu';
      }
      assert.equal(staleCanvas.isConnected, false);
      assert.ok(attemptCanvas.getContext('webgl2'));
      state.root.renderer = 'Gl'; return 'Gl';
    },
  } });
  assert.equal(result.backend, 'Gl');
  assert.equal(state.diagnostics.canvas_replacements, 1);
  assert.equal(state.diagnostics.attempts[0].timed_out, true);
  assert.equal(staleCanvas.listeners.get('webglcontextcreationerror').size, 0);
  release(); await new Promise((resolve) => setImmediate(resolve));
  assert.equal(state.root.renderer, 'Gl');
});

test('a final nonsettling GL start also detaches its canvas when no alternative exists', async () => {
  const state = setup('webgl');
  await assert.rejects(startRenderer({ ...state, surfaceTimeoutMs: 5, gpu: undefined,
    viewer: { start: () => new Promise(() => {}) } }), /Initialisation/);
  assert.equal(state.canvas.isConnected, false);
  assert.equal(state.diagnostics.canvas_replacements, 1);
  assert.equal(state.canvas.listeners.get('webglcontextcreationerror').size, 0);
});

test('automatic software WebGPU is skipped while explicit WebGPU remains available', async () => {
  for (const [preference, expected] of [['auto', true], ['webgpu', false]]) {
    const state = setup(preference); const calls = [];
    await startRenderer({ ...state, gpu: { requestAdapter: async () => ({ info: { isFallbackAdapter: true } }) },
      viewer: { start: async (id, gl) => { calls.push(gl); return gl ? 'Gl' : 'BrowserWebGpu'; } } });
    assert.deepEqual(calls, [expected]);
  }
});

test('navigation readiness suppresses viewer calls and discards an interrupted drag', () => {
  const state = setup(); const rotations = []; const zooms = []; let ready = false;
  attachNavigation(state.canvas, { rotate: (...args) => rotations.push(args), zoom: (delta) => zooms.push(delta) }, () => ready);
  state.canvas.emit('pointerdown', { clientX: 0, clientY: 0, pointerId: 1 });
  state.canvas.emit('wheel', { deltaY: 10, preventDefault() {} });
  ready = true; state.canvas.emit('pointermove', { clientX: 5, clientY: 5 });
  assert.deepEqual(rotations, []); assert.deepEqual(zooms, []);
  state.canvas.emit('pointerdown', { clientX: 0, clientY: 0, pointerId: 1 });
  ready = false; state.canvas.emit('pointermove', { clientX: 5, clientY: 5 });
  ready = true; state.canvas.emit('pointermove', { clientX: 10, clientY: 10 });
  assert.deepEqual(rotations, []);
  state.canvas.emit('wheel', { deltaY: -10, preventDefault() {} });
  assert.deepEqual(zooms, [-10]);
});

test('changed initial controls are applied when ready and exaggeration waits for a loaded world', () => {
  const calls = []; let loaded = false;
  const viewer = {
    set_mode: (value) => calls.push(['layer', value]), set_overlays: (...values) => calls.push(['rivers', ...values]),
    set_view_tilt: (value) => calls.push(['tilt', value]), view_info: () => JSON.stringify({ exaggeration: 1 }),
    set_exaggeration: (value) => { assert.equal(loaded, true); calls.push(['exaggeration', value]); },
  };
  const values = { layer: 3, rivers: false, tilt: 20, exaggeration: 4 };
  applyRendererControls(viewer, values);
  assert.deepEqual(calls, [['layer', 3], ['rivers', false, false], ['tilt', 20]]);
  loaded = true; applyRendererControls(viewer, values, true);
  assert.deepEqual(calls.at(-1), ['exaggeration', 4]);
});

test('shader or device failure reports renderer initialization instead of asserting absent contexts', () => {
  const state = setup(); state.diagnostics.phase = 'gpu-failed';
  state.diagnostics.attempts = [{ backend: 'webgl', status: 'failed', error: 'Shader validation failed' }];
  const message = startupFailureMessage('device failure', state.diagnostics);
  assert.match(message, /Shader validation failed/);
  assert.match(message, /moteur 3D/);
  assert.doesNotMatch(message, /aucun contexte|aucun adapter/);
});
