import assert from 'node:assert/strict';
import { test } from 'node:test';
import * as fs from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { host, run } from './timing.mjs';

const base = {
  INPUT_PHASE: 'prepare', INPUT_SUITE: 'workspace', GITHUB_WORKSPACE: '/fixture',
  GITHUB_RUN_ID: '1', GITHUB_RUN_ATTEMPT: '1', GITHUB_JOB: 'test', GITHUB_SHA: 'abc',
};
function fixture(extra = {}) {
  const files = new Map();
  const env = { ...base, ...extra };
  let clock = '2026-01-01T00:00:00.000Z';
  const io = {
    read: path => files.get(path) ?? null,
    write: (path, data) => files.set(path, data),
    append: (path, data) => files.set(path, (files.get(path) ?? '') + data),
    remove: path => { for (const key of files.keys()) if (key === path || key.startsWith(path + '/')) files.delete(key); },
    mkdir: () => {}, now: () => clock, version: program => `${program} pinned`,
    hardware: () => ({ cpu_models: ['fixture CPU'], available_parallelism: 4 }),
  };
  return {
    env, files, io,
    finish(changes = {}) {
      clock = '2026-01-01T00:00:02.500Z';
      const directory = run({ ...env, INPUT_PHASE: 'finish', INPUT_OUTCOME: 'success', ...changes }, io);
      return JSON.parse(files.get(`${directory}/metadata.json`));
    },
  };
}

test('fresh reports retain hashes, identity, environment and separate hook duration', () => {
  // Arrange
  const current = fixture({ GITHUB_OUTPUT: '/output', 'INPUT_EXACT-CACHE-HIT': 'false',
    INPUT_CONTAINER: 'image@sha256:123', RUNNER_OS: 'Linux', RUNNER_ARCH: 'X64',
    ImageOS: 'ubuntu24', ImageVersion: '123', 'INPUT_BUILD-DIRECTORY': 'target/llvm-cov-target' });
  const junit = '/fixture/target/nextest/ci/junit.xml';
  const cargo = '/fixture/target/llvm-cov-target/cargo-timings/cargo-timing.html';
  current.files.set(junit, 'stale');
  current.files.set(cargo, 'stale');
  current.files.set('/fixture/target/test-results/workspace/metadata.json', 'stale');
  // Act
  const directory = run(current.env, current.io);
  assert.equal(current.files.has(junit), false);
  assert.equal(current.files.has(cargo), false);
  assert.equal(current.files.has(`${directory}/metadata.json`), false);
  current.files.set(junit, 'fresh XML');
  current.files.set(cargo, 'fresh HTML');
  const result = current.finish();
  // Assert
  assert.deepEqual(result.identity, { suite: 'workspace', run_id: '1', attempt: '1', job: 'test', sha: 'abc' });
  assert.equal(result.environment.exact_cache_hit, false);
  assert.equal(result.environment.container, 'image@sha256:123');
  assert.equal(result.environment.image_version, '123');
  assert.equal(result.environment.rustc, 'rustc pinned');
  assert.equal(result.environment.nextest, 'cargo pinned');
  assert.deepEqual(result.environment.cpu_models, ['fixture CPU']);
  assert.equal(result.environment.available_parallelism, 4);
  assert.equal(result.hook_seconds, 2.5);
  assert.equal(result.prepared, true);
  assert.equal(result.reports['junit.xml'].length, 64);
  assert.equal(current.files.get(`${directory}/cargo-timing.html`), 'fresh HTML');
  assert.equal(current.files.get('/output'), `directory=${directory}\ndirectory=${directory}\n`);
  // A second finalization cannot reuse a consumed preparation record.
  assert.equal(current.finish().prepared, false);
  assert.equal(current.files.has(`${directory}/junit.xml`), false);
  const native = fixture({ INPUT_SUITE: 'native-sandbox-test-results-ubuntu-24.04' });
  run(native.env, native.io);
  assert.equal(native.finish().identity.suite, 'native-sandbox-test-results-ubuntu-24.04');
});

test('missing and failed results stay explicit and reruns cannot borrow old reports', () => {
  for (const changes of [null, { GITHUB_RUN_ATTEMPT: '2' }, { GITHUB_JOB: 'other' },
    { GITHUB_SHA: 'other' }, { 'INPUT_BUILD-DIRECTORY': 'target/other' }]) {
    // Arrange
    const current = fixture();
    if (changes !== null) run(current.env, current.io);
    current.files.set('/fixture/target/nextest/ci/junit.xml', 'old XML');
    current.files.set('/fixture/target/test-results/workspace/junit.xml', 'old copy');
    // Act
    const result = current.finish({ ...changes, INPUT_OUTCOME: 'failure' });
    // Assert
    assert.equal(result.prepared, false);
    assert.equal(result.hook_seconds, null);
    assert.equal(result.outcome, 'failure');
    assert.deepEqual(result.reports, { 'junit.xml': null, 'cargo-timing.html': null });
    assert.equal(current.files.has('/fixture/target/test-results/workspace/junit.xml'), false);
  }
  // Arrange / Act
  const current = fixture();
  run(current.env, current.io);
  const result = current.finish({ INPUT_OUTCOME: '' });
  // Assert
  assert.equal(result.outcome, 'unknown');
  assert.equal(result.prepared, true);
  assert.equal(result.reports['junit.xml'], null);
  assert.equal(result.environment.exact_cache_hit, null);
});

test('invalid inputs and corrupt preparation fail visibly', () => {
  // Arrange / Act / Assert
  for (const changes of [
    { INPUT_PHASE: 'bad' }, { INPUT_SUITE: '../bad' }, { INPUT_SUITE: undefined },
    { INPUT_SUITE: '.' }, { INPUT_SUITE: '..' },
    { GITHUB_WORKSPACE: '' }, { GITHUB_SHA: '' },
    ...['/tmp', '../tmp', 'target/../tmp', 'target/./tmp', 'target\n'].map(value => ({ 'INPUT_BUILD-DIRECTORY': value })),
  ]) assert.throws(() => run({ ...base, ...changes }, fixture().io));
  const current = fixture({ 'INPUT_EXACT-CACHE-HIT': 'true' });
  const directory = run(current.env, current.io);
  assert.equal(JSON.parse(current.files.get(`${directory}/start.json`)).environment.exact_cache_hit, true);
  current.files.set(`${directory}/start.json`, '{');
  assert.throws(() => current.finish(), SyntaxError);
  run(current.env, current.io);
  const start = JSON.parse(current.files.get(`${directory}/start.json`));
  start.started_at = '2027-01-01';
  current.files.set(`${directory}/start.json`, JSON.stringify(start));
  assert.throws(() => current.finish(), /Invalid timing interval/);
  start.started_at = 'invalid';
  current.files.set(`${directory}/start.json`, JSON.stringify(start));
  assert.throws(() => current.finish(), /Invalid timing interval/);
});

test('real adapter and action entry preserve reports and propagate failures', t => {
  // Arrange
  const directory = fs.mkdtempSync(resolve(tmpdir(), 'agentty-timing-'));
  t.after(() => fs.rmSync(directory, { force: true, recursive: true }));
  const env = { ...process.env, ...base, GITHUB_WORKSPACE: directory, GITHUB_OUTPUT: resolve(directory, 'output') };
  const entry = resolve(import.meta.dirname, 'index.mjs');
  // Act
  const prepared = spawnSync(process.execPath, [entry], { env, encoding: 'utf8' });
  fs.mkdirSync(resolve(directory, 'target/nextest/ci'), { recursive: true });
  fs.writeFileSync(resolve(directory, 'target/nextest/ci/junit.xml'), '<testsuites/>');
  const finished = spawnSync(process.execPath, [entry], { env: { ...env, INPUT_PHASE: 'finish', INPUT_OUTCOME: 'failure' }, encoding: 'utf8' });
  const failed = spawnSync(process.execPath, [entry], { env: { ...env, INPUT_PHASE: 'invalid' }, encoding: 'utf8' });
  // Assert
  assert.equal(prepared.status, 0, prepared.stderr);
  assert.equal(finished.status, 0, finished.stderr);
  assert.equal(failed.status, 1);
  assert.match(failed.stderr, /Expected prepare or finish/);
  const result = JSON.parse(fs.readFileSync(resolve(directory, 'target/test-results/workspace/metadata.json')));
  assert.equal(result.outcome, 'failure');
  assert.ok(result.hook_seconds >= 0);
  assert.ok(result.environment.available_parallelism > 0);
  assert.equal(fs.readFileSync(resolve(directory, 'target/test-results/workspace/junit.xml'), 'utf8'), '<testsuites/>');
  assert.equal(host.version(process.execPath, ['--version']), process.version);
  assert.deepEqual(host.hardware([{ model: 'Z' }, { model: 'A' }, { model: 'Z' }], 2),
    { cpu_models: ['A', 'Z'], available_parallelism: 2 });
  assert.equal(host.version('/nonexistent-agentty-program', []), null);
  assert.equal(host.read(resolve(directory, 'missing')), null);
  assert.throws(() => host.read(directory));
});
