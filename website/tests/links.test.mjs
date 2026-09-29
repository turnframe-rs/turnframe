// Every link from one page of the site to another lands on a page that was built, and every
// anchor it names exists there. Rendered guides link by repository path; this is what proves
// each of those was rewritten to a page or to GitHub.
import assert from 'node:assert/strict';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { test } from 'node:test';

const dist = new URL('../dist/', import.meta.url);
const built = existsSync(new URL('index.html', dist));

const pages = built
  ? readdirSync(dist, { recursive: true }).filter((file) => file.endsWith('.html')).map((file) => file.split('\\').join('/'))
  : [];
const route = (file) => ('/' + file.replace(/(^|\/)index\.html$/, '').replace(/\.html$/, '')).replace(/\/$/, '') || '/';
const html = new Map(pages.map((file) => [route(file), readFileSync(new URL(file, dist), 'utf8')]));
const ids = (source) => new Set([...source.matchAll(/\sid="([^"]+)"/g)].map((match) => match[1]));

test('internal links and anchors resolve', { skip: !built && 'run npm run build first' }, () => {
  const broken = [];
  for (const [from, source] of html) {
    for (const [, href] of source.matchAll(/\shref="([^"]+)"/g)) {
      if (/^(https?:|mailto:)/.test(href)) continue;
      const [path, hash] = href.split('#');
      const target = path === '' ? from : path.replace(/\/$/, '') || '/';
      const page = html.get(target);
      const file = existsSync(new URL(`.${target}`, dist));
      if (!page && !file) broken.push(`${from} → ${href}`);
      else if (hash && page && !ids(page).has(decodeURIComponent(hash))) broken.push(`${from} → ${href} (no such anchor)`);
    }
  }
  assert.deepEqual(broken, []);
});

test('links to the repository point at files that exist in it', { skip: !built && 'run npm run build first' }, async () => {
  process.chdir(new URL('..', import.meta.url).pathname);
  const { REPO, ROOT, SITE } = await import('../src/lib/repo.mjs');
  // The site's own files are checked where the site is, which is not ROOT when it is built
  // against another checkout.
  const where = (path) => (path.startsWith('website/') ? `${SITE}/${path.slice('website/'.length)}` : `${ROOT}/${path}`);
  const missing = new Set();
  for (const [from, source] of html) {
    for (const [, kind, path] of source.matchAll(new RegExp(`href="${REPO}/(blob|tree|edit)/main/([^"#]+)`, 'g'))) {
      if (!existsSync(where(decodeURIComponent(path)))) missing.add(`${from} → ${kind}/${path}`);
    }
  }
  assert.deepEqual([...missing], []);
});
