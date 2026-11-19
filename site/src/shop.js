// Selling supporter keys through Paddle, which is the seller of record: it
// takes the payment, the tax and the refunds, and tells this Worker of each
// sale. The Worker's part is to sign the key (keys.js) and hand it over,
// on the page a buyer lands on and by email.
//
// Nothing is stored. A key is made from its sale each time it is asked
// for, and one sale always makes the same key, so a notice delivered twice
// or a page opened again gives the buyer the key they already have.
//
// Paddle's side is described at developer.paddle.com: the checkout
// (paddle-js), a transaction (api-reference/transactions), and how a
// notice is signed (webhooks/signature-verification).

import { dayOf, idOf, issue, TIERS } from './keys.js';

/** Where Paddle's API is, for each of its two environments. */
const API = { production: 'https://api.paddle.com', sandbox: 'https://sandbox-api.paddle.com' };
/** What a client-side token and an API key start with in each. */
const MARKS = { production: ['live_', 'pdl_live_apikey_'], sandbox: ['test_', 'pdl_sdbx_apikey_'] };
/** A transaction's id, and a price's. */
const TRANSACTION = /^txn_[a-z\d]{26}$/;
const PRICE = /^pri_[a-z\d]{26}$/;
/** The states of a transaction whose payment has been taken, and of one that may yet be paid for. */
const SOLD = new Set(['paid', 'completed']);
const PENDING = new Set(['draft', 'ready', 'billed', 'past_due']);
/**
 * How long a notice may take in all, in milliseconds, Paddle's answer and
 * the mail together: Paddle waits five seconds, then counts the notice
 * failed and sends it again.
 */
const PATIENCE = 4500;
/** Who a key's email is from, and where a reply goes. */
const FROM = { email: 'keys@imagoemulator.com', name: 'Imago Emulator' };
export const SUPPORT = 'support@imagoemulator.com';
/** What each tier is called. */
const NAMES = { supporter: 'Supporter', patron: 'Patron', founder: 'Founder' };

/**
 * What selling needs, from the Worker's variables and secrets, or nothing
 * if any of it is missing or is another environment's: keys are either on
 * sale whole or not at all, so that no payment is taken for a key that
 * cannot be made or sent.
 */
export function shop(env) {
  const environment = env.PADDLE_ENVIRONMENT;
  if (!Object.hasOwn(API, environment)) return undefined;
  const [token, apiKey] = MARKS[environment];
  const text = (value) => (typeof value === 'string' ? value.trim() : '');
  const found = {
    environment,
    api: API[environment],
    token: text(env.PADDLE_CLIENT_TOKEN),
    apiKey: text(env.PADDLE_API_KEY),
    secret: text(env.PADDLE_WEBHOOK_SECRET),
    seed: text(env.SUPPORTER_SIGNING_KEY),
    mail: env.EMAIL,
    // How often one asker may ask for a key: unset where the Worker is tried locally.
    limit: typeof env.KEY_ASKERS?.limit === 'function' ? env.KEY_ASKERS : undefined,
    prices: {
      supporter: text(env.PADDLE_PRICE_SUPPORTER),
      patron: text(env.PADDLE_PRICE_PATRON),
      founder: text(env.PADDLE_PRICE_FOUNDER),
    },
  };
  const prices = Object.values(found.prices);
  const whole =
    found.token.startsWith(token) &&
    found.apiKey.startsWith(apiKey) &&
    found.secret !== '' &&
    // A signing key that could sign nothing is found out here, not after a payment.
    seedLength(found.seed) === 32 &&
    typeof found.mail?.send === 'function' &&
    prices.every((price) => PRICE.test(price)) &&
    new Set(prices).size === prices.length;
  return whole ? found : undefined;
}

/** How many bytes `seed` holds, which is base64, or nothing if it is not. */
function seedLength(seed) {
  try {
    return atob(seed).length;
  } catch {
    return undefined;
  }
}

/** `body` as a JSON answer that is never kept. */
function json(body, status = 200) {
  return new Response(JSON.stringify(body) + '\n', {
    status,
    headers: { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store' },
  });
}

/** What the page needs to open a checkout: nothing in it is a secret. */
export function checkout(selling) {
  return json({ environment: selling.environment, token: selling.token, prices: selling.prices });
}

/** `bytes` in lower-case hexadecimal. */
function hex(bytes) {
  return [...new Uint8Array(bytes)].map((byte) => byte.toString(16).padStart(2, '0')).join('');
}

/** Whether `a` and `b` are the same text, in time that does not say where they differ. */
function same(a, b) {
  if (a.length !== b.length) return false;
  let difference = 0;
  for (let at = 0; at < a.length; at += 1) difference |= a.charCodeAt(at) ^ b.charCodeAt(at);
  return difference === 0;
}

/**
 * Whether `header`, a notice's `Paddle-Signature`, signs `body` with
 * `secret`: `ts=TIME;h1=HEX`, where HEX is the HMAC-SHA256 of `TIME:body`.
 * There may be more than one `h1` while Paddle changes a secret, and any
 * that matches will do.
 *
 * The time is not held to a window. Paddle does not say whether a notice
 * sent again keeps its first time, and a notice replayed here can do no
 * more than send a buyer the key they already have.
 */
export async function signed(secret, header, body) {
  const parts = (header ?? '').split(';').map((part) => part.split('='));
  const time = parts.find(([name]) => name === 'ts')?.[1];
  const offered = parts.filter(([name]) => name === 'h1').map(([, value]) => value ?? '');
  if (!time || !/^\d+$/.test(time) || offered.length === 0) return false;
  const encoder = new TextEncoder();
  const key = await crypto.subtle.importKey('raw', encoder.encode(secret), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  const expected = hex(await crypto.subtle.sign('HMAC', key, encoder.encode(`${time}:${body}`)));
  // Every one offered is compared, so that the time taken says nothing of which matched.
  return offered.map((value) => same(value.toLowerCase(), expected)).includes(true);
}

/**
 * The transaction `id` as Paddle has it, with its customer when `customer`
 * is set: `{ found }` with the transaction, `{ missing: true }` if Paddle
 * knows none by that id, and otherwise `{ failed }` with what went wrong.
 */
async function transaction(selling, id, { customer = false, by = Date.now() + PATIENCE } = {}) {
  const stop = new AbortController();
  try {
    const asked = fetch(`${selling.api}/transactions/${id}${customer ? '?include=customer' : ''}`, {
      headers: { authorization: `Bearer ${selling.apiKey}`, 'paddle-version': '1' },
      signal: stop.signal,
    }).then(async (answer) => {
      if (answer.status === 404) return { missing: true };
      if (!answer.ok) return { failed: `Paddle answered ${answer.status}` };
      const found = (await answer.json())?.data;
      // The id is checked against the answer, so that one sale cannot stand for another.
      return found?.id === id ? { found } : { failed: 'Paddle answered with another transaction' };
    });
    return await within(by, asked);
  } catch (error) {
    stop.abort();
    return { failed: `Paddle did not answer (${error?.name ?? 'error'})` };
  }
}

/**
 * The tier `sale`, a transaction, bought: the highest of the keys among
 * its items, or nothing if its payment has not been taken or it holds no
 * key at all.
 */
function tierOf(selling, sale) {
  if (!SOLD.has(sale.status) || !Array.isArray(sale.items)) return undefined;
  const bought = Object.keys(TIERS).filter((tier) => sale.items.some((item) => item?.price?.id === selling.prices[tier]));
  return bought.sort((a, b) => TIERS[b] - TIERS[a])[0];
}

/**
 * The key for `sale`, which bought `tier`. Its number is made from the
 * transaction's id and its day is the day the transaction began, neither
 * of which changes, so that the sale makes this key every time.
 */
async function keyFor(selling, sale, tier) {
  const begun = new Date(sale.created_at);
  if (Number.isNaN(begun.getTime())) throw new Error('the transaction has no date');
  return issue(selling.seed, { tier, id: await idOf(sale.id), day: dayOf(begun) });
}

/**
 * The key a transaction bought, for the page a buyer lands on after
 * paying. A transaction's id is the buyer's proof: Paddle makes it up at
 * random and gives it to nobody else.
 */
export async function key(selling, url, request) {
  const id = url.searchParams.get('txn') ?? '';
  if (!TRANSACTION.test(id)) return json({ error: 'no such sale' }, 400);
  // Each answer costs a question to Paddle, so one asker gets only so many
  // a minute: a flood of made-up sales must not use up what real ones need.
  const asker = request.headers.get('cf-connecting-ip') ?? 'nobody';
  if (selling.limit && !(await selling.limit.limit({ key: asker })).success) return json({ error: 'asked too often' }, 429);
  const { found: sale, missing, failed } = await transaction(selling, id);
  if (missing) return json({ error: 'no such sale' }, 404);
  if (failed) {
    console.error(`key: ${failed}`);
    return json({ error: 'the seller did not answer' }, 502);
  }
  // A payment that is still being taken is asked about again.
  if (PENDING.has(sale.status)) return json({ waiting: true }, 202);
  const tier = tierOf(selling, sale);
  if (!tier) return json({ error: 'no such sale' }, 404);
  try {
    return json({ tier: NAMES[tier], key: await keyFor(selling, sale, tier) });
  } catch (error) {
    // Paid for, and no key: said as a fault of ours, which it is.
    console.error(`key: a paid sale's key was not made (${error?.message ?? 'error'})`);
    return json({ error: 'the key could not be made' }, 500);
  }
}

/** The email that carries `key`, a `tier` key bought in transaction `id`. */
export function letter(tier, key, id) {
  const page = `https://imagoemulator.com/thanks?txn=${id}`;
  const text = [
    'Thank you for supporting Imago Emulator.',
    '',
    `Your ${NAMES[tier]} key:`,
    '',
    key,
    '',
    'To use it, copy the key, open Imago Emulator, choose Settings, and under Supporter press Paste Key.',
    '',
    'The key works without the internet, on each of your own computers, for as long as the app does. Keep this email: it is your copy of the key. The key is also at',
    page,
    '',
    `Paddle, which sold you the key, sends your receipt separately. For a refund or any trouble, reply to this email or write to ${SUPPORT}.`,
    '',
  ].join('\n');
  const html = [
    '<div style="font-family:system-ui,-apple-system,Segoe UI,sans-serif;font-size:16px;line-height:1.5;color:#0b0b0d;max-width:560px">',
    '<p>Thank you for supporting Imago Emulator.</p>',
    `<p>Your ${NAMES[tier]} key:</p>`,
    `<p style="font-family:ui-monospace,Menlo,Consolas,monospace;font-size:14px;padding:14px 16px;border-radius:12px;background:#f4f4f5;word-break:break-all">${key}</p>`,
    '<p>To use it, copy the key, open Imago Emulator, choose Settings, and under Supporter press Paste Key.</p>',
    `<p>The key works without the internet, on each of your own computers, for as long as the app does. Keep this email: it is your copy of the key. The key is also on <a href="${page}">its page</a>.</p>`,
    `<p>Paddle, which sold you the key, sends your receipt separately. For a refund or any trouble, reply to this email or write to <a href="mailto:${SUPPORT}">${SUPPORT}</a>.</p>`,
    '</div>',
  ].join('\n');
  return { from: FROM, replyTo: SUPPORT, subject: `Your Imago Emulator ${NAMES[tier]} key`, text, html };
}

/** What `promise` settles to, or a failure if it has not by the time `by`. */
async function within(by, promise) {
  let timer;
  const late = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error('it took too long')), Math.max(0, by - Date.now()));
  });
  try {
    return await Promise.race([promise, late]);
  } finally {
    clearTimeout(timer);
  }
}

/** A notice's answer: Paddle reads only the status, and sends again unless it is 200. */
function noted(what, status = 200) {
  return new Response(`${what}\n`, { status, headers: { 'content-type': 'text/plain; charset=utf-8', 'cache-control': 'no-store' } });
}

/**
 * Paddle's notice of something that happened. A completed transaction
 * that bought a key has the key sent to its buyer.
 *
 * The notice only says which transaction to look at: what was bought, and
 * by whom, is read from Paddle itself. Anything that might yet succeed
 * answers 5xx, so that Paddle sends the notice again (for two days), and
 * what never will answers 200.
 */
export async function notice(selling, request) {
  if (request.method !== 'POST') return noted('Notices are posted.', 405);
  const body = await request.text();
  if (!(await signed(selling.secret, request.headers.get('paddle-signature'), body))) return noted('Not signed by Paddle.', 401);
  let event;
  try {
    event = JSON.parse(body);
  } catch {
    return noted('Not a notice.');
  }
  const id = event?.data?.id;
  if (event?.event_type !== 'transaction.completed' || typeof id !== 'string' || !TRANSACTION.test(id)) return noted('Nothing to do.');

  // Paddle's answer and the mail share one deadline, inside Paddle's five seconds.
  const by = Date.now() + PATIENCE;
  const { found: sale, missing, failed } = await transaction(selling, id, { customer: true, by });
  // Paddle's own test notices name transactions that do not exist.
  if (missing) return noted('No such transaction.');
  if (failed) {
    console.error(`notice: ${failed}`);
    return noted('Paddle did not say what was sold.', 502);
  }
  const tier = tierOf(selling, sale);
  if (!tier) return noted('No key was sold.');
  const to = sale.customer?.email;
  if (typeof to !== 'string' || !to.includes('@')) {
    // A sale has a buyer, so the address is asked for again: it is missing
    // if the API key may not read customers, which can be put right.
    console.error('notice: the sale came without its buyer\'s address');
    return noted('Nobody to write to yet.', 502);
  }
  try {
    await within(by, selling.mail.send({ ...letter(tier, await keyFor(selling, sale, tier), id), to }));
  } catch (error) {
    console.error(`notice: the key was not sent (${error?.code ?? error?.message ?? 'error'})`);
    return noted('The key was not sent.', 500);
  }
  return noted('Sent.');
}
