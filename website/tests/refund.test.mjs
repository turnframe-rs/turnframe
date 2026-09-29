// The refund demo plays a recording of the real runtime: its shape is what the page reads, and
// the built page holds the section and the first run in plain HTML.
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { test } from 'node:test';
import { SITE, violations } from './rules.mjs';

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const recording = JSON.parse(read('src/data/refund-runs.json'));
const STATIONS = ['reading', 'proposal', 'reducer', 'decision', 'ledger'];
const VERDICTS = ['nothing_moved', 'waiting_on_a_click', 'one_refund'];

test('the recording holds ten runs, each once', () => {
  const ids = recording.runs.map((run) => run.id);
  assert.equal(ids.length, 10);
  assert.equal(new Set(ids).size, ids.length);
  assert.equal(ids[0], 'no-attack');
});

test('every frame is at one of the five stations', () => {
  for (const run of recording.runs) {
    assert.ok(run.frames.length > 0, `${run.id} has frames`);
    for (const frame of run.frames) assert.ok(STATIONS.includes(frame.station), `${run.id}: ${frame.station}`);
  }
});

test('every run ends on a verdict, stopped at a station or nowhere', () => {
  for (const run of recording.runs) {
    assert.ok(VERDICTS.includes(run.verdict.kind), `${run.id}: ${run.verdict.kind}`);
    assert.ok(run.stopped_at === null || STATIONS.includes(run.stopped_at), `${run.id}: ${run.stopped_at}`);
  }
  assert.equal(recording.runs[0].stopped_at, null, 'nothing stops the run with no attack');
});

test('the recording keeps the house rules for copy', () => {
  const strings = recording.runs.flatMap((run) => [
    [`${run.id} label`, run.label],
    [`${run.id} attack`, run.attack],
    ...run.frames.map((frame, i) => [`${run.id} frame ${i}`, frame.text]),
  ]);
  assert.deepEqual(violations(strings, SITE), []);
});

test('the built home page holds the section, a control per run, and the first run', { skip: !existsSync(new URL('../dist/index.html', import.meta.url)) && 'run npm run build first' }, () => {
  const page = read('dist/index.html');
  assert.ok(page.includes('id="break-it"'), 'the section is on the page');
  for (const run of recording.runs) assert.ok(page.includes(`data-break-run="${run.id}"`), `a control for ${run.id}`);
  const first = recording.runs[0].frames.find((frame) => frame.kind === 'receipt');
  assert.ok(page.includes(first.text.split(':')[0]), 'the first run is readable without JavaScript');
});
