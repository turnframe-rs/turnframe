// Renders the social card and the touch icon from scripts/cards/*.html with a local Chrome.
// Run it after changing either page or the tokens: `node scripts/cards.mjs`. The PNGs are
// committed, so a build needs no browser.
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import { writeTokens } from './tokens.mjs';

writeTokens();

const chrome = process.env.CHROME ?? '/opt/google/chrome/chrome';
const shot = (page, out, width, height) =>
  execFileSync(chrome, [
    '--headless=new', '--no-sandbox', '--hide-scrollbars', '--force-device-scale-factor=1',
    `--window-size=${width},${height}`, '--virtual-time-budget=2000', `--screenshot=${out}`,
    new URL(page, import.meta.url).href,
  ], { stdio: 'ignore' });

const here = (path) => fileURLToPath(new URL(path, import.meta.url));
shot('cards/og.html', here('../public/og.png'), 1200, 630);
shot('cards/icon.html', here('../public/apple-touch-icon.png'), 180, 180);
