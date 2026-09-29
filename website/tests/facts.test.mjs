// The figures and names the site prints come from the repository it is built from.
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { test } from 'node:test';

process.chdir(new URL('..', import.meta.url).pathname);
const { ROOT, corpusRun, crates, features } = await import('../src/lib/repo.mjs');

test('the corpus run is read whole from docs/benchmarks.md', () => {
  const run = corpusRun();
  assert.match(run.rate, /^\d+(\.\d+)?%$/);
  assert.match(run.passed, /^\d+ of \d+$/);
  assert.match(run.cost, /^\$\d+\.\d{3}$/);
  assert.match(run.calls, /^\d+\.\d$/);
  assert.match(run.p50, /^\d+(\.\d+)? s$/);
  assert.ok(['low', 'medium', 'high'].includes(run.level), `the measured level is ${run.level}`);
  const benchmarks = readFileSync(`${ROOT}/docs/benchmarks.md`, 'utf8');
  assert.ok(benchmarks.includes(run.model) && benchmarks.includes(run.date));
});

test('the crate table lists every crate of the workspace, each once', () => {
  const listed = crates().flatMap(([, rows]) => rows.map(([name]) => name));
  assert.equal(new Set(listed).size, listed.length);
  assert.ok(listed.includes('turnframe') && listed.includes('turnframe-runtime'));
});

test('the feature table names every feature of the facade', () => {
  const manifest = readFileSync(`${ROOT}/crates/turnframe/Cargo.toml`, 'utf8');
  const declared = [...manifest.split('[features]')[1].split(/^\[/m)[0].matchAll(/^([a-z-]+)\s*=/gm)].map((match) => match[1]).filter((name) => name !== 'default');
  assert.deepEqual(features().map((row) => row.feature).sort(), declared.sort());
});

test('the replay names only operations, events and notices the library has', () => {
  const scenarios = readFileSync('src/data/scenarios.js', 'utf8');
  const named = new Set([...scenarios.matchAll(/\b((?:trip|traveler|turnframe\.notice)\.[a-z_]+)\b/g)].map((match) => match[1]));
  const sources = ['crates/turnframe-test/src', 'crates/turnframe-core/src', 'crates/turnframe-runtime/src']
    .flatMap((dir) => readdirSync(`${ROOT}/${dir}`, { recursive: true }).filter((file) => file.endsWith('.rs')).map((file) => readFileSync(`${ROOT}/${dir}/${file}`, 'utf8')))
    .join('\n');
  const unknown = [...named].filter((name) => !sources.includes(`"${name}"`));
  assert.deepEqual(unknown, [], 'the replay must not invent an API: these are not in the sample domain or the runtime');
});
