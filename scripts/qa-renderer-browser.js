async (page) => {
  // Playwright CLI expression, executed once for setup and once per case.
  // It uses the existing page: no second canvas, context or GPU device.
  const worldId = '8f797784bac1c2eb8363';
  const errors = [];
  const onError = (error) => errors.push({ kind: 'pageerror', message: error.message });
  const onConsole = (entry) => {
    if (entry.type() === 'error') errors.push({ kind: 'console', message: entry.text() });
  };
  page.on('pageerror', onError);
  page.on('console', onConsole);
  try {
    const initialized = await page.evaluate(() => Boolean(window.qaRendering10));
    if (!initialized) {
      await page.waitForFunction((id) => typeof window.worldBuilderPerf?.().view?.active_level === 'number'
        && document.getElementById('stats')?.textContent.includes(id.slice(0, 16)), worldId, { timeout: 60000 });
      const setup = await page.evaluate(async ({ worldId }) => {
        const fetchJson = async (path) => {
          const response = await fetch(path);
          if (!response.ok) throw new Error(`${path}: ${response.status}`);
          return response.json();
        };
        const [health, manifest, forms, rivers] = await Promise.all([
          fetchJson('/api/health'),
          fetchJson(`/api/worlds/${worldId}/manifest`),
          fetchJson(`/api/worlds/${worldId}/landforms`),
          fetchJson(`/api/worlds/${worldId}/rivers`),
        ]);
        if (health.generator_version !== 'prototype-0.10.0') throw new Error(`Backend not 0.10: ${health.generator_version}`);
        if (!document.getElementById('stats').textContent.includes(worldId.slice(0, 16))) throw new Error('The visible world is not the requested atlas');
        const viewer = await import('/pkg/world_viewer.js');
        const radius = manifest.config.radius_m;
        const normalize = (v) => {
          const n = Math.hypot(...v);
          return v.map((x) => x / n);
        };
        const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
        const point = async (direction) => {
          const p = normalize(direction);
          const lat = Math.asin(p[1]) * 180 / Math.PI;
          const lon = Math.atan2(p[2], p[0]) * 180 / Math.PI;
          return fetchJson(`/api/worlds/${worldId}/sample?lat=${lat}&lon=${lon}`);
        };
        const marine = (sample) => sample.water_body_id === 0 && sample.water_m > sample.height_m;
        const shore = async (origin) => {
          const p = normalize(origin);
          const reference = Math.abs(p[1]) < 0.9 ? [0, 1, 0] : [1, 0, 0];
          const e1 = normalize(cross(reference, p));
          const e2 = cross(p, e1);
          const offset = (metres, angle) => normalize(p.map((x, i) => x * Math.cos(metres / radius)
            + (e1[i] * Math.cos(angle) + e2[i] * Math.sin(angle)) * Math.sin(metres / radius)));
          const center = await point(p);
          let bracket;
          for (const metres of [250, 750, 2000, 5000, 12000, 30000, 65000]) {
            const ring = await Promise.all(Array.from({ length: 8 }, async (_, i) => {
              const direction = offset(metres, i * Math.PI / 4);
              return { direction, sample: await point(direction) };
            }));
            const opposite = ring.find(({ sample }) => marine(center) ? sample.height_m > 0 : marine(sample));
            if (opposite) {
              bracket = marine(center) ? { wet: p, dry: opposite.direction } : { wet: opposite.direction, dry: p };
              break;
            }
          }
          if (!bracket) throw new Error('No marine shore found around destination');
          for (let i = 0; i < 25; i += 1) {
            const middle = normalize(bracket.wet.map((x, j) => x + bracket.dry[j]));
            const sample = await point(middle);
            if (marine(sample)) bracket.wet = middle;
            else bracket.dry = middle;
          }
          const direction = normalize(bracket.wet.map((x, i) => x + bracket.dry[i]));
          const [wet, dry] = await Promise.all([point(bracket.wet), point(bracket.dry)]);
          const dot = bracket.wet.reduce((sum, x, i) => sum + x * bracket.dry[i], 0);
          return { direction, wet, dry, bracket_m: Math.atan2(Math.hypot(...cross(bracket.wet, bracket.dry)), dot) * radius };
        };
        const fjord = forms.features.find((place) => /fjord/i.test(place.label));
        const islands = forms.features.find((place) => /archipel/i.test(place.label));
        const lakes = forms.features.find((place) => /lacs/i.test(place.label));
        const river = rivers.destinations.find((place) => /grande|rivière/i.test(place.label));
        if (![fjord, islands, lakes, river].every(Boolean)) throw new Error('Required certified destinations missing');
        const riverSample = await point(river.direction);
        if (!(riverSample.water_m > riverSample.height_m && riverSample.river > 0.5 && !riverSample.lake && riverSample.water_body_id !== 0)) throw new Error('The river destination is not actually wet');
        const fjordShore = await shore(fjord.direction);
        const islandShore = await shore(islands.direction);
        const plan = [
          { name: 'orbit-cold', direction: fjordShore.direction, altitude_m: radius * 7, tilt: 0 },
          { name: 'wheel-pan-coast', direction: fjordShore.direction, altitude_m: radius * 7, tilt: 0, interaction: 'wheel-pointer' },
          { name: 'early-coast', direction: fjordShore.direction, altitude_m: 1000000, tilt: 0 },
          { name: 'fjord-wide', direction: fjord.direction, altitude_m: fjord.altitude_m, tilt: 0 },
          { name: 'fjord-shore-500m', direction: fjordShore.direction, altitude_m: 500, tilt: 0 },
          { name: 'fjord-shore-50m', direction: fjordShore.direction, altitude_m: 50, tilt: 0 },
          { name: 'archipelago-wide', direction: islands.direction, altitude_m: islands.altitude_m, tilt: 0 },
          { name: 'island-shore-500m', direction: islandShore.direction, altitude_m: 500, tilt: 0 },
          { name: 'lake-province-8km', direction: lakes.direction, altitude_m: lakes.altitude_m, tilt: 0 },
          { name: 'lake-province-1km', direction: lakes.direction, altitude_m: 1000, tilt: 0 },
          { name: 'river-wide', direction: river.direction, altitude_m: river.altitude_m, tilt: 25 },
          { name: 'river-300m', direction: river.direction, altitude_m: 300, tilt: 25 },
          { name: 'fjord-shore-500m-revisit', direction: fjordShore.direction, altitude_m: 500, tilt: 0 },
          { name: 'fjord-shore-5m-tilted', direction: fjordShore.direction, altitude_m: 5, tilt: 30 },
          { name: 'island-shore-5m-tilted', direction: islandShore.direction, altitude_m: 5, tilt: 30 },
          { name: 'return-orbit', direction: fjordShore.direction, altitude_m: radius * 7, tilt: 0 },
        ];
        document.getElementById('layer').value = '0';
        viewer.set_mode(0);
        document.getElementById('show-rivers').checked = true;
        viewer.set_overlays(true, false);
        const observation = { running: true, phase: 'idle', frames: [], longtasks: [], last: undefined };
        const tick = (time) => {
          if (observation.last !== undefined && observation.phase !== 'idle') observation.frames.push({ phase: observation.phase, ms: time - observation.last });
          observation.last = time;
          if (observation.running) requestAnimationFrame(tick);
        };
        requestAnimationFrame(tick);
        observation.observer = new PerformanceObserver((list) => {
          for (const entry of list.getEntries()) if (observation.phase !== 'idle') observation.longtasks.push({ phase: observation.phase, ms: entry.duration });
        });
        observation.observer.observe({ entryTypes: ['longtask'] });
        const destinations = { fjord, islands, lakes, river };
        window.qaRendering10 = { viewer, plan, index: 0, results: [], observation, manifest, destinations, fjordShore, islandShore, riverSample };
        return { setup: true, world_id: worldId, generator: manifest.generator_version, surface_revision: manifest.surface_revision,
          backend: document.getElementById('backend').textContent, canvas: { width: document.getElementById('planet').width, height: document.getElementById('planet').height },
          destinations, fjordShore, islandShore, riverSample, scenarios: plan.map(({ name }) => name) };
      }, { worldId });
      const browser = page.context().browser();
      if (browser) {
        const cdp = await browser.newBrowserCDPSession();
        try { setup.gpu_devices = (await cdp.send('SystemInfo.getInfo')).gpu.devices; }
        finally { await cdp.detach(); }
      }
      return { ...setup, errors };
    }

    const scenario = await page.evaluate(() => {
      const q = window.qaRendering10;
      const scenario = q.plan[q.index];
      if (!scenario) return null;
      q.observation.frames = [];
      q.observation.longtasks = [];
      q.observation.last = undefined;
      q.observation.phase = 'transition';
      q.start = performance.now();
      q.before = window.worldBuilderPerf();
      q.viewer.set_view_tilt(scenario.tilt);
      const tilt = document.getElementById('view-tilt');
      tilt.value = String(scenario.tilt);
      tilt.dispatchEvent(new Event('input', { bubbles: true }));
      if (scenario.interaction !== 'wheel-pointer') q.viewer.fly_to(JSON.stringify(scenario.direction), scenario.altitude_m);
      return scenario;
    });
    if (!scenario) {
      return await page.evaluate(() => {
        const q = window.qaRendering10;
        q.observation.running = false;
        q.observation.observer.disconnect();
        return { finished: true, results: q.results };
      });
    }

    const started = Date.now();
    const interactionSamples = [];
    const intermediateScreenshots = [];
    if (scenario.interaction === 'wheel-pointer') {
      const bounds = await page.locator('#planet').boundingBox();
      if (!bounds) throw new Error('Planet canvas not visible');
      const x = bounds.x + bounds.width * 0.5;
      const y = bounds.y + bounds.height * 0.5;
      await page.mouse.move(x, y);
      const sample = async (label) => {
        const data = await page.evaluate(() => {
          const p = window.worldBuilderPerf();
          const { resident_keys, requests, ...view } = p.view;
          return { view, in_flight: p.in_flight, queued: p.queued, memory_hits: p.memory_hits, disk_hits: p.disk_hits, network: p.network };
        });
        interactionSamples.push({ label, elapsed_ms: Date.now() - started, ...data });
      };
      const capture = async (name) => {
        await page.evaluate(() => { window.qaRendering10.observation.phase = 'idle'; });
        const path = `output/playwright/renderer-10-${name}.png`;
        await page.screenshot({ path });
        intermediateScreenshots.push(path);
        await page.evaluate(() => {
          window.qaRendering10.observation.phase = 'transition';
          window.qaRendering10.observation.last = undefined;
        });
      };
      for (let step = 1; step <= 10; step += 1) {
        await page.mouse.wheel(0, -420);
        await page.waitForTimeout(200);
        await sample(`zoom-in-${step}`);
        if (step === 2) await capture('wheel-first-lod');
        if (step === 5) {
          await page.mouse.down();
          await page.mouse.move(x + 8, y + 5, { steps: 4 });
          await page.mouse.up();
          await page.waitForTimeout(150);
          await capture('wheel-coast-pan');
          await page.mouse.down();
          await page.mouse.move(x, y, { steps: 4 });
          await page.mouse.up();
        }
        if (step === 10) await capture('wheel-coast-fast');
      }
      for (let step = 1; step <= 10; step += 1) {
        await page.mouse.wheel(0, 420);
        await page.waitForTimeout(100);
        await sample(`zoom-out-${step}`);
      }
      await capture('wheel-return-orbit');
    }
    let centerReadyMs = null;
    let queuesIdleMs = null;
    let centerSince = null;
    while (Date.now() - started < 32000) {
      await page.waitForTimeout(200);
      const current = await page.evaluate(() => {
        const p = window.worldBuilderPerf();
        return { center: p.view.active_level === p.view.level && p.view.pending_tiles === 0, idle: p.in_flight === 0 && p.queued === 0 };
      });
      if (current.center && current.idle && queuesIdleMs === null) queuesIdleMs = Date.now() - started;
      if (current.center) {
        centerSince ??= Date.now();
        if (Date.now() - centerSince >= 600) { centerReadyMs = Date.now() - started; break; }
      } else centerSince = null;
    }
    await page.evaluate(() => {
      window.qaRendering10.observation.phase = 'settled';
      window.qaRendering10.observation.last = undefined;
    });
    const settledStarted = Date.now();
    while (Date.now() - settledStarted < 3000) {
      await page.waitForTimeout(200);
      if (queuesIdleMs === null && await page.evaluate(() => {
        const p = window.worldBuilderPerf();
        return p.view.active_level === p.view.level && p.view.pending_tiles === 0 && p.in_flight === 0 && p.queued === 0;
      })) queuesIdleMs = Date.now() - started;
    }
    const result = await page.evaluate(({ scenario, centerReadyMs, queuesIdleMs }) => {
      const q = window.qaRendering10;
      q.observation.phase = 'idle';
      const current = window.worldBuilderPerf();
      const { resident_keys, requests, ...view } = current.view;
      const quantiles = (phase) => {
        const values = q.observation.frames.filter((frame) => frame.phase === phase).map((frame) => frame.ms).sort((a, b) => a - b);
        return { frames: values.length, raf_p50_ms: values[Math.floor(values.length * 0.5)], raf_p95_ms: values[Math.floor(values.length * 0.95)], raf_max_ms: values.at(-1) };
      };
      const counters = Object.fromEntries(['memory_hits', 'disk_hits', 'network', 'installed', 'stale'].map((key) => [key, (current[key] ?? 0) - (q.before[key] ?? 0)]));
      const result = { ...scenario, elapsed_ms: performance.now() - q.start, center_ready_ms: centerReadyMs, queues_idle_ms: queuesIdleMs,
        center_ready: view.active_level === view.level && view.pending_tiles === 0, view, transition: quantiles('transition'), settled: quantiles('settled'),
        longtasks: q.observation.longtasks, counters, in_flight: current.in_flight, queued: current.queued,
        cpu_render_submit_ms: current.render_ms, cpu_render_submit_max_ms_since_page_load: current.max_render_ms,
        screenshot: `output/playwright/renderer-10-${scenario.name}.png`,
        surface_edge_audit: typeof q.viewer.surface_edge_audit === 'function'
          ? JSON.parse(q.viewer.surface_edge_audit()) : undefined,
        surface_probe: scenario.altitude_m <= 5 && typeof q.viewer.surface_probe === 'function'
          ? JSON.parse(q.viewer.surface_probe(JSON.stringify(view.direction))) : undefined };
      q.results.push(result);
      q.index += 1;
      return result;
    }, { scenario, centerReadyMs, queuesIdleMs });
    result.interaction = scenario.interaction ?? 'fly-to';
    result.interaction_samples = interactionSamples;
    result.intermediate_screenshots = intermediateScreenshots;
    // PNG encoding/capture is outside both cadence measurement windows.
    await page.screenshot({ path: result.screenshot });
    return { ...result, errors };
  } finally {
    page.removeListener('pageerror', onError);
    page.removeListener('console', onConsole);
  }
}
