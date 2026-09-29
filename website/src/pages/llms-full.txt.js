// Every documentation page as Markdown, in reading order, in one file.
import { load } from '../lib/docs.mjs';

export async function GET({ site }) {
  const pages = await load();
  const body = pages
    .map((page) => `<!-- ${new URL(page.url, site)} -->\n\n${page.markdown.trim()}\n`)
    .join('\n\n---\n\n');
  return new Response(body, { headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
}
