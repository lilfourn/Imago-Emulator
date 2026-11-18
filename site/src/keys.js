// Supporter keys, written as crates/emu-supporter reads them (its module
// documentation gives the format, and docs/decisions/0017-supporter-keys.md
// the reasons). The app checks a key on the computer it is pasted into, so
// whatever sells keys only has to sign them: this is that signing, with
// WebCrypto, for the Worker to call once a sale is paid for.

/** What every key starts with. */
export const PREFIX = 'IMAGO-';
/** The one version of the format there is. */
const VERSION = 1;
/** The longest name a key holds, in bytes of UTF-8. */
export const MAX_NAME = 64;
/** What is signed before a key's own bytes. */
const CONTEXT = new TextEncoder().encode('imago-emulator supporter key\0');
/** Each tier's byte in a key. */
export const TIERS = { supporter: 1, patron: 2, founder: 3 };
/** What comes before an Ed25519 seed in PKCS #8 (RFC 8410). */
const PKCS8 = Uint8Array.from([0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20]);

/** The bytes of `text`, which is base64. */
function fromBase64(text) {
  return Uint8Array.from(atob(text.trim()), (character) => character.charCodeAt(0));
}

/** `bytes` in URL-safe base64 without padding. */
function toBase64Url(bytes) {
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
}

/** The days from 1970-01-01 to `date`. */
export function dayOf(date) {
  return Math.floor(date.getTime() / 86_400_000);
}

/**
 * A key's number for the sale `reference` (an order's id): the first eight
 * bytes of its SHA-256, so that asking again for one sale gives one key.
 */
export async function idOf(reference) {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(reference));
  return new DataView(digest).getBigUint64(0);
}

/**
 * A `tier` key numbered `id` (a BigInt), issued on `day` to `name` (which
 * may be empty), signed with the signing key `seed`: its 32 bytes in
 * base64, as `supporter-key keygen` writes them.
 *
 * Throws if the tier is unknown, the seed is not one, or the name does not
 * fit a key.
 */
export async function issue(seed, { tier, id, day, name = '' }) {
  if (!(tier in TIERS)) throw new Error(`no such tier: ${tier}`);
  const secret = fromBase64(seed);
  if (secret.length !== 32) throw new Error('the signing key is not 32 bytes');
  const named = new TextEncoder().encode(name.trim());
  if (named.length > MAX_NAME) throw new Error('the name is too long for a key');
  // Unicode's control characters, as the Rust `char::is_control` has them.
  if (/\p{Cc}/u.test(name.trim())) throw new Error('the name has a control character');

  const body = new Uint8Array(14 + named.length);
  const view = new DataView(body.buffer);
  view.setUint8(0, VERSION);
  view.setUint8(1, TIERS[tier]);
  view.setBigUint64(2, BigInt.asUintN(64, id));
  view.setUint32(10, day);
  body.set(named, 14);

  const der = new Uint8Array(PKCS8.length + 32);
  der.set(PKCS8);
  der.set(secret, PKCS8.length);
  const signer = await crypto.subtle.importKey('pkcs8', der, { name: 'Ed25519' }, false, ['sign']);
  const message = new Uint8Array(CONTEXT.length + body.length);
  message.set(CONTEXT);
  message.set(body, CONTEXT.length);
  const signature = new Uint8Array(await crypto.subtle.sign('Ed25519', signer, message));

  const key = new Uint8Array(body.length + signature.length);
  key.set(body);
  key.set(signature, body.length);
  return PREFIX + toBase64Url(key);
}
