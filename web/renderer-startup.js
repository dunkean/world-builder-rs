const preferences = new Set(['auto', 'webgl', 'webgpu']);

export function startupDiagnostics(preference = 'auto', environment = globalThis) {
  return {
    phase: 'wasm-load',
    preference: preferences.has(preference) ? preference : 'auto',
    secure_context: Boolean(environment.isSecureContext),
    origin: environment.location?.origin ?? '',
    pathname: environment.location?.pathname ?? '',
    user_agent: environment.navigator?.userAgent ?? '',
    attempts: [],
    canvas_replacements: 0,
  };
}

export function startupFailureMessage(error, diagnostics) {
  if (diagnostics.phase === 'wasm-load' || diagnostics.phase === 'wasm-init') {
    return `Le moteur WebAssembly ne peut pas être chargé (${diagnostics.phase}) : ${error}. Vérifiez l’accès à /pkg/world_viewer.js et /pkg/world_viewer_bg.wasm.`;
  }
  if (diagnostics.phase !== 'gpu-start' && diagnostics.phase !== 'gpu-failed') {
    if (diagnostics.selected) return `Le moteur 3D est initialisé, mais l’interface ne peut pas démarrer : ${error}.`;
    return `L’explorateur ne peut pas terminer son initialisation : ${error}.`;
  }
  const firefox = diagnostics.user_agent.includes('Firefox');
  const support = firefox ? ' Dans Firefox, consultez about:support → Graphiques.' : '';
  const secure = diagnostics.secure_context ? '' : ' WebGPU nécessite localhost ou HTTPS.';
  const details = diagnostics.attempts.map((attempt) =>
    `${attempt.backend} : ${attempt.context_error || attempt.error || attempt.status}`).join(' ; ');
  return `Le moteur 3D ne peut pas être initialisé.${support}${secure} ${details || String(error)}`;
}

async function availableAdapter(gpu, timeoutMs) {
  if (!gpu?.requestAdapter) return { available: false, error: 'API WebGPU absente' };
  let timer;
  try {
    const adapter = await Promise.race([
      gpu.requestAdapter({ powerPreference: 'high-performance' }),
      new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Adapter WebGPU : délai dépassé')), timeoutMs); }),
    ]);
    return adapter ? { available: true,
      software: (adapter.info?.isFallbackAdapter ?? adapter.isFallbackAdapter) === true }
      : { available: false, error: 'Aucun adapter WebGPU disponible' };
  } catch (error) {
    return { available: false, error: String(error) };
  } finally {
    clearTimeout(timer);
  }
}

// Never probe getContext on the display canvas. A failed backend may already
// own its context, so every later surface attempt receives a fresh clone.
export async function startRenderer({ viewer, canvas, diagnostics,
  gpu = globalThis.navigator?.gpu, adapterTimeoutMs = 2500, surfaceTimeoutMs = 8000 }) {
  diagnostics.phase = 'gpu-start';
  const preferGl = diagnostics.preference === 'webgl'
    || diagnostics.preference === 'auto' && diagnostics.user_agent.includes('Firefox');
  const candidates = preferGl ? ['webgl', 'webgpu'] : ['webgpu', 'webgl'];
  let surfaceAttempted = false;
  const replaceCanvas = () => {
    const fresh = canvas.cloneNode(false);
    canvas.replaceWith(fresh);
    canvas = fresh;
    diagnostics.canvas_replacements += 1;
    surfaceAttempted = false;
  };
  for (const backend of candidates) {
    const attempt = { backend, status: 'checking' };
    diagnostics.attempts.push(attempt);
    if (backend === 'webgpu') {
      const result = await availableAdapter(gpu, adapterTimeoutMs);
      if (!result.available) {
        Object.assign(attempt, { status: 'unavailable', error: result.error });
        continue;
      }
      if (result.software && diagnostics.preference === 'auto') {
        Object.assign(attempt, { status: 'skipped', error: 'Adapter WebGPU logiciel ; préférence WebGL en mode automatique' });
        continue;
      }
    }
    if (surfaceAttempted) replaceCanvas();
    const attemptCanvas = canvas;
    const creationError = (event) => { attempt.context_error = String(event.statusMessage || 'Création du contexte refusée'); };
    attemptCanvas.addEventListener('webglcontextcreationerror', creationError);
    surfaceAttempted = true;
    attempt.status = 'starting';
    let surfaceTimer;
    try {
      const selected = await Promise.race([
        viewer.start(attemptCanvas.id, backend === 'webgl'),
        new Promise((_, reject) => {
          surfaceTimer = setTimeout(() => {
            attempt.timed_out = true;
            // Rust also checks isConnected before installing its renderer.
            // Detach now, even if no other backend can subsequently start.
            replaceCanvas();
            reject(new Error(`${backend} : initialisation dépassant ${surfaceTimeoutMs} ms`));
          }, surfaceTimeoutMs);
        }),
      ]);
      Object.assign(attempt, { status: 'ready', selected });
      diagnostics.phase = 'ready'; diagnostics.selected = selected;
      return { canvas, backend: selected };
    } catch (error) {
      Object.assign(attempt, { status: 'failed', error: String(error) });
    } finally {
      clearTimeout(surfaceTimer);
      attemptCanvas.removeEventListener('webglcontextcreationerror', creationError);
    }
  }
  diagnostics.phase = 'gpu-failed';
  throw new Error('Initialisation WebGPU et WebGL2 échouée');
}

export function applyRendererControls(viewer, { layer, rivers, tilt, exaggeration }, worldReady = false) {
  viewer.set_mode(layer); viewer.set_overlays(rivers, false); viewer.set_view_tilt(tilt);
  // set_exaggeration rebuilds roots and requires a loaded world.
  if (worldReady && JSON.parse(viewer.view_info()).exaggeration !== exaggeration) viewer.set_exaggeration(exaggeration);
}

export function attachNavigation(canvas, viewer, isReady = () => true) {
  let pointer;
  canvas.addEventListener('pointerdown', (event) => {
    if (!isReady()) return;
    pointer = { x: event.clientX, y: event.clientY };
    canvas.setPointerCapture(event.pointerId);
  });
  canvas.addEventListener('pointermove', (event) => {
    if (!isReady()) { pointer = undefined; return; }
    if (!pointer) return;
    viewer.rotate(event.clientX - pointer.x, event.clientY - pointer.y);
    pointer = { x: event.clientX, y: event.clientY };
  });
  for (const event of ['pointerup', 'pointercancel']) canvas.addEventListener(event, () => { pointer = undefined; });
  canvas.addEventListener('wheel', (event) => { event.preventDefault(); if (isReady()) viewer.zoom(event.deltaY); }, { passive: false });
}
