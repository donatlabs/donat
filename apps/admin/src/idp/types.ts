/**
 * The identity provider's login API, as types.
 *
 * These mirror Rauthy's own generated TypeScript (`frontend/src/api/types/`,
 * Apache-2.0) because they describe *its* wire format, not ours. Keeping them
 * as a separate file makes that boundary visible: everything here is someone
 * else's contract, and the day it changes, this is the file that changes.
 *
 * Nothing about the OpenID Connect flow is re-invented here. The engine is
 * still the relying party — it holds the PKCE verifier, exchanges the code and
 * sets the session cookie — and the provider still mints every token. What
 * this module replaces is the provider's *markup*, and only that.
 */

export type CodeChallengeMethod = 'plain' | 'S256';

/** `POST /oidc/authorize` — a login attempt. */
export interface LoginRequest {
  email: string;
  /** Absent on the first attempt; the provider answers whether it wants one. */
  password?: string;
  /** A solved proof of work. Single-use — see `pow.ts`. */
  pow: string;
  client_id: string;
  redirect_uri: string;
  scopes?: string[];
  state?: string;
  nonce?: string;
  code_challenge?: string;
  code_challenge_method?: CodeChallengeMethod;
  /** RFC 8707 resource indicator, forwarded from the authorization request. */
  resource?: string;
}

/** `POST /users/request_reset` — "email me a reset link". */
export interface RequestResetRequest {
  email: string;
  redirect_uri?: string;
  pow: string;
}

/** `POST /users/register` — an account somebody asks for themselves. */
export interface RegisterRequest {
  email: string;
  given_name?: string;
  family_name?: string;
  /** A solved proof of work, as everywhere else the provider is asked. */
  pow: string;
  /** Where to send them once the account exists. */
  redirect_uri?: string;
}

/** `HTTP 200` from `/oidc/authorize`: the account wants a passkey next. */
export interface WebauthnLoginResponse {
  code: string;
  user_id: string;
  exp: number;
}

/**
 * `POST /users/webauthn_start` — the challenge itself.
 *
 * `code` is a second one, minted for the ceremony, and it is what
 * `webauthn_finish` is answered with; the `code` from HTTP 200 only got us
 * here. `exp` is how long the provider will still accept the signature, in
 * seconds, and it is what the browser's own timeout is set from.
 */
export interface WebauthnStartResponse {
  code: string;
  exp: number;
  user_id?: string;
  rcr: import('./webauthn').CredentialRequest;
}

/**
 * `GET /tos/latest` — the terms a deployment put in force.
 *
 * `ts` identifies the version and goes back with the answer, so that accepting
 * says *which* terms were accepted. `opt_until` is a deadline: until it passes,
 * these terms may be declined and the login still finishes. After it, they may
 * not, and the only honest screen is one with a single button.
 */
export interface Terms {
  content: string;
  is_html: boolean;
  ts: number;
  opt_until?: number;
}

export type SessionState = 'Init' | 'Auth' | 'LoggedOut' | 'Unknown';

/** `POST /oidc/session` — establishes the session and hands out the CSRF token. */
export interface SessionInfoResponse {
  id: string;
  csrf_token?: string;
  user_id?: string;
  roles?: string;
  groups?: string;
  exp: string;
  timeout: string;
  state: SessionState;
}

/** The provider's error body. */
export interface ErrorResponse {
  error: string;
  message: string;
}
