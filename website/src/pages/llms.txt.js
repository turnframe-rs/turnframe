// llms.txt: the documentation as an index a language model can follow, one line per page.
import { NAV, load } from '../lib/docs.mjs';
import { REPO } from '../lib/repo.mjs';

export async function GET({ site }) {
  const pages = await load();
  const lines = [
    '# Turnframe',
    '',
    '> Deterministic conversational workflows for Rust. Small model tasks propose what a message means; deterministic reducers decide effects; committed events decide what the reply may claim.',
    '',
    `Source: ${REPO}. Every page below is also available as one file at ${new URL('/llms-full.txt', site)}.`,
  ];
  for (const section of NAV) {
    lines.push('', `## ${section.title}`, '');
    for (const page of pages.filter((candidate) => candidate.section === section.title)) {
      lines.push(`- [${page.title}](${new URL(page.url, site)})${page.description ? `: ${page.description}` : ''}`);
    }
  }
  return new Response(`${lines.join('\n')}\n`, { headers: { 'Content-Type': 'text/plain; charset=utf-8' } });
}
