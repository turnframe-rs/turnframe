// Writes src/styles/tokens.css from design/tokens.json, the design system's own token file.
// To take a newer design system, replace design/tokens.json; the stylesheet follows.
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const root = new URL('../', import.meta.url);

// The width axis each variable face carries in its file (fvar `wdth`). tokens.json records weight
// only; without the range a browser draws every width at the face's default, the widest one.
const STRETCH = { 'Martian Mono': '75% 112.5%', 'IBM Plex Sans': '75% 100%' };

export function tokensCss(tokens) {
  const [primary, second] = tokens.color.themes.map((theme) => theme.id);
  const colours = (theme) =>
    tokens.color.tokens.map((t) => {
      const value = typeof t.value === 'string' ? t.value : (t.value[theme] ?? t.value[primary]);
      return `  --${t.name}: ${value};`;
    });
  const plain = ['spacing', 'radius', 'size']
    .flatMap((family) => tokens[family].tokens)
    .map((t) => `  --${t.name}: ${t.value};`);
  const shadow = (theme) =>
    tokens.shadow.tokens.map((t) => `  --${t.name}: ${typeof t.value === 'string' ? t.value : t.value[theme]};`);
  const families = Object.entries(tokens.type.families).map(([name, stack]) => `  --font-${name}: ${stack};`);
  const faces = tokens.type.fonts.map(
    (font) => `@font-face {
  font-family: "${font.family}";
  src: url("/${font.file}") format("woff2");
  font-weight: ${font.weight};${STRETCH[font.family] ? `\n  font-stretch: ${STRETCH[font.family]};` : ''}
  font-style: ${font.style ?? 'normal'};
  font-display: swap;
}`,
  );
  const block = (selector, theme) =>
    `${selector} {\n  color-scheme: ${theme === 'light' ? 'light' : 'dark'};\n${[...colours(theme), ...shadow(theme)].join('\n')}\n}`;

  return [
    '/* Generated from design/tokens.json by scripts/tokens.mjs. Edit the tokens, not this file. */',
    ...faces,
    `:root {\n${[...families, ...plain].join('\n')}\n}`,
    block(':root', primary),
    `@media (prefers-color-scheme: ${second}) {\n${block(`  :root:not([data-theme="${primary}"])`, second).replace(/\n/g, '\n  ')}\n}`,
    block(`:root[data-theme="${second}"]`, second),
    '',
  ].join('\n\n');
}

export function writeTokens() {
  const tokens = JSON.parse(readFileSync(new URL('design/tokens.json', root), 'utf8'));
  writeFileSync(new URL('src/styles/tokens.css', root), tokensCss(tokens));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) writeTokens();
