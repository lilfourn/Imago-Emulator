// Checks of the site that need no browser: node --test site/site.test.mjs

import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import test from 'node:test';
import { Script } from 'node:vm';

import worker, { LINKS } from './src/worker.js';

const publicRoot = new URL('./public/', import.meta.url);
const html = readFileSync(new URL('index.html', publicRoot), 'utf8');
const config = readFileSync(new URL('wrangler.jsonc', import.meta.url), 'utf8');

/** What the Worker answers for `path`, with `vars` as its variables. */
function ask(path, vars = {}) {
  const env = { ...vars, ASSETS: { fetch: async () => new Response('a file', { status: 200 }) } };
  return worker.fetch(new Request(`https://imagoemulator.com${path}`), env);
}

test('every local link, picture and font is a file, an anchor or a route', () => {
  const ids = new Set([...html.matchAll(/\bid="([^"]+)"/g)].map((match) => match[1]));
  const refs = [...html.matchAll(/(?:href|src)="([^"]+)"|url\(["']?(\/[^)"'\s]+)["']?\)/g)].map((match) => match[1] ?? match[2]);
  assert.ok(refs.length > 60, 'the page names its pictures');
  for (const ref of refs) {
    if (ref.startsWith('#')) assert.ok(ids.has(ref.slice(1)), `no such anchor: ${ref}`);
    else if (ref.startsWith('/') && !(ref in LINKS)) assert.ok(existsSync(new URL(ref.slice(1), publicRoot)), `no such file: ${ref}`);
    else if (!ref.startsWith('/')) assert.equal(ref, 'https://imagoemulator.com/', `the page links out: ${ref}`);
  }
});

test('the page loads nothing from another site and tracks nobody', () => {
  assert.doesNotMatch(html, /<script\b[^>]*\bsrc=/i);
  assert.doesNotMatch(html, /<link\b[^>]*href="https?:(?!\/\/imagoemulator\.com\/")/i);
  assert.doesNotMatch(html, /url\(["']?https?:/i);
  for (const [, source] of html.matchAll(/<script>([\s\S]*?)<\/script>/g)) new Script(source);
});

test('public copy names systems, not games or their makers\' franchises', () => {
  // Decision 0016: the site describes the systems played and shows homebrew.
  assert.doesNotMatch(html, /pok[eé]mon|mario|zelda|kirby|metroid|animal crossing/i);
  assert.match(html, /not affiliated with, endorsed by or sponsored by Nintendo/);
});

test('every game shown is credited', () => {
  const credits = html.slice(html.indexOf('<h3>Games shown</h3>'));
  const shown = new Set([...html.matchAll(/src="\/assets\/games\/[^"]+" alt="([^"]+)"/g)].map((match) => match[1]));
  assert.ok(shown.size >= 15);
  for (const title of shown) assert.ok(credits.includes(title), `${title} is shown but not credited`);
});

test('the accents offered are the app\'s', () => {
  const theme = readFileSync(new URL('../crates/emu-app/src/theme.rs', import.meta.url), 'utf8');
  const colours = [...html.matchAll(/style="--c:#([0-9a-f]{6})">(\w+)</g)].map((match) => [match[2], match[1]]);
  assert.deepEqual(colours.map(([name]) => name), ['Blue', 'Teal', 'Green', 'Gold', 'Violet']);
  for (const [name, colour] of colours) {
    const written = `0x00${colour.slice(0, 2)}_${colour.slice(2)}`.toUpperCase().replace('0X', '0x');
    assert.ok(theme.includes(written), `${name} is #${colour} here but theme.rs has no ${written}`);
  }
});

test('every route has a variable, and the page links only to routes that lead somewhere', () => {
  for (const variable of Object.values(LINKS)) assert.match(config, new RegExp(`"${variable}":`), `${variable} is not in wrangler.jsonc`);
  const linked = new Set([...html.matchAll(/href="(\/(?:download|buy)\/[^"]+)"/g)].map((match) => match[1]));
  assert.ok(linked.has('/download/mac'), 'the page offers the Mac download');
  for (const path of linked) {
    assert.ok(path in LINKS, `no such route: ${path}`);
    const address = config.match(new RegExp(`"${LINKS[path]}":\\s*"([^"]*)"`))[1];
    assert.match(address, /^https:\/\//, `${path} is linked, but ${LINKS[path]} is empty: it would answer 404`);
  }
});

test('the download is this version\'s disk image, and the page says which', () => {
  const version = readFileSync(new URL('../Cargo.toml', import.meta.url), 'utf8').match(/^version = "(.+)"$/m)[1];
  const file = `Imago-Emulator-${version}.dmg`;
  assert.ok(config.includes(`"DOWNLOAD_MAC": "https://imagoemulator.com/downloads/${file}"`), `DOWNLOAD_MAC is not ${file}`);
  assert.ok(html.includes(`Version ${version} ·`), `the page does not say version ${version}`);
  assert.match(html, new RegExp(`<span data-sha256="${file.replaceAll('.', '\\.')}">[0-9a-f]{64}</span>`));
  // Every download is this version's, and the page prints a checksum for each.
  for (const [, name] of config.matchAll(/"DOWNLOAD_\w+": "https:\/\/imagoemulator\.com\/downloads\/([^"]+)"/g)) {
    assert.ok(name.startsWith(`Imago-Emulator-${version}`), `${name} is not version ${version}`);
    assert.ok(html.includes(`data-sha256="${name}"`), `the page prints no SHA-256 for ${name}`);
  }
});

test('a link leads where its variable says, and says so when it has none', async () => {
  const sent = await ask('/download/mac', { DOWNLOAD_MAC: 'https://example.com/Imago-Emulator.dmg' });
  assert.equal(sent.status, 302);
  assert.equal(sent.headers.get('location'), 'https://example.com/Imago-Emulator.dmg');

  for (const vars of [{}, { BUY_PATRON: '' }, { BUY_PATRON: 'javascript:alert(1)' }, { BUY_PATRON: 'http://example.com/' }, { BUY_PATRON: 'not an address' }]) {
    const unset = await ask('/buy/patron/', vars);
    assert.equal(unset.status, 404);
    assert.equal(await unset.text(), 'Not available yet.\n');
  }
});

test('the address the app opens for supporters lands on their section', async () => {
  const app = readFileSync(new URL('../crates/emu-app/src/supporter.rs', import.meta.url), 'utf8');
  assert.match(app, /"https:\/\/imagoemulator\.com\/supporter"/);
  assert.match(html, /<section class="sect" id="supporters">/);
  const answer = await ask('/supporter');
  assert.equal(answer.status, 302);
  assert.equal(answer.headers.get('location'), 'https://imagoemulator.com/#supporters');
});

test('anything else is left to the files', async () => {
  for (const path of ['/', '/assets/geist-latin.woff2', '/download', '/buy/everything']) {
    assert.equal(await (await ask(path)).text(), 'a file', path);
  }
});

test('a key the Worker signs is one the app\'s format describes, and one sale gives one key', async () => {
  const { issue, idOf, dayOf, PREFIX } = await import('./src/keys.js');
  const { generateKeyPairSync, verify } = await import('node:crypto');
  // A signing key of the test's own: its seed is the last 32 bytes of its PKCS #8.
  const pair = generateKeyPairSync('ed25519');
  const seed = pair.privateKey.export({ type: 'pkcs8', format: 'der' }).subarray(-32).toString('base64');
  const id = await idOf('cs_test_a1b2c3');
  assert.equal(id, await idOf('cs_test_a1b2c3'));
  assert.notEqual(id, await idOf('cs_test_a1b2c4'));

  const sale = { tier: 'patron', id, day: dayOf(new Date('2026-11-18T12:00:00Z')), name: '  Zoë Ⅱ ' };
  const key = await issue(seed, sale);
  assert.equal(key, await issue(seed, sale), 'one sale, one key');
  assert.ok(key.startsWith(PREFIX));
  const bytes = Buffer.from(key.slice(PREFIX.length), 'base64url');
  const body = bytes.subarray(0, -64);
  assert.equal(body[0], 1, 'the format\'s version');
  assert.equal(body[1], 2, 'a patron\'s byte');
  assert.equal(body.readBigUInt64BE(2), id);
  assert.equal(body.readUInt32BE(10), 20775, '2026-11-18 in days since 1970');
  assert.equal(body.subarray(14).toString('utf8'), 'Zoë Ⅱ');
  const signed = Buffer.concat([Buffer.from('imago-emulator supporter key\0'), body]);
  assert.ok(verify(null, signed, pair.publicKey, bytes.subarray(-64)), 'the signature is over the context and the body');

  await assert.rejects(issue(seed, { ...sale, tier: 'emperor' }), /no such tier/);
  await assert.rejects(issue(seed, { ...sale, name: 'x'.repeat(65) }), /too long/);
  await assert.rejects(issue(seed, { ...sale, name: 'a\u0007b' }), /control/);
  await assert.rejects(issue('c2hvcnQ=', sale), /32 bytes/);
});

test('the buttons that get the app get it for the visitor\'s system', async () => {
  const { runInNewContext } = await import('node:vm');
  const block = html.match(/\n  \{\n    const system = [\s\S]*?\n  \}\n/)[0];
  const buttons = (navigator) => {
    const found = [...html.matchAll(/<a class="btn[^"]*" href="([^"]+)" data-get>([^<]+)<\/a>/g)].map(([, href, textContent]) => ({ href, textContent }));
    assert.equal(found.length, 2, 'the hero\'s button and the download section\'s');
    runInNewContext(block, { navigator, document: { querySelectorAll: (selector) => (selector === 'a[data-get]' ? found : []) } });
    return found;
  };
  const cases = [
    [{ userAgentData: { platform: 'Windows' } }, '/download/windows', 'Download for Windows'],
    [{ platform: 'Win32' }, '/download/windows', 'Download for Windows'],
    [{ userAgentData: { platform: 'Linux' } }, '#download', 'Download for Linux'],
    [{ platform: 'Linux x86_64' }, '#download', 'Download for Linux'],
    [{ userAgentData: { platform: 'macOS' }, platform: 'MacIntel' }, '/download/mac', 'Download for Mac'],
    [{ userAgentData: { platform: 'Android' }, platform: 'Linux armv81' }, '/download/mac', 'Download for Mac'],
    [{}, '/download/mac', 'Download for Mac'],
  ];
  for (const [navigator, href, textContent] of cases) {
    for (const button of buttons(navigator)) assert.deepEqual(button, { href, textContent }, JSON.stringify(navigator));
  }
});
