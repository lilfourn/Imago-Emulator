// The site's few dynamic routes. Files in public/ are served before this
// Worker runs, so it sees only paths with no file: the downloads, the page
// the app opens for supporters, and the selling of their keys (shop.js).
//
// Each download's address is a variable in wrangler.jsonc, so a new release
// is a change of configuration and not of the page. A link whose variable
// is unset answers 404, in words, rather than pretending, and so does
// everything to do with keys until all of what selling needs is set.

import { checkout, key, notice, shop } from './shop.js';

/** The paths that leave the site, and the variable holding each address. */
export const LINKS = {
  '/download/mac': 'DOWNLOAD_MAC',
  // The installer, and the same files as a zip to run from any folder.
  '/download/windows': 'DOWNLOAD_WINDOWS',
  '/download/windows-zip': 'DOWNLOAD_WINDOWS_ZIP',
  // The AppImage and the .deb, for x86-64 and for arm64.
  '/download/linux': 'DOWNLOAD_LINUX',
  '/download/linux-deb': 'DOWNLOAD_LINUX_DEB',
  '/download/linux-arm64': 'DOWNLOAD_LINUX_ARM64',
  '/download/linux-arm64-deb': 'DOWNLOAD_LINUX_ARM64_DEB',
};

/**
 * The paths that sell keys: what the page asks before opening a checkout,
 * what the page a buyer lands on asks for their key, and where Paddle
 * posts its notices.
 */
export const SELLING = {
  '/api/checkout': (selling) => checkout(selling),
  '/api/key': (selling, request, url) => key(selling, url, request),
  '/api/paddle': (selling, request) => notice(selling, request),
};

/** `value` if it is an https address, or else nothing. */
function address(value) {
  if (typeof value !== 'string' || value === '') return undefined;
  try {
    const url = new URL(value);
    return url.protocol === 'https:' ? url.href : undefined;
  } catch {
    return undefined;
  }
}

/** The answer for what is not there, or not yet. */
function unavailable() {
  return new Response('Not available yet.\n', {
    status: 404,
    headers: { 'content-type': 'text/plain; charset=utf-8', 'cache-control': 'no-store' },
  });
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const path = url.pathname.replace(/\/+$/, '');
    // Settings > Supporter > Become a Supporter opens this address
    // (crates/emu-app/src/supporter.rs, PAGE).
    if (path === '/supporter') return Response.redirect(new URL('/#supporters', url).href, 302);
    if (Object.hasOwn(SELLING, path)) {
      const selling = shop(env);
      return selling ? SELLING[path](selling, request, url) : unavailable();
    }
    if (!Object.hasOwn(LINKS, path)) return env.ASSETS.fetch(request);
    const target = address(env[LINKS[path]]);
    if (!target) return unavailable();
    return new Response(null, { status: 302, headers: { location: target, 'cache-control': 'no-store' } });
  },
};
