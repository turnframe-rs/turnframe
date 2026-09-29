# turnframe.rs

The website of Turnframe: a home page and the documentation, served from Cloudflare.

It lives in the repository it describes, and reads that repository when it builds. The guides,
decision records, changelog and security policy are the files under `docs/` and at the root,
rendered as they stand. The version, MSRV, crate table and feature flags come from the manifests
and the README. The cost and pass rate of each effort level come from `docs/benchmarks.md`. A
source whose shape changed fails the build with the file to look at, so the site cannot publish a
figure the repository no longer holds.

No crate includes this directory: each crate packages its own folder, and the workspace root is
not a package.

## Running it

Node 22.12 or newer.

```sh
cd website
npm install
npm run dev       # http://localhost:4321; search reads the last build's index
npm run build     # dist/, with the search index
npm test          # after a build: copy rules, links, figures, the replay's names
npm run preview   # dist/ through the Worker, as Cloudflare serves it
```

`TURNFRAME_ROOT=/path/to/checkout` builds the site against another checkout of the repository.

## What is where

| Path | What it holds |
| --- | --- |
| `src/pages/index.astro` | the home page |
| `src/pages/docs/[...slug].astro` | every documentation page, with its sidebar and contents |
| `src/lib/docs.mjs` | which pages exist, in which order, and where each one's Markdown comes from |
| `src/lib/markdown.mjs` | Markdown to HTML: anchors, highlighted code, links rewritten to the site or GitHub |
| `src/lib/repo.mjs` | the facts read from the repository |
| `src/content/` | the few pages written for the site: introduction, installation, examples, API reference |
| `src/data/scenarios.js` | the four scripted turns of the replay on the home page |
| `src/ds/turnframe.js`, `src/styles/ds.css` | the design system's components and stylesheet |
| `design/tokens.json` | the design system's tokens; `scripts/tokens.mjs` writes `src/styles/tokens.css` from them |
| `scripts/cards/` | the social card and touch icon, as HTML; `node scripts/cards.mjs` renders the PNGs |
| `worker/index.js` | the Worker in front of the assets: `www` to the apex, and security headers |

Pages are static HTML. React renders the design system's components at build time and hydrates
four of them in the browser: the replay, the effort panel, code tabs and copy buttons. Search is
[Pagefind](https://pagefind.app), indexed from the built pages. `/llms.txt` lists every page for a
language model, and `/llms-full.txt` holds them all in one file.

## Adding a page

A guide added under `docs/` appears on the site once it has an entry in `NAV` in
`src/lib/docs.mjs`, and a title and description in `src/data/search.js`; the build names a page
that has none. Links between guides stay relative repository paths, as GitHub reads them;
the site sends each one to the page that renders it, or to the file on GitHub.

## Search engines

Each page's title tag and meta description are written for what people search with, in
`src/data/search.js`; decision records are described from their status line. The home page carries
`SoftwareSourceCode` structured data, and every docs page a `TechArticle` and its breadcrumbs. The
sitemap and each page's "last changed" date come from `git log`, so CI checks out the whole
history. After a deploy, `scripts/indexnow.mjs` tells Bing and the engines built on it which pages
exist; Google reads the sitemap. The Worker marks the `workers.dev` copy `noindex`.
`tests/seo.test.mjs` holds the rules: one h1, a title and description that fit a result and are
the page's own, a canonical address, structured data that parses, a sitemap of exactly the pages.

## Copy

Everything the site itself writes follows the rules in `tests/rules.mjs`: no em or en dash, no
stacked negatives, no «rather than», nothing that says when something ships. Numbers are read
from the repository, never typed.

## Deploying

`.github/workflows/website.yml` builds and tests the site on every push and pull request that
touches it or anything it reads, and a push to `main` deploys it. Running it by hand on `main`, from
the Actions tab or with `gh workflow run website.yml --ref main`, redeploys without a change. It
needs two secrets, `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`. The Worker is
`turnframe-website`, on the custom domains `turnframe.rs` and `www.turnframe.rs`; wrangler creates
their DNS records and certificates on the first deploy, once the zone is on Cloudflare.

By hand, from a machine logged in with `npx wrangler login`:

```sh
npm run deploy
```
