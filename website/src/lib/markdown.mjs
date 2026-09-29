// Renders a Markdown document from the repository into the site's HTML: headings with GitHub's
// anchors, code in the design system's code block, tables that scroll on a phone, and every
// relative link sent to the page that renders its target, or to the file on GitHub.
import GithubSlugger from 'github-slugger';
import MarkdownIt from 'markdown-it';
import { createHighlighter } from 'shiki';

const syntax = (scope, token, fontStyle) => ({
  scope,
  settings: { foreground: `var(--${token})`, ...(fontStyle ? { fontStyle } : {}) },
});

// Colours are the design system's syntax tokens, so a block follows the page's theme.
const THEME = {
  name: 'turnframe',
  type: 'dark',
  colors: { 'editor.foreground': 'var(--ink)', 'editor.background': 'var(--surface-sunken)' },
  tokenColors: [
    syntax(['keyword', 'storage', 'variable.language.self', 'support.type.property-name'], 'syn-keyword'),
    syntax(['keyword.operator', 'punctuation', 'meta.brace'], 'ink-muted'),
    syntax(['string', 'string.quoted', 'punctuation.definition.string'], 'syn-string'),
    syntax(['constant.numeric', 'constant.language', 'constant.other', 'entity.name.type.lifetime', 'punctuation.definition.lifetime', 'variable.parameter.option'], 'syn-number'),
    syntax(['entity.name.type', 'entity.name.class', 'entity.name.namespace', 'support.type', 'entity.name.tag', 'support.class'], 'syn-type'),
    syntax(['entity.name.function', 'support.function', 'meta.function-call.generic'], 'syn-fn'),
    syntax(['entity.name.function.macro', 'meta.attribute', 'punctuation.definition.attribute', 'support.macro'], 'syn-macro'),
    syntax(['comment', 'punctuation.definition.comment'], 'syn-comment', 'italic'),
    syntax(['variable.other.key', 'entity.name.command'], 'syn-keyword'),
    syntax(['entity.name.section'], 'syn-type'),
    syntax(['string.unquoted.argument'], 'ink'),
  ],
};

const LANGS = ['rust', 'toml', 'shellscript', 'json', 'sql', 'yaml', 'diff'];
const ALIAS = { sh: 'shellscript', shell: 'shellscript', bash: 'shellscript', console: 'shellscript', rs: 'rust' };

let highlighter;
export async function ready() {
  highlighter ??= await createHighlighter({ themes: [THEME], langs: LANGS });
  return highlighter;
}

const escape = (text) =>
  text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

// rustdoc hides lines that start with `# ` in a Rust block; the site shows what rustdoc shows.
function rustdocVisible(code) {
  return code
    .split('\n')
    .filter((line) => !/^\s*#(\s|$)/.test(line))
    .map((line) => line.replace(/^(\s*)##/, '$1#'))
    .join('\n');
}

/** One code block, as the design system's CodeBlock draws it, highlighted at build time. */
export function codeBlock(code, info = '', { caption } = {}) {
  const lang = (info.trim().split(/[\s,]+/)[0] || 'text').toLowerCase();
  const grammar = ALIAS[lang] ?? lang;
  const shown = (grammar === 'rust' ? rustdocVisible(code) : code).replace(/\n+$/, '');
  const lines = LANGS.includes(grammar)
    ? highlighter.codeToTokens(shown, { lang: grammar, theme: 'turnframe' }).tokens.map((line) =>
        line
          .map((token) => {
            const style = [`color:${token.color}`, token.fontStyle & 1 ? 'font-style:italic' : '']
              .filter(Boolean)
              .join(';');
            return `<span style="${style}">${escape(token.content)}</span>`;
          })
          .join(''),
      )
    : shown.split('\n').map(escape);
  const numbered = grammar === 'rust' && lines.length > 4;
  const name = { shellscript: 'shell', text: 'text', '': 'text' }[grammar] ?? grammar;
  return `<div class="tf tf-code${numbered ? ' tf-code--numbered' : ''}" data-code>
<div class="tf-code__bar"><div class="tf-code__tabs"><span class="tf-code__tab" data-selected>${escape(name)}</span></div><button type="button" class="tf-btn tf-btn--ghost tf-btn--sm" data-copy><svg class="tf-icon" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/><path d="M10.5 3.5v-.5a1 1 0 0 0-1-1H3.5a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h.5"/></svg><span>Copy</span></button></div>
<div class="tf-code__body"><pre><code>${lines
    .map((line, i) => `<span class="tf-code__line" data-n="${i + 1}">${line || ' '}</span>`)
    .join('')}</code></pre></div>${caption ? `<div class="tf-code__caption">${caption}</div>` : ''}
<textarea hidden data-source data-pagefind-ignore>${escape(shown)}</textarea></div>`;
}

// A Mermaid state diagram becomes the table of its transitions: no script, and a table is what
// the design system prefers to a picture. Any other diagram stays as its source.
function stateTable(source) {
  const rows = [];
  for (const line of source.split('\n')) {
    const found = line.trim().match(/^(\[\*\]|\w+)\s*-->\s*(\[\*\]|\w+)\s*(?::\s*(.*))?$/);
    if (!found) continue;
    const [, from, to, when] = found;
    if (from === '[*]') rows.push([`<i>start</i>`, `<code>${to}</code>`, escape(when ?? '')]);
    else if (to === '[*]') rows.push([`<code>${from}</code>`, `<i>end</i>`, '']);
    else rows.push([`<code>${from}</code>`, `<code>${to}</code>`, escape(when ?? '')]);
  }
  const moves = rows.filter((row) => row[1] !== '<i>end</i>');
  const ends = rows.filter((row) => row[1] === '<i>end</i>').map((row) => row[0]);
  return `<div class="doc-table"><table><thead><tr><th>From</th><th>To</th><th>When</th></tr></thead><tbody>${moves
    .map((row) => `<tr><td>${row[0]}</td><td>${row[1]}</td><td>${row[2]}</td></tr>`)
    .join('')}</tbody></table></div>${ends.length ? `<p class="doc-note">Terminal: ${ends.join(', ')}.</p>` : ''}`;
}

/**
 * Renders `source` (Markdown). `link(href)` maps a relative link to its URL on the site.
 * Returns the HTML without the first `# ` heading, which is returned as `title`.
 */
export function render(source, { link = (href) => href } = {}) {
  const slugger = new GithubSlugger();
  const headings = [];
  let title = null;
  let description = null;

  const md = new MarkdownIt({ html: true, linkify: false, typographer: false });
  md.renderer.rules.fence = (tokens, i) => {
    const token = tokens[i];
    const lang = token.info.trim().split(/\s+/)[0];
    if (lang === 'mermaid' && /^\s*stateDiagram/.test(token.content)) return stateTable(token.content);
    return codeBlock(token.content, token.info);
  };
  md.renderer.rules.code_block = (tokens, i) => codeBlock(tokens[i].content, 'text');
  md.renderer.rules.table_open = () => '<div class="doc-table"><table>';
  md.renderer.rules.table_close = () => '</table></div>';

  const tokens = md.parse(source, {});
  for (let i = 0; i < tokens.length; i += 1) {
    const token = tokens[i];
    if (token.type === 'heading_open') {
      const inline = tokens[i + 1];
      const text = inline.children.map((child) => child.content).join('');
      if (token.tag === 'h1' && title === null) {
        title = inline.content;
        tokens.splice(i, 3);
        i -= 1;
        continue;
      }
      const id = slugger.slug(text);
      token.attrSet('id', id);
      if (token.tag === 'h2' || token.tag === 'h3') headings.push({ depth: Number(token.tag[1]), id, text });
      inline.children.push(
        Object.assign(new inline.constructor('html_inline', '', 0), {
          content: ` <a class="doc-anchor" href="#${id}" aria-label="Link to this section">#</a>`,
        }),
      );
    }
    if (token.type === 'paragraph_open' && description === null && title !== null) {
      const text = plainText(tokens[i + 1].content);
      if (!/^Status:/.test(text)) description = text;
    }
    if (token.type === 'inline') {
      for (const child of token.children ?? []) {
        if (child.type === 'link_open') child.attrSet('href', link(child.attrGet('href')));
        if (child.type === 'image') child.attrSet('src', link(child.attrGet('src'), { raw: true }));
      }
    }
  }
  return { title, description, headings, html: md.renderer.render(tokens, md.options, {}) };
}

export function plainText(markdown) {
  return markdown
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/`|\*\*/g, '')
    .replace(/\s+/g, ' ')
    .trim();
}
