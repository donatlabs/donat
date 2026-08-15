import { csrfToken } from './csrf';
/**
 * How a request from this panel is authenticated.
 *
 * There is one way, and it is the same way every other client of this engine
 * is authenticated: a verified token. The panel does not hold that token — the
 * engine's `/auth/callback` put it in an `HttpOnly` session cookie that no
 * script can read, and the browser attaches it because every request goes to
 * the engine's own origin (`credentials: 'include'`).
 *
 * So there is nothing here to store, refresh or clear. What is left is the
 * one header that still matters — `X-Donat-Role`, which *selects* among the
 * roles the token already grants and can never add one — and what to do when
 * the engine says the session is gone.
 */

import { DONAT_ROLE, LOGIN_PATH, LOGOUT_PATH, SESSION_PATH } from '../env';

/** What to attach to one outbound GraphQL request. */
export interface RequestAuth {
  readonly headers: Record<string, string>;
  readonly credentials: RequestCredentials;
}

/** What the engine says about the caller it is looking at. */
export interface SessionState {
  readonly authenticated: boolean;
  /** The role this request runs as — the unauthorized one when not signed in. */
  readonly role: string | null;
  /**
   * Every role the caller's token grants.
   *
   * The panel asks the engine to act as one particular role (a stand *is* a
   * role), and a token that does not grant it is refused. Knowing the list is
   * what lets the panel say "this account cannot use this panel" instead of
   * showing an error with a Retry button that can never work.
   *
   * `undefined` means the engine did not say — an older one — which is not the
   * same as an empty list, and must not be read as "this account holds none".
   */
  readonly roles: readonly string[] | undefined;
}

export interface AuthTransport {
  /** Headers + credentials for one request, as the given role. */
  authorize(role: string): RequestAuth;
  /**
   * Ask the engine who this browser is.
   *
   * Necessary rather than convenient: a deployment that sets an unauthorized
   * role answers an unauthenticated request *successfully*, as that role, so
   * "not signed in" and "this role may see nothing" are identical from here.
   * Without asking, the panel would render an empty screen instead of sending
   * the operator to log in.
   */
  session(): Promise<SessionState>;
  /**
   * Called after the engine rejects a request with 401/403. Nothing here can
   * silently re-authenticate — only the identity provider can — so this hands
   * the browser to the login route and answers false: the caller must not
   * retry.
   */
  recover(): Promise<boolean>;
  /** End the session at the engine, and at the provider when it supports it. */
  signOut(): void;
}

export function createTransport(defaultRole: string = DONAT_ROLE): AuthTransport {
  return {
    // The role is per request because the panel serves several stands, and a
    // stand IS a role: which one is asked for changes with the stand the
    // operator is looking at. It still only selects among the roles the
    // session's token already granted — asking for another is denied.
    //
    // Empty means *let the token decide*. A deployment that grants its
    // operator exactly one role has nothing to select between, and asking it
    // to name that role here is asking it to repeat, at build time, something
    // the token already says — which then has to be kept in step with an
    // identity provider it does not control. Sending no header is not a
    // weaker request: the engine reads the token's own default role, and a
    // role that token never granted is refused either way.
    authorize: (role = defaultRole) => ({
      headers: {
        ...(role ? { 'X-Donat-Role': role } : {}),
        // The identity provider's CSRF token, when this browser has one.
        //
        // The identity fields reach the provider carrying *this* session
        // rather than a credential of the deployment's, and the provider
        // refuses a write from a session that cannot prove it meant to make
        // it. The token is not a secret — it is proof the request came from a
        // page, not from a link somebody was sent — and it is read from the
        // provider's own `sessioninfo`, on this origin, by `csrf.ts`.
        ...(csrfToken() ? { 'x-csrf-token': csrfToken()! } : {}),
      } as Record<string, string>,
      credentials: 'include',
    }),
    async session() {
      try {
        const response = await fetch(SESSION_PATH, { credentials: 'include' });
        if (!response.ok) return { authenticated: false, role: null, roles: undefined };
        const body = (await response.json()) as Partial<SessionState>;
        return {
          authenticated: body.authenticated === true,
          role: typeof body.role === 'string' ? body.role : null,
          // Absent stays absent: an older engine saying nothing is not an
          // account holding nothing, and only one of those is a refusal.
          roles: Array.isArray(body.roles)
            ? body.roles.filter((role): role is string => typeof role === 'string' && role !== '')
            : undefined,
        };
      } catch {
        // An engine that cannot be reached is not a signed-out browser; say
        // so rather than bouncing the operator to a login that will not help.
        return { authenticated: false, role: null, roles: undefined };
      }
    },
    async recover() {
      signIn();
      return false;
    },
    signOut() {
      window.location.assign(LOGOUT_PATH);
    },
  };
}

/**
 * Start a login.
 *
 * A full-page navigation, not a fetch: the provider needs the browser itself,
 * and the engine sets a cookie the panel is never allowed to see. `redirect`
 * comes back through the engine, which accepts only a path on its own origin.
 *
 * The panel renders no credential form of its own. It was tried and dropped:
 * a first-party form has to reach the provider from the page, which its
 * `allowed_origins` refuse, and the grant that makes it possible at all has no
 * way to offer a second factor, a passkey or a recovery. The provider's page
 * carries those, so the panel's styling reaches it instead — see
 * `scripts/idp-theme.mjs`.
 */
export function signIn(returnTo: string = window.location.pathname): void {
  window.location.assign(`${LOGIN_PATH}?redirect=${encodeURIComponent(returnTo)}`);
}
