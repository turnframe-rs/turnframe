// The documentation: which pages exist, in which order, and where each one's Markdown comes
// from. Most are the repository's own guides, rendered from the checkout the site is built
// from, so the two cannot disagree. The few under src/content are the site's own: what this
// is, how to install it, what to run first.
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, normalize, posix, resolve } from 'node:path';

import { DOCS } from '../data/search.js';
import { BRANCH, MSRV, REPO, ROOT, SITE, crates, features, lastChanged, read } from './repo.mjs';
import { ready, render } from './markdown.mjs';

const CONTENT = resolve(SITE, 'src/content');

const ADRS = readdirSync(resolve(ROOT, 'docs/adr'))
  .filter((file) => /^ADR-\d+.*\.md$/.test(file))
  .sort()
  .map((file) => ({
    slug: `adr/${file.match(/^ADR-(\d+)/)[1]}`,
    source: `docs/adr/${file}`,
    adr: file.match(/^ADR-(\d+)/)[1],
  }));

export const NAV = [
  {
    title: 'Getting started',
    pages: [
      { slug: '', label: 'Introduction', local: 'introduction.md' },
      { slug: 'installation', label: 'Installation', local: 'installation.md' },
      { slug: 'quickstart', label: 'Quickstart', source: 'crates/turnframe/src/quickstart.md', title: 'Quickstart: a minimal turn' },
      { slug: 'examples', label: 'Examples', local: 'examples.md' },
    ],
  },
  {
    title: 'Concepts',
    pages: [
      { slug: 'flow-map', label: 'The Flow Map', source: 'docs/flow-map.md' },
      { slug: 'architecture', label: 'Architecture', source: 'docs/architecture.md' },
      { slug: 'reliability-model', label: 'Reliability model', source: 'docs/reliability-model.md' },
      { slug: 'interactions', label: 'Persistent interactions', source: 'docs/interactions.md' },
      { slug: 'composition', label: 'What the reply may say', source: 'docs/composition.md' },
    ],
  },
  {
    title: 'Guides',
    pages: [
      { slug: 'provider-adapters', label: 'Provider adapters', source: 'docs/provider-adapters.md' },
      { slug: 'persistence', label: 'Persistence', source: 'docs/persistence.md' },
      { slug: 'revision-migration', label: 'Adding a revision column', source: 'crates/turnframe-store-postgres/docs/revision-migration.md' },
      { slug: 'telemetry', label: 'Telemetry', source: 'docs/telemetry.md' },
      { slug: 'evaluation', label: 'Evaluation', source: 'docs/evaluation.md' },
      { slug: 'recipes', label: 'Recipes', source: 'docs/recipes.md' },
      { slug: 'consent-and-acceptance', label: 'Consent and acceptance', source: 'docs/consent-and-acceptance.md' },
      { slug: 'canary-and-rollback', label: 'Canary and rollback', source: 'docs/canary-and-rollback.md' },
    ],
  },
  {
    title: 'Security',
    pages: [
      { slug: 'threat-model', label: 'Threat model', source: 'docs/threat-model.md' },
      { slug: 'security-policy', label: 'Reporting a vulnerability', source: 'SECURITY.md' },
    ],
  },
  {
    title: 'Project',
    pages: [
      { slug: 'benchmarks', label: 'Benchmarks', source: 'docs/benchmarks.md' },
      { slug: 'roadmap', label: 'Roadmap', source: 'docs/roadmap.md' },
      { slug: 'release-checklist', label: 'Release checklist', source: 'docs/release-checklist.md' },
      { slug: 'changelog', label: 'Changelog', source: 'CHANGELOG.md' },
      { slug: 'contributing', label: 'Contributing', source: 'CONTRIBUTING.md' },
    ],
  },
  {
    title: 'Decision records',
    collapsed: true,
    pages: [{ slug: 'adr', label: 'Index', source: 'docs/adr/README.md' }, ...ADRS],
  },
  {
    title: 'Reference',
    pages: [{ slug: 'api', label: 'API reference', local: 'api.md' }],
  },
];

export const PAGES = NAV.flatMap((section) => section.pages.map((page) => ({ ...page, section: section.title })));

export const url = (slug) => (slug ? `/docs/${slug}` : '/docs');

const bySource = new Map(PAGES.filter((page) => page.source).map((page) => [page.source, page]));

/** Where a link written in `from` (a repository path, or '' for the site's own pages) goes. */
export function linkFrom(from) {
  return (href, { raw } = {}) => {
    if (!href || /^(https?:|mailto:|#|\/)/.test(href)) return href;
    const intra = href.match(/^(?:crate::)([\w:]+)$/);
    if (intra) return `https://docs.rs/turnframe/latest/turnframe/${intra[1].split('::').join('/')}/`;
    const [path, hash] = href.split('#');
    const target = normalize(join(dirname(from || '.'), path)).split('\\').join('/');
    if (target.startsWith('..')) return href;
    const page = bySource.get(target);
    if (page) return url(page.slug) + (hash ? `#${hash}` : '');
    const onDisk = resolve(ROOT, target);
    const kind = existsSync(onDisk) && statSync(onDisk).isDirectory() ? 'tree' : raw ? 'raw' : 'blob';
    return `${REPO}/${kind}/${BRANCH}/${posix.normalize(target)}${hash ? `#${hash}` : ''}`;
  };
}

// The site's own pages may print facts the repository holds.
const FACTS = {
  repo: () => REPO,
  msrv: () => MSRV,
  features: () =>
    ['| Feature | What it turns on |', '| --- | --- |', ...features().map((row) => `| \`${row.feature}\` | ${row.what} |`)].join('\n'),
  crates: () =>
    [
      '| Crate | Role | Feature |',
      '| --- | --- | --- |',
      ...crates().flatMap(([, rows]) =>
        rows.map(([name, role, feature]) => `| \`${name}\` | ${role} | ${feature === 'included' ? 'included' : `\`${feature}\``} |`),
      ),
    ].join('\n'),
};

const withFacts = (markdown) =>
  markdown.replace(/\{\{(\w+)\}\}/g, (all, name) => {
    if (!FACTS[name]) throw new Error(`src/content names an unknown fact {{${name}}}`);
    return FACTS[name]();
  });

let loaded;

/** Every page, rendered: title, sidebar label, description, table of contents, HTML. */
export function load() {
  loaded ??= ready().then(() => PAGES.map(renderPage));
  return loaded;
}

// A decision record is described by its number, status and title, since its first paragraph is
// a list of the records it amends.
function adrSearch(page, heading, markdown) {
  const status = markdown.match(/Status:\s*(\w+)\s*\((\d{4}-\d{2}-\d{2})\)/);
  const lead = `Turnframe decision record ${page.adr}${status ? `, ${status[1].toLowerCase()} ${status[2]}` : ''}: ${heading}.`;
  const tail = [' Its context, the decision, its consequences and how it is enforced.', ' Its context, decision and enforcement.', '']
    .find((candidate) => lead.length + candidate.length <= 160);
  return { title: `ADR-${page.adr}: ${heading}`, description: lead + tail };
}

// A title tag carries the site's name when it fits in what a result shows.
const branded = (title) => (/Turnframe/.test(title) ? title : `${title} · Turnframe docs`.length <= 65 ? `${title} · Turnframe docs` : title);

function renderPage(page, index) {
  const markdown = page.local ? withFacts(readFileSync(join(CONTENT, page.local), 'utf8')) : read(page.source);
  const { title, description, headings, html } = render(markdown, { link: linkFrom(page.source ?? '') });
  const adrTitle = page.adr && title?.replace(/^ADR-\d+:?\s*/, '');
  const heading = page.title ?? adrTitle ?? title ?? page.label;
  const search = page.adr ? adrSearch(page, adrTitle, markdown) : DOCS[page.slug];
  if (!search) throw new Error(`src/data/search.js has no title and description for /docs/${page.slug}`);
  return {
    ...page,
    index,
    url: url(page.slug),
    title: heading,
    label: page.label ?? `${page.adr} · ${adrTitle}`,
    metaTitle: branded(search.title),
    description: search.description ?? description,
    updated: page.local ? lastChanged(`src/content/${page.local}`, SITE) : lastChanged(page.source),
    headings,
    html,
    edit: `${REPO}/edit/${BRANCH}/${page.source ?? `website/src/content/${page.local}`}`,
    markdown,
  };
}
