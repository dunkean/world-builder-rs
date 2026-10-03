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

test('renderer eviction requests a formerly loaded parent again without a world reset', () => {
  const { messages, stream, context, requests, respond } = setup();
  stream.update([requests[0]], { ...context, residentKeys: [] });
  respond(messages.find((m) => m.type === 'load'));
  stream.installNext();
  assert.ok(stream.loaded.has('1'));
  stream.update([requests[0]], { ...context, residentKeys: [requests[0]] });
  assert.equal(messages.filter((m) => m.type === 'load').length, 1);
  // A completed inverse cut evicted the parent without installing any packet.
  stream.update([requests[0]], { ...context, residentKeys: [] });
  assert.equal(messages.filter((m) => m.type === 'load' && m.key === '1').length, 2);
  assert.equal(stream.epoch, context.epoch);
});

test('resident refresh discards an unnecessary prepared packet and preserves queue accounting', () => {
  const { messages, installed, stream, context, requests, respond } = setup();
  stream.update(requests.slice(0, 2), { ...context, residentKeys: [] });
  const first = messages.find((m) => m.key === '1');
  const second = messages.find((m) => m.key === '2');
  respond(first);
  assert.equal(stream.metrics.ready_bytes, 1);
  stream.update(requests.slice(0, 2), { ...context, residentKeys: requests.slice(0, 2) });
  assert.equal(stream.ready.length, 0);
  assert.equal(stream.metrics.ready_bytes, 0);
  assert.ok(messages.some((m) => m.type === 'cancel' && m.id === second.id));
  respond(second);
  stream.installNext();
  assert.deepEqual(installed, []);
  assert.equal(stream.jobs.size, 0);
  assert.equal(messages.filter((m) => m.type === 'load').length, 2);
});

test('synchronous epoch reset rejects prepared and in-flight old terrain before the next update', () => {
  const { messages, installed, stream, context, requests, respond } = setup();
  stream.update(requests.slice(0, 2), context);
  const first = messages.find((m) => m.key === '1');
  const second = messages.find((m) => m.key === '2');
  respond(first);
  assert.equal(stream.ready.length, 1);
  stream.reset(2);
  respond(second);
  stream.installNext();
  assert.deepEqual(installed, []);
  assert.equal(stream.ready.length, 0);
  assert.equal(stream.jobs.size, 0);
  assert.equal(stream.metrics.ready_bytes, 0);
  assert.equal(stream.loaded.size, 0);
  // Reset does not submit the previous world's demand between animation frames.
  assert.equal(messages.filter((m) => m.type === 'load').length, 2);
  stream.update([requests[0]], { ...context, epoch: 2 });
  assert.ok(messages.some((m) => m.type === 'load' && m.key === '1' && m.epoch === 2));
});

test('camera cancellation releases dispatch slots before the worker acknowledges old jobs', () => {
  const { messages, stream, context, requests, respond } = setup();
  stream.update(requests.slice(0, 3), context);
  const previous = messages.filter((m) => m.type === 'load');
  stream.update(requests.slice(10, 13), context);
  assert.equal(stream.jobs.size, 3);
  for (const tile of requests.slice(10, 13)) {
    assert.ok(messages.some((m) => m.type === 'load' && m.key === tile.key));
  }
  for (const job of previous) respond(job);
  assert.equal(stream.ready.length, 0);
  assert.equal(stream.jobs.size, 3);
  assert.equal(messages.filter((m) => m.type === 'load').length, 6);
});

test('future detail leaves a slot for new visible demand without cancelling useful jobs', () => {
  const { messages, stream, context, requests } = setup();
  stream.update(requests.slice(0, 3), { ...context, critical: new Set() });
  const original = messages.filter((m) => m.type === 'load');
  assert.equal(original.length, 2);
  // The centre changes before either speculative fetch completes.
  const centre = requests[3];
  stream.update([centre, ...requests.slice(0, 3)], { ...context, critical: new Set([centre.key]) });
  assert.ok(messages.some((m) => m.type === 'load' && m.key === centre.key));
  assert.equal(stream.jobs.size, 3);
  assert.equal(messages.filter((m) => m.type === 'cancel').length, 0);
  for (const job of original) assert.ok(stream.jobs.has(job.id));
  assert.equal(messages.filter((m) => m.type === 'load' && m.key === '3').length, 0);
});

test('all three slots remain usable for visible demand and a partial quartet can finish', () => {
  const { messages, stream, context, requests, respond } = setup();
  const critical = new Set(requests.slice(0, 4).map((tile) => tile.key));
  stream.update(requests, { ...context, critical });
  assert.deepEqual(messages.filter((m) => m.type === 'load').map((m) => m.key), ['1', '2', '3']);
  respond(messages.find((m) => m.type === 'load' && m.key === '1'));
  assert.ok(messages.some((m) => m.type === 'load' && m.key === '4'));
  assert.equal(stream.jobs.size, 3);
  assert.equal(messages.filter((m) => m.type === 'load' && m.key === '5').length, 0);
});
