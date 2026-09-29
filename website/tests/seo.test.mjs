// What a search engine reads from each built page: one heading, a title and a description that
// fit a result and are the page's own, a canonical address, structured data that parses, and a
// sitemap that lists exactly the pages there are.
import assert from 'node:assert/strict';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { test } from 'node:test';

const dist = new URL('../dist/', import.meta.url);
const built = existsSync(new URL('index.html', dist));
const skip = !built && 'run npm run build first';
const SITE = 'https://turnframe.rs';

const decode = (text) => text.replace(/&amp;/g, '&').replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&lt;/g, '<').replace(/&gt;/g, '>');
const pages = built
  ? readdirSync(dist, { recursive: true })
      .filter((file) => file.endsWith('.html') && file !== '404.html')
      .map((file) => {
        const html = readFileSync(new URL(file, dist), 'utf8');
        const meta = (name) => decode(html.match(new RegExp(`<meta (?:name|property)="${name}" content="([^"]*)"`))?.[1] ?? '');
        return {
          route: ('/' + file.replace(/(^|\/)index\.html$/, '')).replace(/\/$/, '') || '/',
          title: decode(html.match(/<title>([^<]*)<\/title>/)?.[1] ?? ''),
          description: meta('description'),
          robots: meta('robots'),
          image: meta('og:image'),
          canonical: html.match(/<link rel="canonical" href="([^"]+)"/)?.[1],
          h1: (html.match(/<h1[\s>]/g) ?? []).length,
          schema: [...html.matchAll(/<script type="application\/ld\+json">([\s\S]*?)<\/script>/g)].map((match) => JSON.parse(match[1])),
        };
      })
  : [];

test('every page has one h1, and a title and description that fit a result', { skip }, () => {
  const problems = [];
  for (const page of pages) {
    if (page.h1 !== 1) problems.push(`${page.route}: ${page.h1} h1`);
    const limit = page.route.startsWith('/docs/adr/') ? 100 : 70;
    if (!page.title || page.title.length > limit) problems.push(`${page.route}: title of ${page.title.length} characters`);
    if (page.description.length < 70 || page.description.length > 160) {
      problems.push(`${page.route}: description of ${page.description.length} characters`);
    }
  }
  assert.deepEqual(problems, []);
});

test('no two pages share a title or a description', { skip }, () => {
  for (const field of ['title', 'description']) {
    const seen = new Map();
    for (const page of pages) seen.set(page[field], [...(seen.get(page[field]) ?? []), page.route]);
    assert.deepEqual([...seen.values()].filter((routes) => routes.length > 1), [], `a shared ${field}`);
  }
});

test('every page is indexable at its canonical address, with a social image', { skip }, () => {
  for (const page of pages) {
    assert.equal(page.canonical, SITE + (page.route === '/' ? '/' : page.route), page.route);
    assert.match(page.robots, /^index, follow/, page.route);
    assert.equal(page.image, `${SITE}/og.png`, page.route);
  }
});

test('structured data: the software on the home page, an article and breadcrumbs on each doc', { skip }, () => {
  const home = pages.find((page) => page.route === '/');
  const types = (page) => page.schema.map((object) => object['@type']);
  assert.deepEqual(types(home).sort(), ['SoftwareSourceCode', 'WebSite']);
  assert.equal(home.schema.find((object) => object['@type'] === 'SoftwareSourceCode').programmingLanguage.name, 'Rust');
  for (const page of pages.filter((candidate) => candidate.route.startsWith('/docs'))) {
    assert.deepEqual(types(page).sort(), ['BreadcrumbList', 'TechArticle'], page.route);
    for (const object of page.schema) assert.equal(object['@context'], 'https://schema.org', page.route);
  }
});

test('the sitemap lists every page and nothing else', { skip }, () => {
  const sitemap = readFileSync(new URL('sitemap.xml', dist), 'utf8');
  const listed = [...sitemap.matchAll(/<loc>([^<]+)<\/loc>/g)].map((match) => match[1].replace(SITE, '').replace(/^\/$/, '/')).sort();
  assert.deepEqual(listed, pages.map((page) => page.route).sort());
  assert.match(readFileSync(new URL('robots.txt', dist), 'utf8'), /Sitemap: https:\/\/turnframe\.rs\/sitemap\.xml/);
});
