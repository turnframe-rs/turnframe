// Tells the search engines that take IndexNow (Bing, and those built on its index) which pages
// exist, after a deploy. Google reads the sitemap instead. The key is public by design: it is
// served at /<KEY>.txt, so an engine can check that the request came from this site.
import { readFileSync } from 'node:fs';

const KEY = '159f3cfaf3ff6d497733b76db57d6230';
const HOST = 'turnframe.rs';

const sitemap = readFileSync(new URL('../dist/sitemap.xml', import.meta.url), 'utf8');
const urlList = [...sitemap.matchAll(/<loc>([^<]+)<\/loc>/g)].map((match) => match[1]);
const response = await fetch('https://api.indexnow.org/indexnow', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json; charset=utf-8' },
  body: JSON.stringify({ host: HOST, key: KEY, keyLocation: `https://${HOST}/${KEY}.txt`, urlList }),
});
console.log(`IndexNow: ${urlList.length} URLs, HTTP ${response.status}`);
if (!response.ok && response.status !== 202) process.exitCode = 1;
