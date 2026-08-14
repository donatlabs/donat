/**
 * Reading the authorization request the engine started.
 *
 * `GET /auth/login` on the engine mints `state` and a PKCE verifier, keeps
 * them in a short-lived cookie of its own, and redirects the browser to the
 * provider's authorization endpoint with the public half in the query. Point
 * that endpoint at this page (`DONAT_OIDC.authorization_endpoint`) and the same
 * query arrives here instead — so this page is a rendering of the request, and
 * carries every parameter through to the provider untouched.
 *
 * Nothing is invented here. A parameter this page cannot see is a parameter the
 * provider will not get, which is why a request missing `client_id` or
 * `redirect_uri` is an error rather than a default.
 */
import type { CodeChallengeMethod, LoginRequest } from './types';

export interface AuthorizeParams {
  clientId: string;
  redirectUri: string;
  scopes: string[];
  state?: string;
  nonce?: string;
  codeChallenge?: string;
  codeChallengeMethod?: CodeChallengeMethod;
  resource?: string;
  /** `login_hint`, when the caller already knows who is signing in. */
  loginHint?: string;
}

const some = (value: string | null): string | undefined =>
  value === null || value === '' ? undefined : value;

export function parseAuthorizeParams(search: string): AuthorizeParams | undefined {
  const query = new URLSearchParams(search);
  const clientId = some(query.get('client_id'));
  const redirectUri = some(query.get('redirect_uri'));
  if (!clientId || !redirectUri) return undefined;

  const method = some(query.get('code_challenge_method'));
  return {
    clientId,
    redirectUri,
    scopes: some(query.get('scope'))?.split(' ').filter(Boolean) ?? [],
    state: some(query.get('state')),
    nonce: some(query.get('nonce')),
    codeChallenge: some(query.get('code_challenge')),
    codeChallengeMethod:
      method === 'plain' || method === 'S256' ? (method as CodeChallengeMethod) : undefined,
    resource: some(query.get('resource')),
    loginHint: some(query.get('login_hint')),
  };
}

/**
 * The login payload for these parameters.
 *
 * `state` is percent-encoded on the way out, as the provider's own page does:
 * it comes back verbatim in the redirect the engine has to match against its
 * flow cookie, so it must survive a round trip through a URL.
 */
export function loginRequest(
  params: AuthorizeParams,
  credentials: { email: string; password?: string; pow: string },
): LoginRequest {
  const request: LoginRequest = {
    email: credentials.email,
    pow: credentials.pow,
    client_id: params.clientId,
    redirect_uri: params.redirectUri,
    scopes: params.scopes,
    state: params.state === undefined ? undefined : encodeURIComponent(params.state),
    nonce: params.nonce,
  };
  if (credentials.password) request.password = credentials.password;
  if (params.codeChallenge && params.codeChallengeMethod) {
    request.code_challenge = params.codeChallenge;
    request.code_challenge_method = params.codeChallengeMethod;
  }
  if (params.resource) request.resource = params.resource;
  return request;
}

/**
 * Where to send someone this page cannot finish serving.
 *
 * A passkey, a terms update or a forced enrolment are screens the provider
 * still owns, and its own page is one proxied `GET` away — with this exact
 * request, so nothing is lost by going there.
 */
export function providerPageUrl(base: string, search: string): string {
  return `${base.replace(/\/$/, '')}/oidc/authorize${search}`;
}
