/**
 * Build-time configuration (Vite inlines `VITE_*`).
 *
 * Nothing secret belongs here, and nothing secret is needed: the panel holds
 * no credential of any kind. It is authenticated by the engine's session
 * cookie, which the browser attaches and only the engine can read.
 */

const str = (value: string | undefined, fallback: string): string =>
  value === undefined || value === '' ? fallback : value;

/**
 * The engine's GraphQL endpoint. Relative by default, because the panel is
 * served from the engine's own origin (nginx proxies `/v1/` and `/auth/` —
 * see `docker-compose.yml`). A session cookie only comes back to the origin
 * that set it, so this is not a deployment detail: it is what makes the panel
 * authenticated at all.
 */
export const GRAPHQL_URL = str(import.meta.env.VITE_DONAT_GRAPHQL_URL, '/v1/graphql');

/**
 * The role this panel runs as, unless a stand names its own.
 *
 * Every deployment calls its operator role something — `admin`, `support`,
 * `operator`, `staff` — so it is configuration, and the default is the word
 * most deployments use. Naming it here grants nothing: this engine has no
 * admin role, and the header only *selects* among the roles the signed-in
 * user's token already granted. What that role may do is decided entirely by
 * the per-role permissions in the deployment's own metadata; a role this panel
 * asserts but the deployment never declared simply sees nothing.
 */
export const DONAT_ROLE = str(import.meta.env.VITE_DONAT_ROLE, 'admin');

/**
 * The engine's own login routes. `/auth/login` redirects to the configured
 * identity provider and `/auth/callback` puts the provider's token in the
 * session cookie; the engine stores no users and issues no tokens of its own
 * (`knowledgebase/api-surfaces/decisions/010-donat-does-not-own-identity.md`).
 */
export const LOGIN_PATH = str(import.meta.env.VITE_DONAT_LOGIN_PATH, '/auth/login');
/** Reports the caller back to itself; see `AuthTransport.session`. */
export const SESSION_PATH = str(import.meta.env.VITE_DONAT_SESSION_PATH, '/auth/session');
export const LOGOUT_PATH = str(import.meta.env.VITE_DONAT_LOGOUT_PATH, '/auth/logout');

/**
 * The identity provider's own API, on this origin.
 *
 * `pages/idp-authorize.tsx` renders the provider's login screen in this
 * panel's own interface, and talks to the provider's unchanged endpoints
 * underneath. It has to reach them **same-origin** — the provider's session
 * cookie is `__Host-`-prefixed and its `allowed_origins` refuse a browser
 * calling in from elsewhere — so nginx proxies this prefix to it (see
 * `nginx.conf.template`), and the default is relative for the same reason
 * `GRAPHQL_URL` is.
 *
 * Set it to the empty string to switch that screen off; the engine then has to
 * send people to the provider's own page instead
 * (`DONAT_OIDC.authorization_endpoint`), which is a supported deployment and
 * the one every non-Rauthy provider uses today.
 */
export const IDP_BASE = str(import.meta.env.VITE_DONAT_IDP_BASE, '/auth/v1');

/**
 * Whether the sign-in screen offers to create an account.
 *
 * The provider decides whether anyone may — most deployments keep it closed
 * and have an operator create each account — and it tells nobody in advance:
 * there is no endpoint to ask, only a 403 when you try. So this is
 * configuration rather than detection, and the form still reports the refusal
 * plainly if a deployment sets it and the provider disagrees.
 */
export const IDP_REGISTRATION =
  str(import.meta.env.VITE_DONAT_IDP_REGISTRATION, 'false') === 'true';
