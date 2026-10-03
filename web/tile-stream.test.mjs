import assert from 'node:assert/strict';
import test from 'node:test';
import { TileStream } from './tile-stream.js';

function setup() {
  const messages = [];
  const installed = [];
  const worker = { postMessage: (message) => messages.push(message) };
  const stream = new TileStream(worker, (bytes) => {
    installed.push(bytes[0]);
    return JSON.stringify(installed.map((key) => ({ key: String(key) })));
  }, () => {});
  const context = { epoch: 1, keyOf: (tile) => tile.key, urlOf: (tile) => tile.key,
    radius: 1737000, exaggeration: 1, format: 'postcard' };
  const requests = Array.from({ length: 20 }, (_, i) => ({ key: String(i + 1) }));
  function respond(job) {
    worker.onmessage({ data: { type: 'tile', id: job.id, key: job.key, epoch: job.epoch,
      packet: Uint8Array.of(Number(job.key)).buffer, metrics: {} } });
  }
  return { messages, installed, worker, stream, context, requests, respond };
}

test('resident coastal tiles are reused, uploads wait for animation frames', () => {
  const { messages, installed, stream, context, requests, respond } = setup();
  stream.update(requests, context);
  assert.equal(messages.filter((m) => m.type === 'load').length, 3);
  const job = messages.find((m) => m.key === '1');
  respond(job);
  assert.equal(installed.length, 0);
  stream.installNext();
  assert.deepEqual(installed, [1]);
  stream.update(requests, context);
  assert.equal(messages.filter((m) => m.type === 'load' && m.key === '1').length, 1);
});

test('scroll cancels obsolete demand and rejects old world or season packets', () => {
  const { messages, stream, context, requests, respond } = setup();
  stream.update(requests, context);
  const old = messages.find((m) => m.key === '2');
  stream.update([requests[0]], context);
  assert.ok(messages.some((m) => m.type === 'cancel' && m.id === old.id));
  respond(old);
  assert.equal(stream.ready.length, 0);
  const first = messages.find((m) => m.key === '1');
  stream.update([requests[0]], { ...context, epoch: 2 });
  respond(first);
  assert.equal(stream.ready.length, 0);
  assert.equal(stream.loaded.size, 0);
});

test('worker completion queue is bounded even while the renderer is busy', () => {
  const { messages, stream, context, requests, respond } = setup();
  stream.update(requests, context);
  const completed = new Set();
  for (let step = 0; step < 12; step += 1) {
    const job = messages.find((m) => m.type === 'load' && !completed.has(m.id));
    if (!job) break;
    completed.add(job.id); respond(job);
    assert.ok(stream.jobs.size <= 3);
    assert.ok(stream.jobs.size + stream.ready.length <= 6);
  }
  assert.equal(stream.ready.length, 6);
  assert.equal(messages.filter((m) => m.type === 'load').length, 6);
});


test('camera demand reprioritizes prepared arrivals before speculative neighbors', () => {
  const { messages, installed, stream, context, requests, respond } = setup();
  stream.update(requests.slice(0, 3), context);
  respond(messages.find((m) => m.key === '3'));
  respond(messages.find((m) => m.key === '1'));
  stream.update([requests[1], requests[2], requests[0]], context);
  respond(messages.find((m) => m.key === '2'));
  stream.installNext();
  stream.installNext();
  stream.installNext();
  assert.deepEqual(installed, [2, 3, 1]);
});
