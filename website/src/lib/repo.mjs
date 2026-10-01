// Facts the site prints, read from the repository it is built from, so a page cannot drift
// from the code: the facade's version, MSRV and links from the workspace manifest, the crate family
// from the README, the features from the facade, and every measured figure from
// docs/benchmarks.md. A source whose shape changed fails the build with the file to look at.
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse as parseToml } from 'smol-toml';

/** The site's own directory. Builds run from it (npm scripts do), and bundling moves this module. */
export const SITE = process.cwd();
if (!existsSync(resolve(SITE, 'astro.config.mjs'))) {
  throw new Error(`turnframe.rs builds from its own directory, and ${SITE} is not it`);
}

/** The repository root: the parent of the site, or TURNFRAME_ROOT to build against another checkout. */
export const ROOT = resolve(process.env.TURNFRAME_ROOT ?? resolve(SITE, '..'));

export function read(path) {
  const file = resolve(ROOT, path);
  if (!existsSync(file)) throw new Error(`turnframe.rs reads ${file}, which does not exist`);
  return readFileSync(file, 'utf8');
}

function fail(path, what) {
  throw new Error(`turnframe.rs could not read ${what} from ${path}: its shape changed`);
}

const workspace = parseToml(read('Cargo.toml'));

export const REPO = workspace.workspace.package.repository;
// The facade's version is the one applications see; a release may leave other crates behind it.
const facade = parseToml(read('crates/turnframe/Cargo.toml')).package.version;
export const VERSION = typeof facade === 'string' ? facade : workspace.workspace.package.version;
export const MSRV = workspace.workspace.package['rust-version'];
export const LICENSE = workspace.workspace.package.license;
export const KEYWORDS = workspace.workspace.package.keywords ?? [];
export const BRANCH = 'main';

/** The rows of the first Markdown table after `heading` whose header satisfies `accept`. */
export function table(markdown, heading, accept = () => true) {
  const lines = markdown.split('\n');
  let at = heading ? lines.findIndex((line) => line.trim() === heading) : 0;
  if (at < 0) return null;
  for (; at < lines.length; at += 1) {
    if (!lines[at].startsWith('|') || !/^\|[\s:|-]+\|$/.test(lines[at + 1]?.trim() ?? '')) continue;
    const cells = (line) => line.trim().replace(/^\||\|$/g, '').split('|').map((cell) => cell.trim());
    const header = cells(lines[at]);
    if (!accept(header)) continue;
    const rows = [];
    for (let row = at + 2; row < lines.length && lines[row].startsWith('|'); row += 1) rows.push(cells(lines[row]));
    return { header, rows };
  }
  return null;
}

const plain = (cell) => cell.replace(/`/g, '');

/** The day `path` last changed in `repo`'s history (YYYY-MM-DD), or null outside a git checkout. */
export function lastChanged(path, repo = ROOT) {
  try {
    const day = execFileSync('git', ['-C', repo, 'log', '-1', '--format=%cs', '--', path], { encoding: 'utf8' }).trim();
    return day || null;
  } catch {
    return null;
  }
}

/** The crate family, as the README's table lists it, grouped for the loadout table. */
export function crates() {
  const readme = read('README.md');
  const found = table(readme, '## Crate family', (header) => header[0] === 'Crate');
  if (!found) fail('README.md', 'the crate table under “## Crate family”');
  const facade = parseToml(read('crates/turnframe/Cargo.toml'));
  const optional = Object.entries(facade.features ?? {});
  const enabledBy = (name) =>
    optional
      .filter(([, list]) => list.some((entry) => entry === `dep:${name}` || entry.startsWith(`${name}/`)))
      .map(([feature]) => feature);
  const rows = found.rows.map(([crate, role]) => {
    const name = plain(crate);
    const features = name === 'turnframe' ? [] : enabledBy(name);
    const required = name === 'turnframe' || Object.hasOwn(facade.dependencies, name) && !facade.dependencies[name].optional;
    return { name, role: plain(role), features: features.join(', ') || (required ? 'included' : '') };
  });
  const members = workspace.workspace.members.filter((path) => path.startsWith('crates/'));
  for (const path of members) {
    const name = parseToml(read(`${path}/Cargo.toml`)).package.name;
    if (!rows.some((row) => row.name === name)) {
      throw new Error(`${name} is a workspace crate the README's crate table does not list`);
    }
  }
  const group = (name) =>
    name.startsWith('turnframe-provider') ? 'Providers'
    : /^turnframe-(store|prompt)/.test(name) ? 'Persistence and prompts'
    : /^turnframe-(test|eval|telemetry)$/.test(name) ? 'Proof'
    : name === 'turnframe-macros' ? null
    : 'Core';
  const order = ['Core', 'Providers', 'Persistence and prompts', 'Proof'];
  return order.map((title) => [
    title,
    rows.filter((row) => group(row.name) === title).map((row) => [row.name, row.role, row.features]),
  ]);
}

/** The facade's feature flags, from the table in its crate documentation. */
export function features() {
  const doc = read('crates/turnframe/src/lib.rs')
    .split('\n')
    .filter((line) => line.startsWith('//!'))
    .map((line) => line.replace(/^\/\/! ?/, ''))
    .join('\n');
  const found = table(doc, null, (header) => header[0] === 'Feature');
  if (!found) fail('crates/turnframe/src/lib.rs', 'the feature table');
  // rustdoc's intra-doc links, [`module`], become links the site sends to docs.rs.
  const intra = (text) => text.replace(/\[`([\w:]+)`\](?!\()/g, '[`$1`](crate::$1)');
  return found.rows.map(([feature, what]) => ({ feature: plain(feature), what: intra(what) }));
}

/** The live corpus as docs/benchmarks.md last measured it: the latest run, and what was not run. */
export function corpusRun() {
  const path = 'docs/benchmarks.md';
  const heading = '## The corpus against a real model';
  const doc = read(path);
  const start = doc.indexOf(heading);
  if (start < 0) fail(path, `the section “${heading}”`);
  const next = doc.indexOf('\n## ', start + heading.length);
  const section = doc.slice(start, next < 0 ? undefined : next);
  const found = table(section, heading, (header) => header[0] === 'Run' && header.includes('Samples passed'));
  if (!found) fail(path, 'the table of runs');
  const at = (name) => found.header.findIndex((cell) => cell.startsWith(name));
  const text = section.replace(/\s+/g, ' ');
  const items = text.match(/It holds (\d+) items/);
  const measured = text.match(/Measured on (.+?) against \w+'s `([^`]+)` at the default `(\w+)` effort, (\w+) samples per item/);
  const pricing = text.match(/\$([\d.]+) per million input tokens and \$([\d.]+) per million output tokens/);
  if (!items || !measured || !pricing) fail(path, 'the item count, date, model, level or prices of the corpus run');
  const row = found.rows.at(-1);
  const passed = row[at('Samples passed')].match(/^(\d+)\/(\d+) \(([\d.]+%)\)$/);
  const cost = Number(row[at('Cost')].replace('$', ''));
  const calls = Number(row[at('Model calls')].replace(/,/g, ''));
  const [p50] = row[at('Turn p50')].split(' / ');
  if (!passed || Number.isNaN(cost) || Number.isNaN(calls)) fail(path, `the “${row[0]}” row of the runs`);
  const samples = Number(passed[2]);
  const unmeasured = text.match(/What is not measured in this release\.\*\*(.*?)(?:$|\*\*)/)?.[1] ?? '';
  return {
    date: measured[1],
    model: measured[2],
    level: measured[3],
    samplesPerItem: measured[4],
    items: Number(items[1]),
    run: row[0].toLowerCase(),
    runs: found.rows.length,
    passed: `${passed[1]} of ${passed[2]}`,
    rate: passed[3],
    itemsAtFull: row[at('Items at')],
    calls: (calls / samples).toFixed(1),
    cost: `$${(cost / samples).toFixed(3)}`,
    p50,
    pricing: `$${pricing[1]} / $${pricing[2]} per M tokens`,
    notMeasured: ['low', 'medium', 'high'].filter((level) => unmeasured.includes(`\`${level}\``)),
  };
}
