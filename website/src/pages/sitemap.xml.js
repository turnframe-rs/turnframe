// Every page, with the day it last changed: its source's last commit, and for the home page the
// latest of the site's own and the benchmarks it prints.
import { load } from '../lib/docs.mjs';
import { SITE, lastChanged } from '../lib/repo.mjs';

export async function GET({ site }) {
  const pages = await load();
  const home = [lastChanged('.', SITE), lastChanged('docs/benchmarks.md'), lastChanged('README.md')].filter(Boolean).sort().at(-1);
  const entries = [{ url: '/', modified: home }, ...pages.map((page) => ({ url: page.url, modified: page.updated }))];
  const body = `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${entries
  .map(({ url, modified }) => `  <url><loc>${new URL(url, site).href}</loc>${modified ? `<lastmod>${modified}</lastmod>` : ''}</url>`)
  .join('\n')}
</urlset>
`;
  return new Response(body, { headers: { 'Content-Type': 'application/xml; charset=utf-8' } });
}
