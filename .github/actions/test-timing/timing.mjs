import * as fs from 'node:fs';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { posix, resolve } from 'node:path';

// Explicit boundary: orchestration tests use an in-memory filesystem and clock.
export const host = {
  read(path) {
    try {
      return fs.readFileSync(path);
    } catch (error) {
      if (error.code === 'ENOENT') return null;
      throw error;
    }
  },
  write: (path, value) => fs.writeFileSync(path, value),
  append: (path, value) => fs.appendFileSync(path, value),
  remove: path => fs.rmSync(path, { force: true, recursive: true }),
  mkdir: path => fs.mkdirSync(path, { recursive: true }),
  now: () => new Date().toISOString(),
  version(program, args) {
    const result = spawnSync(program, args, { encoding: 'utf8' });
    return result.status === 0 ? result.stdout.trim() : null;
  },
};

function identity(env, suite) {
  const result = { suite };
  for (const [field, variable] of Object.entries({
    run_id: 'GITHUB_RUN_ID', attempt: 'GITHUB_RUN_ATTEMPT',
    job: 'GITHUB_JOB', sha: 'GITHUB_SHA',
  })) {
    if (!env[variable]) throw new Error(`Missing ${variable}`);
    result[field] = env[variable];
  }
  return result;
}

function environment(env, io) {
  return {
    runner_os: env.RUNNER_OS ?? null,
    runner_arch: env.RUNNER_ARCH ?? null,
    image: env.ImageOS ?? null,
    image_version: env.ImageVersion ?? null,
    container: env.INPUT_CONTAINER || null,
    rustc: io.version('rustc', ['--version', '--verbose']),
    nextest: io.version('cargo', ['nextest', '--version']),
    exact_cache_hit: ['true', 'false'].includes(env['INPUT_EXACT-CACHE-HIT'])
      ? env['INPUT_EXACT-CACHE-HIT'] === 'true' : null,
  };
}

function relativePath(value) {
  if (!/^[a-zA-Z0-9_./-]+$/.test(value) || posix.isAbsolute(value)
      || value.split('/').some(part => part === '..' || part === '.')) {
    throw new Error(`Unsafe report path: ${value}`);
  }
  return value;
}

// Also callable from the postsubmit loop: no extra Cargo build before each hook.
export function run(env, io = host) {
  const phase = env.INPUT_PHASE;
  if (!['prepare', 'finish'].includes(phase)) throw new Error('Expected prepare or finish');
  const suite = env.INPUT_SUITE;
  if (!/^[a-zA-Z0-9_-]+$/.test(suite ?? '')) throw new Error('Invalid suite identifier');
  if (!env.GITHUB_WORKSPACE) throw new Error('Missing GITHUB_WORKSPACE');
  const current = identity(env, suite);
  const build = relativePath(env['INPUT_BUILD-DIRECTORY'] || 'target');
  const directory = resolve(env.GITHUB_WORKSPACE, 'target/test-results', suite);
  const reports = {
    'junit.xml': resolve(env.GITHUB_WORKSPACE, 'target/nextest/ci/junit.xml'),
    'cargo-timing.html': resolve(env.GITHUB_WORKSPACE, build, 'cargo-timings/cargo-timing.html'),
  };
  const startPath = `${directory}/start.json`;
  const timestamp = io.now();
  if (phase === 'prepare') {
    io.remove(directory);
    io.mkdir(directory);
    for (const path of Object.values(reports)) io.remove(path);
    io.write(startPath, JSON.stringify({
      schema: 1, identity: current, environment: environment(env, io),
      started_at: io.now(), build_directory: build,
    }));
  } else {
    const raw = io.read(startPath);
    const start = raw === null ? null : JSON.parse(raw.toString());
    const prepared = start !== null
      && JSON.stringify(start.identity) === JSON.stringify(current)
      && start.build_directory === build;
    io.remove(startPath);
    const result = {
      schema: 1, identity: current,
      environment: prepared ? start.environment : environment(env, io),
      started_at: prepared ? start.started_at : null,
      finished_at: timestamp,
      hook_seconds: prepared ? (Date.parse(timestamp) - Date.parse(start.started_at)) / 1000 : null,
      outcome: env.INPUT_OUTCOME || 'unknown',
      prepared,
      reports: {},
    };
    if (result.hook_seconds !== null && !(result.hook_seconds >= 0)) {
      throw new Error('Invalid timing interval');
    }
    io.mkdir(directory);
    for (const [name, path] of Object.entries(reports)) {
      const data = prepared ? io.read(path) : null;
      result.reports[name] = data === null ? null : createHash('sha256').update(data).digest('hex');
      const destination = `${directory}/${name}`;
      if (data === null) io.remove(destination);
      else io.write(destination, data);
    }
    io.write(`${directory}/metadata.json`, JSON.stringify(result, null, 2) + '\n');
  }
  if (env.GITHUB_OUTPUT) io.append(env.GITHUB_OUTPUT, `directory=${directory}\n`);
  return directory;
}
