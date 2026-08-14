#!/usr/bin/env node
/**
 * Send this panel's palette to the identity provider's login page.
 *
 * The login belongs to the provider — that is what makes it able to offer a
 * second factor, a passkey and a password reset, and what keeps every
 * credential out of this panel. What it does not have to be is a different
 * interface: Rauthy serves a **per-client theme** as CSS custom properties, so
 * the same tokens the panel is built from can drive the page an operator signs
 * in on.
 *
 * This is a deploy-time step, like every other piece of donat configuration —
 * nothing here runs in a browser and nothing about it reaches the bundle.
 *
 *   node scripts/idp-theme.mjs \
 *     --url http://localhost:8081 --client petshop --api-key "$RAUTHY_API_KEY"
 *
 * The API key is an admin credential for the provider; create one in its admin
 * UI (or bootstrap it) and keep it wherever that deployment keeps secrets. The
 * script sends one request and stores nothing.
 *
 * `--dry-run` prints the payload instead, which is also how to see what a
 * palette change would do before doing it.
 */

/**
 * The panel's tokens, as HSL triples.
 *
 * Kept beside the values in `src/styles.css` rather than parsed out of it: the
 * provider names seven colours and the panel names forty, so this is a
 * deliberate mapping — which of ours plays each of theirs — and reading it
 * should not require reading a CSS parser.
 */
const LIGHT = {
  // --foreground / --background and their "high" contrast partners.
  text: [222, 47, 11],
  text_high: [222, 47, 4],
  bg: [0, 0, 100],
  bg_high: [210, 40, 96],
  // --primary is what a button is; --accent, what a link is.
  action: [222, 47, 11],
  accent: [222, 47, 30],
  error: [0, 84, 44],
  btn_text: 'white',
};

const DARK = {
  text: [210, 40, 96],
  text_high: [0, 0, 100],
  bg: [222, 47, 8],
  bg_high: [217, 33, 17],
  action: [210, 40, 96],
  accent: [215, 20, 65],
  error: [0, 63, 50],
  btn_text: 'hsl(222 47% 8%)',
};

/** Matches `--radius` in the panel's own stylesheet. */
const BORDER_RADIUS = '0.5rem';

const hsla = ([h, s, l], alpha) => `hsla(${h} ${s}% ${l}% / ${alpha})`;

function theme(colours) {
  return {
    ...colours,
    // The provider's light/dark toggle. Tinted from our own accent so the
    // control does not arrive from someone else's palette.
    theme_sun: hsla(colours.action, 0.7),
    theme_moon: hsla(colours.accent, 0.85),
  };
}

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    if (!flag.startsWith('--')) continue;
    const name = flag.slice(2);
    if (name === 'dry-run') {
      args.dryRun = true;
      continue;
    }
    args[name] = argv[index + 1];
    index += 1;
  }
  return args;
}

const args = parseArgs(process.argv.slice(2));
const client = args.client ?? process.env.IDP_CLIENT_ID;
const url = (args.url ?? process.env.IDP_URL ?? '').replace(/\/$/, '');
const apiKey = args['api-key'] ?? process.env.IDP_API_KEY;

if (!client || (!url && !args.dryRun)) {
  console.error('usage: idp-theme.mjs --url <idp> --client <client-id> [--api-key <key>] [--dry-run]');
  process.exit(2);
}

const payload = {
  client_id: client,
  light: theme(LIGHT),
  dark: theme(DARK),
  border_radius: BORDER_RADIUS,
};

if (args.dryRun) {
  console.log(JSON.stringify(payload, null, 2));
  process.exit(0);
}

if (!apiKey) {
  // Said plainly rather than attempted: the request would come back 401 with
  // a message about a session, which reads like a bug rather than a missing
  // credential.
  console.error(
    'The provider needs an admin credential to accept a theme. Pass --api-key, or --dry-run to see the payload.',
  );
  process.exit(2);
}

const response = await fetch(`${url}/auth/v1/theme/${encodeURIComponent(client)}`, {
  method: 'PUT',
  headers: {
    'content-type': 'application/json',
    // Rauthy's API-key scheme; a different provider is a different script.
    authorization: `API-Key ${apiKey}`,
  },
  body: JSON.stringify(payload),
});

if (!response.ok) {
  console.error(`the provider answered ${response.status}: ${await response.text()}`);
  process.exit(1);
}
console.log(`themed ${client} at ${url}`);
