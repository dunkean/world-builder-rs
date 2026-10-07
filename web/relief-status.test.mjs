import assert from 'node:assert/strict';
import test from 'node:test';
import { terrainStatus, reliefSummary } from './relief-status.js';

test('orbital hybrid selection explicitly reports no NN and offers the demo action', () => {
  const text = terrainStatus({ mode: 'hybrid', desired: 0, ready_wanted: 0, applied_wanted: 0 }, 95000, 0.2);
  assert.match(text, /Vue globale procédurale/);
  assert.match(text, /aucun détail NN demandé/);
  assert.match(text, /Voir relief NN/);
  assert.doesNotMatch(text, /0\/0|Champ NN validé/);
});

test('loaded data never claims applied geometry before the field commits', () => {
  const text = terrainStatus({ mode: 'hybrid', desired: 2, ready_wanted: 2, applied_wanted: 0 }, 24, 0.2);
  assert.match(text, /Terrain procédural affiché/);
  assert.match(text, /2\/2 patches chargés/);
  assert.doesNotMatch(text, /Champ NN validé/);
});

test('failed request retains an applied field instead of claiming a procedural rollback', () => {
  const text = terrainStatus({ mode: 'hybrid', desired: 2, ready_wanted: 1, applied_wanted: 1, last_error: 'network' }, 6, 0.2);
  assert.match(text, /Champ NN validé pour 1\/2/);
  assert.match(text, /terrain précédent conservé/);
  assert.doesNotMatch(text, /Terrain procédural affiché/);
});

test('numeric text alternative distinguishes actual NN metres from a morph target', () => {
  const probe = { direction: [0, 0, 1], displayed_height_m: 681.3,
    triangle: [44, 44.5, 44.2].map((learned_delta_m) => ({ learned_delta_m, physical_slope_degrees: 25, morph_progress: 1 })) };
  assert.match(reliefSummary(probe), /altitude 681,3 m/);
  assert.match(reliefSummary(probe), /Δ NN appliqué : 44 à 44,5 m/);
  probe.triangle[0].morph_progress = 0.4;
  assert.match(reliefSummary(probe), /Δ NN cible, transition en cours/);
  probe.triangle[0].learned_delta_m = undefined;
  assert.match(reliefSummary(probe), /indisponible/);
});
