// Checks of the site that need no browser: node --test site/site.test.mjs

import assert from 'node:assert/strict';
import { createHmac, generateKeyPairSync, verify } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import test from 'node:test';
import { runInNewContext, Script } from 'node:vm';

import { idOf, PREFIX, TIERS } from './src/keys.js';
import { feed, PAGE } from './update-feed.mjs';
import { signed, SUPPORT } from './src/shop.js';
import worker, { LINKS, SELLING } from './src/worker.js';

const publicRoot = new URL('./public/', import.meta.url);
const read = (name) => readFileSync(new URL(name, publicRoot), 'utf8');
const html = read('index.html');
/** The small pages: the three policies, and the page that shows a buyer their key. */
const POLICIES = ['terms', 'privacy', 'refunds'];
const pages = Object.fromEntries(['index', ...POLICIES, 'thanks'].map((name) => [name, read(`${name}.html`)]));
const config = readFileSync(new URL('wrangler.jsonc', import.meta.url), 'utf8');
/** The app's source: the repository, its issues and its licence, which the pages may link to. */
const SOURCE = 'https://github.com/lilfourn/Imago-Emulator';
const SOURCE_LINKS = [SOURCE, `${SOURCE}/issues`, `${SOURCE}/blob/main/LICENSE`];
/** The one script from another site: Paddle's checkout, loaded when someone buys. */
const PADDLE_JS = 'https://cdn.paddle.com/paddle/v2/paddle.js';

/** What the Worker answers for `path`, with `vars` as its variables. */
function ask(path, vars = {}, init = {}) {
  const env = { ...vars, ASSETS: { fetch: async () => new Response('a file', { status: 200 }) } };
  return worker.fetch(new Request(`https://imagoemulator.com${path}`, init), env);
}

test('every local link, picture and font is a file, an anchor or a route', () => {
  for (const [name, page] of Object.entries(pages)) {
    const ids = new Set([...page.matchAll(/\bid="([^"]+)"/g)].map((match) => match[1]));
    const refs = [...page.matchAll(/(?:href|src)="([^"]+)"|url\(["']?(\/[^)"'\s]+)["']?\)/g)].map((match) => match[1] ?? match[2]);
    if (name === 'index') assert.ok(refs.length > 60, 'the page names its pictures');
    for (const ref of refs) {
      if (ref.startsWith('#')) assert.ok(ids.has(ref.slice(1)), `${name}: no such anchor: ${ref}`);
      else if (ref === '/' || ref === '/#supporters') assert.match(html, /id="supporters"/);
      else if (ref.startsWith('/') && !(ref in LINKS)) {
        // A page is served at its name without ".html".
        const file = ref.slice(1);
        assert.ok(existsSync(new URL(file, publicRoot)) || POLICIES.includes(file), `${name}: no such file: ${ref}`);
      } else if (ref === `mailto:${SUPPORT}` || SOURCE_LINKS.includes(ref)) continue;
      // Otherwise only the policies link out, and only to the seller's own terms and help.
      else if (POLICIES.includes(name)) assert.match(ref, /^https:\/\/(www\.paddle\.com\/legal\/[a-z-]+|paddle\.net)$/, `${name} links out: ${ref}`);
      else if (!ref.startsWith('/')) assert.equal(ref, 'https://imagoemulator.com/', `${name} links out: ${ref}`);
    }
  }
});

test('the pages load nothing from another site and track nobody, but for the checkout when someone buys', () => {
  for (const [name, page] of Object.entries(pages)) {
    assert.doesNotMatch(page, /<script\b[^>]*\bsrc=/i, name);
    assert.doesNotMatch(page, /<link\b[^>]*href="https?:(?!\/\/imagoemulator\.com\/")/i, name);
    assert.doesNotMatch(page, /url\(["']?https?:/i, name);
    assert.doesNotMatch(page, /<(?:img|iframe|video|audio|source|embed|object)\b[^>]*\bsrc="https?:/i, name);
    const scripts = [...page.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((match) => match[1]);
    for (const source of scripts) new Script(source);
    // Paddle's script is named once, in the block that a press of a buy button runs.
    const named = scripts.join('\n').match(/https?:\/\/[^\s'"`]+/g) ?? [];
    assert.deepEqual(named, name === 'index' ? [PADDLE_JS] : [], name);
  }
  assert.doesNotMatch(read('assets/page.css'), /https?:/);
  const buying = html.slice(html.indexOf('const buttons = document.querySelectorAll(\'button[data-buy]\')'));
  assert.ok(buying.includes(PADDLE_JS), 'the checkout is loaded by the buy buttons\' own block');
});

test('public copy names systems, not games or their makers\' franchises', () => {
  // Decision 0016: the site describes the systems played and shows homebrew.
  for (const [name, page] of Object.entries(pages)) assert.doesNotMatch(page, /pok[eé]mon|mario|zelda|kirby|metroid|animal crossing/i, name);
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
  const linked = new Set([...html.matchAll(/href="(\/download\/[^"]+)"/g)].map((match) => match[1]));
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

test('the update feed names this version and every download, with the SHA-256 the page prints', () => {
  const cargo = readFileSync(new URL('../Cargo.toml', import.meta.url), 'utf8');
  const version = cargo.match(/^version = "(.+)"$/m)[1];
  const made = feed({ cargo, wrangler: config, html, sizeOf: (file) => file.length });
  assert.equal(made.version, version);
  assert.equal(made.page, PAGE);
  assert.ok(html.includes('id="download"'), 'the page the feed sends people to has no download section');
  const app = readFileSync(new URL('../crates/emu-app/src/update.rs', import.meta.url), 'utf8');
  assert.match(app, /"https:\/\/imagoemulator\.com\/update\.json"/, 'the app asks another address');
  assert.match(app, /Some\("mac"\)/, 'the app looks for another package name');
  const variables = [...config.matchAll(/"DOWNLOAD_(\w+)": "(https:[^"]+)"/g)];
  assert.deepEqual(Object.keys(made.downloads).sort(), variables.map(([, name]) => name.toLowerCase().replaceAll('_', '-')).sort());
  for (const [, name, url] of variables) {
    const entry = made.downloads[name.toLowerCase().replaceAll('_', '-')];
    const file = url.slice(url.lastIndexOf('/') + 1);
    assert.equal(entry.url, url);
    assert.equal(entry.size, file.length);
    assert.ok(html.includes(`<span data-sha256="${file}">${entry.sha256}</span>`), `${file}'s SHA-256 is not the page's`);
  }
  // A page that prints no SHA-256 for a download makes no feed.
  assert.throws(() => feed({ cargo, wrangler: config, html: '', sizeOf: () => 1 }), /SHA-256/);
});

test('a link leads where its variable says, and says so when it has none', async () => {
  const sent = await ask('/download/mac', { DOWNLOAD_MAC: 'https://example.com/Imago-Emulator.dmg' });
  assert.equal(sent.status, 302);
  assert.equal(sent.headers.get('location'), 'https://example.com/Imago-Emulator.dmg');

  for (const vars of [{}, { DOWNLOAD_LINUX: '' }, { DOWNLOAD_LINUX: 'javascript:alert(1)' }, { DOWNLOAD_LINUX: 'http://example.com/' }, { DOWNLOAD_LINUX: 'not an address' }]) {
    const unset = await ask('/download/linux/', vars);
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
  for (const path of ['/', '/assets/geist-latin.woff2', '/download', '/buy/everything', '/terms', '/thanks', '/api', '/api/keys', '/constructor']) {
    assert.equal(await (await ask(path)).text(), 'a file', path);
  }
});

test('a key the Worker signs is one the app\'s format describes, and one sale gives one key', async () => {
  const { issue, dayOf } = await import('./src/keys.js');
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

// ---- Selling keys (src/shop.js) ----

/** A signing key of the tests' own: its seed is the last 32 bytes of its PKCS #8. */
const pair = generateKeyPairSync('ed25519');
const seed = pair.privateKey.export({ type: 'pkcs8', format: 'der' }).subarray(-32).toString('base64');
const PRICES = { supporter: `pri_${'a'.repeat(26)}`, patron: `pri_${'b'.repeat(26)}`, founder: `pri_${'c'.repeat(26)}` };
const SALE = `txn_01${'k'.repeat(24)}`;
const WEBHOOK_SECRET = 'pdl_ntfset_a-secret-of-the-tests';

/** A Worker with all that selling needs, and the mail it was asked to send. */
function open(overrides = {}) {
  const mail = [];
  const vars = {
    PADDLE_ENVIRONMENT: 'production',
    PADDLE_CLIENT_TOKEN: 'live_0123456789abcdef',
    PADDLE_API_KEY: 'pdl_live_apikey_of-the-tests',
    PADDLE_WEBHOOK_SECRET: WEBHOOK_SECRET,
    SUPPORTER_SIGNING_KEY: seed,
    PADDLE_PRICE_SUPPORTER: PRICES.supporter,
    PADDLE_PRICE_PATRON: PRICES.patron,
    PADDLE_PRICE_FOUNDER: PRICES.founder,
    EMAIL: {
      send: async (message) => {
        mail.push(message);
        return { messageId: 'sent' };
      },
    },
    ...overrides,
  };
  return { vars, mail };
}

/** A transaction as Paddle answers with one. */
function sale(changes = {}) {
  return {
    id: SALE,
    status: 'completed',
    created_at: '2026-11-18T23:59:59.5Z',
    billed_at: '2026-11-19T00:00:03Z',
    customer_id: 'ctm_01buyer',
    items: [{ price: { id: PRICES.patron }, quantity: 1 }],
    customer: { id: 'ctm_01buyer', email: 'buyer@example.com', name: null },
    ...changes,
  };
}

/** Runs `run` with Paddle's API answered by `paddle`, and gives the requests made of it. */
async function withPaddle(paddle, run) {
  const asked = [];
  const real = globalThis.fetch;
  globalThis.fetch = async (url, init = {}) => {
    asked.push({ url: String(url), authorization: init.headers?.authorization });
    return paddle(String(url), init);
  };
  try {
    return [await run(), asked];
  } finally {
    globalThis.fetch = real;
  }
}
const answers = (data, status = 200) => () => new Response(JSON.stringify({ data }), { status });

/** A notice of `event`, signed as Paddle signs one. */
function posted(event, secret = WEBHOOK_SECRET) {
  const body = JSON.stringify(event);
  const time = '1763510400';
  const h1 = createHmac('sha256', secret).update(`${time}:${body}`).digest('hex');
  return { method: 'POST', body, headers: { 'paddle-signature': `ts=${time};h1=${h1}` } };
}
const completed = (id = SALE) => ({ event_id: 'evt_01', event_type: 'transaction.completed', occurred_at: '2026-11-19T00:00:05Z', data: { id, status: 'completed' } });

/** The parts of `key`, with its signature checked against the tests' public key. */
function opened(key) {
  assert.ok(key.startsWith(PREFIX));
  const bytes = Buffer.from(key.slice(PREFIX.length), 'base64url');
  const body = bytes.subarray(0, -64);
  assert.ok(verify(null, Buffer.concat([Buffer.from('imago-emulator supporter key\0'), body]), pair.publicKey, bytes.subarray(-64)), 'the key is signed');
  return { tier: body[1], id: body.readBigUInt64BE(2), day: body.readUInt32BE(10), name: body.subarray(14).toString('utf8') };
}

test('nothing is sold until all that selling needs is set, and for one environment', async () => {
  const without = (name) => open({ [name]: undefined }).vars;
  const closed = [
    {},
    ...Object.keys(open().vars).map(without),
    open({ PADDLE_ENVIRONMENT: 'live' }).vars,
    // A sandbox token, key or environment beside a live one.
    open({ PADDLE_ENVIRONMENT: 'sandbox' }).vars,
    open({ PADDLE_CLIENT_TOKEN: 'test_0123456789abcdef' }).vars,
    open({ PADDLE_API_KEY: 'pdl_sdbx_apikey_of-the-tests' }).vars,
    open({ PADDLE_PRICE_PATRON: 'pri_short' }).vars,
    // Two tiers on one price would sell a Founder key at a Supporter's.
    open({ PADDLE_PRICE_FOUNDER: PRICES.supporter }).vars,
    open({ EMAIL: {} }).vars,
    // A signing key that could sign nothing: found out before any payment.
    open({ SUPPORTER_SIGNING_KEY: 'c2hvcnQ=' }).vars,
    open({ SUPPORTER_SIGNING_KEY: '~/.config/imago-emulator/supporter-signing.key' }).vars,
    open({ SUPPORTER_SIGNING_KEY: `${seed}AAAA` }).vars,
  ];
  for (const vars of closed) {
    for (const path of Object.keys(SELLING)) {
      const [answer, asked] = await withPaddle(answers(sale()), () => ask(`${path}?txn=${SALE}`, vars, path === '/api/paddle' ? posted(completed()) : {}));
      assert.equal(answer.status, 404, `${path} with ${JSON.stringify(Object.keys(vars))}`);
      assert.equal(await answer.text(), 'Not available yet.\n');
      assert.deepEqual(asked, []);
    }
  }
  // The published site's own settings: the live environment, with the mail, and no secret in the file.
  assert.match(config, /"PADDLE_ENVIRONMENT": "production"/);
  assert.match(config, /"send_email": \[\{ "name": "EMAIL" \}\]/);
  for (const name of ['PADDLE_CLIENT_TOKEN', 'PADDLE_PRICE_SUPPORTER', 'PADDLE_PRICE_PATRON', 'PADDLE_PRICE_FOUNDER']) assert.match(config, new RegExp(`"${name}": "`));
  assert.doesNotMatch(config, /pdl_|SIGNING_KEY"|WEBHOOK_SECRET"|API_KEY"/);
});

test('the page is told what opens a checkout, and no secret', async () => {
  const { vars } = open();
  const answer = await ask('/api/checkout', vars);
  assert.equal(answer.status, 200);
  assert.equal(answer.headers.get('cache-control'), 'no-store');
  const text = await answer.text();
  assert.deepEqual(JSON.parse(text), { environment: 'production', token: vars.PADDLE_CLIENT_TOKEN, prices: PRICES });
  for (const secret of [vars.PADDLE_API_KEY, vars.PADDLE_WEBHOOK_SECRET, vars.SUPPORTER_SIGNING_KEY]) assert.ok(!text.includes(secret));

  const sandbox = open({ PADDLE_ENVIRONMENT: 'sandbox', PADDLE_CLIENT_TOKEN: 'test_0123456789abcdef', PADDLE_API_KEY: 'pdl_sdbx_apikey_of-the-tests' }).vars;
  assert.equal((await (await ask('/api/checkout', sandbox)).json()).environment, 'sandbox');
});

test('a notice is Paddle\'s only if its signature is', async () => {
  const body = '{"event_type":"transaction.completed"}';
  const sign = (secret, time, text) => createHmac('sha256', secret).update(`${time}:${text}`).digest('hex');
  const good = sign('secret', '1763510400', body);
  assert.ok(await signed('secret', `ts=1763510400;h1=${good}`, body));
  assert.ok(await signed('secret', `ts=1763510400;h1=${good.toUpperCase()}`, body));
  // While a secret is changed there are two, and either will do.
  assert.ok(await signed('secret', `ts=1763510400;h1=${'0'.repeat(64)};h1=${good}`, body));
  const bad = [
    `ts=1763510400;h1=${sign('another', '1763510400', body)}`,
    `ts=1763510401;h1=${good}`,
    `ts=1763510400;h1=${good.slice(0, 63)}`,
    `ts=1763510400;h1=`,
    `ts=1763510400`,
    `h1=${good}`,
    `ts=soon;h1=${good}`,
    '',
    null,
  ];
  for (const header of bad) assert.equal(await signed('secret', header, body), false, String(header));
  assert.equal(await signed('secret', `ts=1763510400;h1=${good}`, `${body} `), false, 'a body changed by one space');
});

test('a paid sale shows its buyer one key, and nothing else shows any', async () => {
  const { vars } = open();
  const [answer, asked] = await withPaddle(answers(sale()), () => ask(`/api/key?txn=${SALE}`, vars));
  assert.equal(answer.status, 200);
  assert.equal(answer.headers.get('cache-control'), 'no-store');
  const found = await answer.json();
  assert.equal(found.tier, 'Patron');
  // The key's number is the sale's, and its day the day the sale began.
  assert.deepEqual(opened(found.key), { tier: TIERS.patron, id: await idOf(SALE), day: 20775, name: '' });
  assert.deepEqual(asked, [{ url: `https://api.paddle.com/transactions/${SALE}`, authorization: `Bearer ${vars.PADDLE_API_KEY}` }]);

  // Asked again, and while Paddle still calls it paid: the same key.
  const [again] = await withPaddle(answers(sale({ status: 'paid', billed_at: null })), () => ask(`/api/key?txn=${SALE}`, vars));
  assert.equal((await again.json()).key, found.key);

  // The highest key among what was bought.
  const both = sale({ items: [{ price: { id: PRICES.supporter } }, { price: { id: PRICES.founder } }, { price: { id: 'pri_other' } }] });
  const [most] = await withPaddle(answers(both), () => ask(`/api/key?txn=${SALE}`, vars));
  assert.equal((await most.json()).tier, 'Founder');

  // The sandbox is asked at its own address.
  const sandbox = open({ PADDLE_ENVIRONMENT: 'sandbox', PADDLE_CLIENT_TOKEN: 'test_0123456789abcdef', PADDLE_API_KEY: 'pdl_sdbx_apikey_of-the-tests' }).vars;
  const [, there] = await withPaddle(answers(sale()), () => ask(`/api/key?txn=${SALE}`, sandbox));
  assert.equal(there[0].url, `https://sandbox-api.paddle.com/transactions/${SALE}`);

  const refused = [
    // Not yet paid for: asked again by the page.
    [answers(sale({ status: 'ready' })), 202],
    [answers(sale({ status: 'billed' })), 202],
    [answers(sale({ status: 'past_due' })), 202],
    // Given up on: there will be no key.
    [answers(sale({ status: 'canceled' })), 404],
    [answers(sale({ status: 'refunded-or-anything-new' })), 404],
    // Paid for, but not a key.
    [answers(sale({ items: [{ price: { id: 'pri_other' } }] })), 404],
    [answers(sale({ items: undefined })), 404],
    [answers(undefined, 404), 404],
    // Paddle failing, or answering about another sale.
    [answers(undefined, 500), 502],
    [answers(undefined, 403), 502],
    [answers(sale({ id: `txn_01${'j'.repeat(24)}` })), 502],
    [() => new Response('not json'), 502],
    [() => { throw new TypeError('no network'); }, 502],
    // Paid for, and the key cannot be made: a fault of ours, said as one.
    [answers(sale({ created_at: 'no date' })), 500],
  ];
  for (const [paddle, status] of refused) {
    const [none] = await withPaddle(paddle, () => ask(`/api/key?txn=${SALE}`, vars));
    assert.equal(none.status, status);
    assert.equal((await none.json()).key, undefined);
  }
  // One asker asks only so often, and what is refused costs Paddle no question.
  const askers = [];
  const limited = (success) => open({ KEY_ASKERS: { limit: async ({ key }) => { askers.push(key); return { success }; } } }).vars;
  const from = { headers: { 'cf-connecting-ip': '203.0.113.7' } };
  const [often, unasked] = await withPaddle(answers(sale()), () => ask(`/api/key?txn=${SALE}`, limited(false), from));
  assert.equal(often.status, 429);
  assert.deepEqual(unasked, []);
  const [allowed] = await withPaddle(answers(sale()), () => ask(`/api/key?txn=${SALE}`, limited(true), from));
  assert.equal((await allowed.json()).key, found.key);
  assert.deepEqual(askers, ['203.0.113.7', '203.0.113.7']);
  assert.match(config, /"ratelimits": \[\{ "name": "KEY_ASKERS", /);

  // An address that names no sale is not asked of Paddle at all.
  for (const id of ['', 'txn_1', `${SALE}/../../customers`, `${SALE}?include=customer`, SALE.toUpperCase(), `ctm_01${'k'.repeat(24)}`]) {
    const [none, nobody] = await withPaddle(answers(sale()), () => ask(`/api/key?txn=${encodeURIComponent(id)}`, vars));
    assert.equal(none.status, 400, id);
    assert.deepEqual(nobody, []);
  }
});

test('Paddle\'s notice of a sale sends the buyer their key, once per notice and only for a sale', async () => {
  const { vars, mail } = open();
  const [answer, asked] = await withPaddle(answers(sale()), () => ask('/api/paddle', vars, posted(completed())));
  assert.equal(answer.status, 200);
  assert.deepEqual(asked, [{ url: `https://api.paddle.com/transactions/${SALE}?include=customer`, authorization: `Bearer ${vars.PADDLE_API_KEY}` }]);
  assert.equal(mail.length, 1);
  const [letter] = mail;
  assert.equal(letter.to, 'buyer@example.com');
  assert.deepEqual(letter.from, { email: 'keys@imagoemulator.com', name: 'Imago Emulator' });
  assert.equal(letter.replyTo, SUPPORT);
  assert.equal(letter.subject, 'Your Imago Emulator Patron key');
  // The key mailed is the one the buyer's page shows.
  const [shown] = await withPaddle(answers(sale()), () => ask(`/api/key?txn=${SALE}`, vars));
  const { key } = await shown.json();
  for (const part of [letter.text, letter.html]) {
    assert.ok(part.includes(key));
    assert.ok(part.includes(`https://imagoemulator.com/thanks?txn=${SALE}`));
    assert.ok(part.includes('Paste Key') && part.includes(SUPPORT));
  }
  // What the letter tells a buyer to press is what the app's settings say.
  const settings = readFileSync(new URL('../crates/emu-app/src/settings_window.rs', import.meta.url), 'utf8');
  assert.ok(settings.includes('"Paste Key"') && settings.includes('"Supporter"'));

  // The same notice again sends the same key: nothing is remembered, and nothing need be.
  await withPaddle(answers(sale()), () => ask('/api/paddle', vars, posted(completed())));
  assert.equal(mail.length, 2);
  assert.equal(mail[1].text, letter.text);

  // What is not Paddle's, or not a sale of a key, sends nothing.
  const quiet = [
    [posted(completed(), 'another secret'), answers(sale()), 401, 0],
    [{ method: 'POST', body: JSON.stringify(completed()) }, answers(sale()), 401, 0],
    [{ ...posted(completed()), body: JSON.stringify({ ...completed(), data: { id: `txn_01${'j'.repeat(24)}` } }) }, answers(sale()), 401, 0],
    [{ method: 'GET', headers: posted(completed()).headers }, answers(sale()), 405, 0],
    [posted({ ...completed(), event_type: 'transaction.paid' }), answers(sale()), 200, 0],
    [posted({ ...completed(), event_type: 'adjustment.created' }), answers(sale()), 200, 0],
    [posted(completed('sub_01')), answers(sale()), 200, 0],
    [posted({}), answers(sale()), 200, 0],
    // Paddle's test notices name sales that do not exist.
    [posted(completed()), answers(undefined, 404), 200, 1],
    [posted(completed()), answers(sale({ items: [{ price: { id: 'pri_other' } }] })), 200, 1],
    [posted(completed()), answers(sale({ status: 'canceled' })), 200, 1],
    // Paddle failing, or answering without the buyer's address: the notice is to be sent again.
    [posted(completed()), answers(sale({ customer: undefined })), 502, 1],
    [posted(completed()), answers(sale({ customer: { email: null } })), 502, 1],
    [posted(completed()), answers(undefined, 503), 502, 1],
    [posted(completed()), () => { throw new TypeError('no network'); }, 502, 1],
  ];
  for (const [init, paddle, status, requests] of quiet) {
    const [none, made] = await withPaddle(paddle, () => ask('/api/paddle', vars, init));
    assert.equal(none.status, status, JSON.stringify(init).slice(0, 120));
    assert.equal(made.length, requests);
  }
  assert.equal(mail.length, 2, 'none of those sent a key');

  // Mail that fails asks Paddle to send the notice again.
  const failing = open({ EMAIL: { send: async () => { throw new Error('no mail today'); } } }).vars;
  const [unsent] = await withPaddle(answers(sale()), () => ask('/api/paddle', failing, posted(completed())));
  assert.equal(unsent.status, 500);
});

test('a notice is answered inside Paddle\'s five seconds, however slow Paddle and the mail are', { timeout: 20_000 }, async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout', 'Date'], now: 1_763_510_400_000 });
  // Waits for `ready` without moving the clock: what the Worker awaits
  // (signing, reading an answer) takes real time, and no fixed number of
  // turns is enough for it on every machine.
  const until = async (ready) => {
    while (!ready()) await new Promise((resolve) => setImmediate(resolve));
  };
  const turns = async () => {
    for (let at = 0; at < 50; at += 1) await new Promise((resolve) => setImmediate(resolve));
  };
  const never = new Promise(() => {});
  const after = (wait, value) => new Promise((resolve) => setTimeout(() => resolve(value), wait));

  /**
   * A notice posted to a Worker whose Paddle answers as `paddle` says and
   * whose mail is sent as `send` says, for `steps` to move the clock
   * under: it is given what has happened so far.
   */
  const posting = async (paddle, send, steps) => {
    const seen = { asked: 0, mailed: 0, answer: undefined, question: undefined };
    const { vars } = open({
      EMAIL: {
        send: (message) => {
          seen.mailed += 1;
          return send(message);
        },
      },
    });
    await withPaddle(
      (url, init) => {
        seen.asked += 1;
        seen.question = init.signal;
        return paddle();
      },
      async () => {
        const answered = ask('/api/paddle', vars, posted(completed())).then((answer) => {
          seen.answer = answer;
        });
        await steps(seen);
        await answered;
      },
    );
    return seen;
  };
  const prompt = () => new Response(JSON.stringify({ data: sale() }));
  const slow = () => after(3000, prompt());

  // Mail that never finishes: answered when the time is up, and not before, so that Paddle sends again.
  const stuck = await posting(prompt, () => never, async (seen) => {
    await until(() => seen.mailed === 1);
    t.mock.timers.tick(4499);
    await turns();
    assert.equal(seen.answer, undefined, 'not given up on before the time is up');
    t.mock.timers.tick(1);
  });
  assert.equal(stuck.answer.status, 500);

  // Paddle never answering: the same, and the question is abandoned.
  const silent = await posting(() => never, () => never, async (seen) => {
    await until(() => seen.asked === 1);
    t.mock.timers.tick(4500);
  });
  assert.equal(silent.answer.status, 502);
  assert.ok(silent.question.aborted, 'the question to Paddle is abandoned');
  assert.equal(silent.mailed, 0);

  // The two share the time: after a slow answer from Paddle the mail has only what is left.
  const shared = await posting(slow, () => never, async (seen) => {
    await until(() => seen.asked === 1);
    t.mock.timers.tick(3000);
    await until(() => seen.mailed === 1);
    t.mock.timers.tick(1499);
    await turns();
    assert.equal(seen.answer, undefined, 'the mail had not yet used up what was left');
    t.mock.timers.tick(1);
  });
  assert.equal(shared.answer.status, 500);

  // And both in time is a key sent.
  const sent = await posting(slow, () => after(1000, { messageId: 'sent' }), async (seen) => {
    await until(() => seen.asked === 1);
    t.mock.timers.tick(3000);
    await until(() => seen.mailed === 1);
    t.mock.timers.tick(1000);
  });
  assert.equal(sent.answer.status, 200);
  assert.equal(sent.mailed, 1);
});

test('the keys offered are the app\'s three, at the prices the owner set, with a button each', () => {
  const offered = [...html.matchAll(/<h3>(\w+)<\/h3>\s*<b>\$(\d+)<\/b>[\s\S]*?<button class="btn" type="button" data-buy="(\w+)" disabled>Buy an? (\w+) key<\/button>/g)].map((match) => match.slice(1));
  assert.deepEqual(offered, [['Supporter', '5', 'supporter', 'Supporter'], ['Patron', '15', 'patron', 'Patron'], ['Founder', '30', 'founder', 'Founder']]);
  assert.deepEqual(offered.map(([, , tier]) => tier), Object.keys(TIERS));
  // Each icon named for a tier is that tier's in the app.
  const app = readFileSync(new URL('../crates/emu-app/src/supporter.rs', import.meta.url), 'utf8');
  assert.match(app, /Self::Ember => Tier::Patron,\s*Self::Founder => Tier::Founder,\s*_ => Tier::Supporter,/);
  assert.match(html, /the Night, Ink, Chalk and Blueprint icons/);
  assert.match(html, /and the Ember icon\./);
  assert.match(html, /and the Founder icon\./);
});

test('every page leads to the terms, privacy and refunds, and they say who sells and how long a refund is offered', () => {
  for (const [name, page] of Object.entries(pages)) {
    for (const policy of POLICIES) assert.ok(page.includes(`href="/${policy}"`), `${name} does not link to /${policy}`);
    assert.ok(page.includes(`mailto:${SUPPORT}`), `${name} gives no address to write to`);
  }
  assert.match(pages.terms, /made and run by Imago Emulator/);
  assert.match(pages.terms, /Paddle\.com is the Merchant of Record for all our orders/);
  for (const page of [html, pages.terms, pages.refunds]) assert.match(page, /refunded (?:in full )?within 30 days/);
  assert.doesNotMatch(Object.values(pages).join(''), /donat|\btip\b/i, 'a key is something bought, not a gift of money');
});

test('the front page and the terms say the app is open source, under the licence the repository gives it', () => {
  const license = readFileSync(new URL('../LICENSE', import.meta.url), 'utf8');
  assert.match(license, /^Mozilla Public License Version 2\.0\n/);
  for (const page of [html, pages.terms]) {
    assert.match(page, /open source/);
    assert.ok(page.includes(`href="${SOURCE}"`), 'no link to the source');
    assert.ok(page.includes(`<a href="${SOURCE}/blob/main/LICENSE">Mozilla Public License 2.0</a>`), 'the licence is not named and linked');
  }
  assert.match(html, /<nav>[\s\S]*?<a href="https:\/\/github\.com\/lilfourn\/Imago-Emulator">GitHub<\/a>\s*<\/nav>/, 'the header leads to GitHub');
  // The repository's README leads back to the same place.
  assert.ok(readFileSync(new URL('../README.md', import.meta.url), 'utf8').includes(`(${SOURCE})`));
});

/** A stand-in for a page: its elements by id, the buy buttons, and what was added to its head. */
function pageOf(ids) {
  const elements = Object.fromEntries(ids.map((id) => [id, { id, hidden: false, textContent: '', listeners: {}, addEventListener(type, heard) { this.listeners[type] = heard; } }]));
  return elements;
}
const settled = () => new Promise((resolve) => setTimeout(resolve, 0));

test('the buy buttons work only when keys are on sale, and open the checkout for their own key', async () => {
  const block = html.match(/\n  \{\n    const buttons = [\s\S]*?\n  \}\n/)[0];
  const run = async (answer, search = '') => {
    const buttons = Object.keys(TIERS).map((tier) => ({ dataset: { buy: tier }, disabled: true, addEventListener(type, heard) { this.press = heard; } }));
    const { sale: note } = pageOf(['sale']);
    note.textContent = 'Keys are not on sale yet.';
    const added = [];
    const went = [];
    const calls = [];
    const context = {
      URLSearchParams,
      encodeURIComponent,
      location: { search, assign: (to) => went.push(to) },
      fetch: async (path) => { calls.push(path); return answer; },
      document: {
        querySelectorAll: (selector) => (selector === 'button[data-buy]' ? buttons : []),
        getElementById: (id) => (id === 'sale' ? note : undefined),
        createElement: () => ({}),
        head: { append: (script) => added.push(script) },
      },
      Paddle: {
        Environment: { set: (name) => calls.push(['environment', name]) },
        Initialize: (options) => calls.push(['initialize', options]),
        Checkout: { open: (options) => calls.push(['open', options]) },
      },
    };
    runInNewContext(block, context);
    await settled();
    return { buttons, note, added, went, calls };
  };

  // Not on sale: the buttons stay as the page wrote them, and nothing is loaded.
  for (const answer of [new Response('Not available yet.\n', { status: 404 }), new Response('', { status: 500 })]) {
    const closed = await run(answer);
    assert.ok(closed.buttons.every((button) => button.disabled));
    assert.equal(closed.note.textContent, 'Keys are not on sale yet.');
    assert.deepEqual(closed.added, []);
    assert.deepEqual(closed.calls, ['/api/checkout']);
  }

  const shop = { environment: 'production', token: 'live_0123456789abcdef', prices: PRICES };
  const on = await run(Response.json(shop));
  assert.ok(on.buttons.every((button) => !button.disabled));
  assert.ok(on.note.hidden);
  assert.deepEqual(on.added, [], 'Paddle is not loaded until a button is pressed');

  on.buttons[1].press();
  assert.equal(on.added.length, 1);
  assert.equal(on.added[0].src, PADDLE_JS);
  on.added[0].onload();
  await settled();
  const initialized = on.calls.find(([name]) => name === 'initialize')[1];
  assert.equal(initialized.token, shop.token);
  assert.ok(!on.calls.some(([name]) => name === 'environment'), 'the live checkout is Paddle\'s default');
  // (What the page's script made is another realm's: compared as JSON.)
  assert.equal(JSON.stringify(on.calls.at(-1)[1].items), JSON.stringify([{ priceId: PRICES.patron, quantity: 1 }]));
  // A second press opens the checkout again without loading the script twice.
  on.buttons[2].press();
  await settled();
  assert.equal(on.added.length, 1);
  assert.equal(JSON.stringify(on.calls.at(-1)[1].items), JSON.stringify([{ priceId: PRICES.founder, quantity: 1 }]));
  // Paid: the buyer is taken to the page that shows the key.
  initialized.eventCallback({ name: 'checkout.loaded', data: {} });
  assert.deepEqual(on.went, []);
  initialized.eventCallback({ name: 'checkout.completed', data: { transaction_id: SALE } });
  assert.deepEqual(on.went, [`/thanks?txn=${SALE}`]);

  // The sandbox is named before Paddle is set up, and a script that does not load says so.
  const sandbox = await run(Response.json({ ...shop, environment: 'sandbox', token: 'test_0123456789abcdef' }));
  sandbox.buttons[0].press();
  sandbox.added[0].onload();
  await settled();
  assert.deepEqual(sandbox.calls.slice(1, 3).map(([name]) => name), ['environment', 'initialize']);
  const blocked = await run(Response.json(shop));
  blocked.buttons[0].press();
  blocked.added[0].onerror();
  await settled();
  assert.match(blocked.note.textContent, /did not load/);
  assert.equal(blocked.note.hidden, false);
  blocked.buttons[0].press();
  assert.equal(blocked.added.length, 2, 'pressing again tries again');

  // A link Paddle made for one sale has the checkout loaded at once.
  const linked = await run(Response.json(shop), `?_ptxn=${SALE}`);
  assert.equal(linked.added.length, 1);
});

test('the page a buyer lands on shows the key once the payment is taken', async () => {
  const block = pages.thanks.match(/<script>([\s\S]*?)<\/script>/)[1];
  const run = async (search, answers) => {
    const elements = pageOf(['title', 'say', 'help', 'key', 'ready', 'copy']);
    elements.ready.hidden = true;
    elements.help.hidden = true;
    const asked = [];
    const copied = [];
    runInNewContext(block, {
      URLSearchParams,
      encodeURIComponent,
      location: { search },
      setTimeout: (again) => setTimeout(again, 0),
      fetch: async (path) => { asked.push(path); return answers.length > 1 ? answers.shift() : answers[0]; },
      document: { getElementById: (id) => elements[id] },
      navigator: { clipboard: { writeText: async (text) => copied.push(text) } },
    });
    for (let turn = 0; turn < 60; turn += 1) await settled();
    return { elements, asked, copied };
  };
  const waiting = () => Response.json({ waiting: true }, { status: 202 });

  const shown = await run(`?txn=${SALE}`, [waiting(), waiting(), Response.json({ tier: 'Patron', key: 'IMAGO-abc' })]);
  assert.deepEqual(shown.asked, Array(3).fill(`/api/key?txn=${SALE}`));
  assert.equal(shown.elements.title.textContent, 'Your Patron key');
  assert.equal(shown.elements.key.textContent, 'IMAGO-abc');
  assert.equal(shown.elements.ready.hidden, false);
  await shown.elements.copy.listeners.click();
  assert.deepEqual(shown.copied, ['IMAGO-abc']);
  assert.equal(shown.elements.copy.textContent, 'Copied');

  // A payment that stays unconfirmed: the page stops asking and says the key will be mailed.
  const slow = await run(`?txn=${SALE}`, [waiting()]);
  assert.equal(slow.asked.length, 21);
  assert.equal(slow.elements.title.textContent, 'Your key is on its way');
  assert.equal(slow.elements.ready.hidden, true);
  assert.equal(slow.elements.help.hidden, false);

  // A fault that passes: asked again, and the key shown when it comes.
  const fault = () => new Response('error code: 1101', { status: 500 });
  const busy = () => Response.json({ error: 'asked too often' }, { status: 429 });
  const recovered = await run(`?txn=${SALE}`, [fault(), busy(), new Response('', { status: 502 }), Response.json({ tier: 'Founder', key: 'IMAGO-def' })]);
  assert.equal(recovered.elements.title.textContent, 'Your Founder key');
  // A fault that stays is never called "no such sale" to someone who paid.
  const broken = await run(`?txn=${SALE}`, [fault()]);
  assert.equal(broken.asked.length, 21);
  assert.equal(broken.elements.title.textContent, 'Your key is on its way');

  // No sale by that name, or no name at all.
  for (const status of [400, 404]) {
    const none = await run(`?txn=${SALE}`, [Response.json({ error: 'no such sale' }, { status })]);
    assert.equal(none.asked.length, 1);
    assert.equal(none.elements.title.textContent, 'No key here');
  }
  for (const search of ['', '?txn=', '?txn=txn_1', `?txn=${SALE}%2F..`]) {
    const unnamed = await run(search, [Response.json({ tier: 'Patron', key: 'IMAGO-abc' })]);
    assert.deepEqual(unnamed.asked, [], search);
    assert.equal(unnamed.elements.title.textContent, 'No key here');
    assert.equal(unnamed.elements.ready.hidden, true);
  }
});
