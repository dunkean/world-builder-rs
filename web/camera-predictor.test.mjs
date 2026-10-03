import assert from 'node:assert/strict';
import test from 'node:test';
import { CameraPredictor } from './camera-predictor.js';
const radius = 1737000;
const pose = (yaw = 0, altitude_m = 1000) => ({ direction: [Math.sin(yaw), 0, Math.cos(yaw)], altitude_m });

test('stationary camera does not request a moving forecast', () => {
  const predictor = new CameraPredictor();
  for (let t = 0; t <= 1000; t += 100) assert.equal(predictor.sample(pose(), t, radius), undefined);
});
test('fast wheel predicts at most one additional detail level', () => {
  const predictor = new CameraPredictor(); predictor.sample(pose(0, 1000), 0, radius);
  const forecast = predictor.sample(pose(0, 500), 100, radius);
  assert.ok(forecast.altitude_m >= 250 && forecast.altitude_m <= 500);
  assert.equal(Math.hypot(...forecast.direction), 1);
});
test('yaw wrap is continuous and destination jumps reset forecasting', () => {
  const predictor = new CameraPredictor(); predictor.sample(pose(Math.PI - 0.0001), 0, radius);
  const forecast = predictor.sample(pose(-Math.PI + 0.0001), 100, radius);
  assert.ok(forecast.direction[2] < -0.999);
  assert.equal(predictor.sample(pose(0), 200, radius), undefined);
  predictor.reset(); assert.equal(predictor.sample(pose(), 300, radius), undefined);
});
test('forecast decays after input stops and cannot change the current pose', () => {
  const predictor = new CameraPredictor(); predictor.sample(pose(), 0, radius);
  const current = pose(0.0001); const before = JSON.stringify(current);
  const early = predictor.sample(current, 100, radius);
  let late;
  for (let t = 200; t <= 1000; t += 100) late = predictor.sample(current, t, radius);
  assert.equal(JSON.stringify(current), before);
  assert.ok(!late || Math.abs(Math.atan2(late.direction[0], late.direction[2]) - 0.0001)
    < Math.abs(Math.atan2(early.direction[0], early.direction[2]) - 0.0001) * 0.01);
});
