import react from '@astrojs/react';
import { defineConfig } from 'astro/config';

import { existsSync, readFileSync } from 'node:fs';
import { extname, join } from 'node:path';

import { writeTokens } from './scripts/tokens.mjs';

// `astro dev` has no search index of its own: it serves the one the last build wrote.
const TYPES = { '.js': 'text/javascript', '.json': 'application/json', '.css': 'text/css', '.wasm': 'application/wasm' };
const lastBuildsIndex = {
  name: 'turnframe-dev-search',
  configureServer(server) {
    server.middlewares.use('/pagefind', (request, response, next) => {
      const path = join('dist', 'pagefind', decodeURIComponent(request.url.split('?')[0]));
      if (!existsSync(path) || !path.startsWith(join('dist', 'pagefind'))) return next();
      response.setHeader('Content-Type', TYPES[extname(path)] ?? 'application/octet-stream');
      response.end(readFileSync(path));
    });
  },
};

// Every page is static HTML. React renders the design system's components at build time and
// hydrates only the few that move (the replay, the effort panel, code tabs, copy buttons).
export default defineConfig({
  site: 'https://turnframe.rs',
  trailingSlash: 'never',
  build: { format: 'directory' },
  integrations: [
    react(),
    { name: 'turnframe-tokens', hooks: { 'astro:config:setup': () => writeTokens() } },
  ],
  devToolbar: { enabled: false },
  vite: { plugins: [lastBuildsIndex] },
});
