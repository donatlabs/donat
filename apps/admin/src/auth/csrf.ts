/**
 * The identity provider's CSRF token, kept for as long as this page lives.
 *
 * The panel's identity screens reach the provider through the engine, and the
 * request carries the browser's own provider session rather than a credential
 * of the deployment's — which is the point: the provider then decides what
 * this person may do to whose account, instead of a shared key deciding it for
 * everyone. What the provider asks for in return is proof the request came
 * from a page rather than from a link somebody was sent, and that proof is
 * this token.
 *
 * It is not a secret. It is readable by any script on this origin by design —
 * the provider hands it to its own frontend the same way — and it is worth
 * exactly nothing without the cookie, which no script can read.
 */
const XSRF_PATH = '/auth/v1/oidc/sessioninfo/xsrf';

let token: string | undefined;

/** What to send, if anything is known yet. */
export function csrfToken(): string | undefined {
  return token;
}

/**
 * Ask the provider for one.
 *
 * Failure is not an error to show: a deployment whose identity fields act as
 * the deployment needs no token at all, and one whose session has ended has
 * bigger news coming from the next request.
 */
export async function loadCsrfToken(fetchImpl: typeof fetch = fetch): Promise<void> {
  try {
    const response = await fetchImpl(XSRF_PATH, { credentials: 'same-origin' });
    if (!response.ok) return;
    const body = (await response.json()) as { csrf_token?: string };
    if (body.csrf_token) token = body.csrf_token;
  } catch {
    // No provider on this origin, or none that answers. Nothing to send.
  }
}

/** For tests, and for signing out. */
export function forgetCsrfToken(): void {
  token = undefined;
}
