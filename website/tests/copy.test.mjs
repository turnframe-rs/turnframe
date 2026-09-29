// The site's own copy follows the house rules. The repository's guides, which the site renders,
// answer to the repository; of those rules only the em dash is checked on every built page.
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';

import { EM_DASH, SITE, violations } from './rules.mjs';

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const files = (dir, ext) =>
  readdirSync(new URL(`../${dir}`, import.meta.url), { recursive: true })
    .filter((file) => file.endsWith(ext))
    .map((file) => join(dir, file));

// Prose in Markdown, one paragraph at a time, code excluded.
function markdownProse(path) {
  return read(path)
    .replace(/```[\s\S]*?```/g, '')
    .split(/\n\s*\n/)
    .map((paragraph) => [path, paragraph.replace(/`[^`]*`/g, '').replace(/\s+/g, ' ').trim()])
    .filter(([, text]) => text);
}

// Quoted strings in source: the copy a component or page renders.
function sourceStrings(path) {
  const text = read(path);
  const strings = [...text.matchAll(/(["'`])((?:\\.|(?!\1)[^\\\n])*)\1/g)].map((match) => match[2]);
  const jsx = [...text.matchAll(/>([^<>{}]*[a-z][^<>{}]*)</g)].map((match) => match[1]);
  return [...strings, ...jsx].map((value) => [path, value.replace(/\s+/g, ' ').trim()]).filter(([, value]) => /[a-z] [a-z]/i.test(value));
}

test('the site’s own pages follow the house rules', () => {
  const strings = [
    ...files('src/content', '.md').flatMap(markdownProse),
    ...files('src/pages', '.astro').flatMap(sourceStrings),
    ...files('src/components', '.astro').flatMap(sourceStrings),
    ...sourceStrings('src/data/scenarios.js'),
    ...sourceStrings('src/data/search.js'),
    ...sourceStrings('src/ds/turnframe.js'),
  ];
  assert.ok(strings.length > 200, `only ${strings.length} strings were found: the extraction broke`);
  assert.deepEqual(violations(strings, SITE), []);
});

test('no built page carries an em dash', { skip: !exists('dist/index.html') && 'run npm run build first' }, () => {
  const pages = files('dist', '.html').map((path) => [path, visibleText(read(path))]);
  assert.deepEqual(violations(pages, [EM_DASH]), []);
});

function exists(path) {
  try {
    readFileSync(new URL(`../${path}`, import.meta.url));
    return true;
  } catch {
    return false;
  }
}

function visibleText(html) {
  return html
    .replace(/<script[\s\S]*?<\/script>/g, '')
    .replace(/<style[\s\S]*?<\/style>/g, '')
    .replace(/<[^>]+>/g, ' ');
}
