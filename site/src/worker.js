// The site's few dynamic routes. Files in public/ are served before this
// Worker runs, so it sees only paths with no file: the links that leave the
// site (a download, a checkout) and the page the app opens for supporters.
//
// Each link's address is a variable in wrangler.jsonc, so a new release or a
// new seller is a change of configuration and not of the page. A link whose
// variable is unset answers 404, in words, rather than pretending.

/** The paths that leave the site, and the variable holding each address. */
export const LINKS = {
  '/download/mac': 'DOWNLOAD_MAC',
  '/download/windows': 'DOWNLOAD_WINDOWS',
  '/download/linux': 'DOWNLOAD_LINUX',
  '/buy/supporter': 'BUY_SUPPORTER',
  '/buy/patron': 'BUY_PATRON',
  '/buy/founder': 'BUY_FOUNDER',
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

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const path = url.pathname.replace(/\/+$/, '');
    // Settings > Supporter > Become a Supporter opens this address
    // (crates/emu-app/src/supporter.rs, PAGE).
    if (path === '/supporter') return Response.redirect(new URL('/#supporters', url).href, 302);
    const variable = LINKS[path];
    if (!variable) return env.ASSETS.fetch(request);
    const target = address(env[variable]);
    if (!target) {
      return new Response('Not available yet.\n', {
        status: 404,
        headers: { 'content-type': 'text/plain; charset=utf-8', 'cache-control': 'no-store' },
      });
    }
    return new Response(null, { status: 302, headers: { location: target, 'cache-control': 'no-store' } });
  },
};
