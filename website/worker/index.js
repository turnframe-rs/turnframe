// The only server-side code the site has. It sends www to the apex, and puts the security
// headers and a cache policy on what the static assets answer. It sets no cookie and stores
// nothing about a reader.

const DOCUMENT_HEADERS = {
  'X-Content-Type-Options': 'nosniff',
  'X-Frame-Options': 'DENY',
  'Referrer-Policy': 'strict-origin-when-cross-origin',
  'Permissions-Policy': 'camera=(), microphone=(), geolocation=(), interest-cohort=()',
  'Content-Security-Policy': [
    "default-src 'self'",
    "script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'",
    "style-src 'self' 'unsafe-inline'",
    "img-src 'self' data:",
    "font-src 'self'",
    "connect-src 'self'",
    "frame-ancestors 'none'",
    "base-uri 'self'",
    "form-action 'self'",
  ].join('; '),
};

// Built assets carry a content hash in their name and never change; fonts are versioned by hand.
const IMMUTABLE = /^\/(_astro|fonts)\//;


export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.hostname.startsWith('www.')) {
      url.hostname = url.hostname.slice(4);
      return Response.redirect(url.toString(), 301);
    }

    const response = await env.ASSETS.fetch(request);
    const headers = new Headers(response.headers);
    // The site is indexed at its own name only; its workers.dev preview stays out of search.
    if (url.hostname.endsWith('.workers.dev')) {
      headers.set('X-Robots-Tag', 'noindex');
    }
    if ((headers.get('Content-Type') ?? '').startsWith('text/html')) {
      for (const [name, value] of Object.entries(DOCUMENT_HEADERS)) headers.set(name, value);
    } else if (IMMUTABLE.test(url.pathname) && response.ok) {
      headers.set('Cache-Control', 'public, max-age=31536000, immutable');
    }
    return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
  },
};
