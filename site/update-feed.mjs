// The feed the app asks for a newer version (crates/emu-app/src/update.rs):
// public/update.json, written by publish.sh from what the page and
// wrangler.jsonc already say: the workspace's version, and each download's
// address, its SHA-256 as the page prints it, and its size.
//
// usage: node site/update-feed.mjs    (publish.sh runs it)

import { readFileSync, statSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/** Where the app sends people whose update it doesn't install itself. */
export const PAGE = 'https://imagoemulator.com/#download';

/**
 * The feed, from Cargo.toml's text, wrangler.jsonc's, the page's, and the
 * size of each file in public/downloads by name. Each DOWNLOAD_ variable
 * gives a package named for it in lower case: DOWNLOAD_MAC is "mac",
 * DOWNLOAD_LINUX_ARM64_DEB is "linux-arm64-deb".
 */
export function feed({ cargo, wrangler, html, sizeOf }) {
  const version = cargo.match(/^version = "(.+)"$/m)[1];
  const sums = new Map([...html.matchAll(/<span data-sha256="([^"]+)">([0-9a-f]{64})<\/span>/g)].map((match) => [match[1], match[2]]));
  const downloads = {};
  for (const [, name, url] of wrangler.matchAll(/"DOWNLOAD_([A-Z0-9_]+)": "(https:\/\/imagoemulator\.com\/downloads\/[^"]+)"/g)) {
    const file = url.slice(url.lastIndexOf('/') + 1);
    if (!file.startsWith(`Imago-Emulator-${version}`)) throw new Error(`${file} is not version ${version}`);
    if (!sums.has(file)) throw new Error(`the page prints no SHA-256 for ${file}`);
    downloads[name.toLowerCase().replaceAll('_', '-')] = { url, sha256: sums.get(file), size: sizeOf(file) };
  }
  if (!downloads.mac) throw new Error('wrangler.jsonc names no Mac download');
  return { version, page: PAGE, downloads };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const here = (name) => new URL(name, import.meta.url);
  const text = feed({
    cargo: readFileSync(here('../Cargo.toml'), 'utf8'),
    wrangler: readFileSync(here('wrangler.jsonc'), 'utf8'),
    html: readFileSync(here('public/index.html'), 'utf8'),
    sizeOf: (file) => statSync(here(`public/downloads/${file}`)).size,
  });
  writeFileSync(here('public/update.json'), `${JSON.stringify(text, null, 2)}\n`);
}
