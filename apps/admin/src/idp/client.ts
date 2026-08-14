/**
 * The provider's login endpoints, as one small client.
 *
 * Every request here goes to the identity provider unchanged — same paths,
 * same payloads, same CSRF header, same status codes as its own page uses. The
 * panel is not in the middle of the flow: it never sees a token, and the code
 * the provider issues travels back to the **engine's** `/auth/callback`, which
 * is the only party holding the PKCE verifier.
 *
 * It must be reached on this origin (nginx proxies `/auth/v1/` to the
 * provider). That is not a deployment nicety: the provider's session cookie is
 * `__Host-`-prefixed, and its `allowed_origins` refuse a browser calling it
 * from somewhere else. A same-origin proxy makes both a non-issue without
 * asking an operator to widen anything.
 */
import type {
  ErrorResponse,
  LoginRequest,
  RegisterRequest,
  RequestResetRequest,
  SessionInfoResponse,
  Terms,
  WebauthnLoginResponse,
  WebauthnStartResponse,
} from './types';
import type { WebauthnPurpose } from './webauthn';

/**
 * What `POST /oidc/authorize` can mean.
 *
 * The provider answers with a status rather than a body, and the mapping is
 * its, not ours — see `handleAuthRes` in its own login page.
 */
export type AuthorizeOutcome =
  /** 202 — done. The `Location` header carries the code back to the engine. */
  | { kind: 'redirect'; location: string }
  /** 200 — credentials accepted, a passkey is still required. */
  | { kind: 'passkey'; challenge: WebauthnLoginResponse }
  /** 205 — a password-only account the provider wants to update first. */
  | { kind: 'update-required' }
  /** 206 — signed in, but new terms have to be accepted. */
  | { kind: 'terms-required'; code: string }
  /** 406 — the client demands a second factor the account does not have. */
  | { kind: 'mfa-required' }
  /** 403 with `PasswordRefresh` — the password has expired. */
  | { kind: 'password-expired' }
  /** 429 — too many failed attempts; `notBefore` is a unix timestamp. */
  | { kind: 'rate-limited'; notBefore: number | undefined }
  /** 400/403 — refused, with the provider's own wording. */
  | { kind: 'rejected'; message: string }
  /**
   * Anything else — in practice 401.
   *
   * Deliberately not called "wrong password": the provider answers exactly the
   * same way to an email it has never seen, to one whose account needs a
   * password it was not given, and to a wrong password. Which of those it is
   * depends on what the page has already asked for, so the page decides.
   */
  | { kind: 'unauthorized' };

export type RegisterOutcome =
  /** The provider took it and is emailing them a link to set a password. */
  | { kind: 'sent' }
  /** The deployment does not let people register themselves. */
  | { kind: 'closed' }
  | { kind: 'rate-limited' }
  | { kind: 'rejected'; message: string };

export type ResetOutcome =
  | { kind: 'sent' }
  | { kind: 'rate-limited' }
  | { kind: 'rejected'; message: string };

export class IdpError extends Error {}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null;

async function errorBody(response: Response): Promise<ErrorResponse | undefined> {
  try {
    const body: unknown = await response.json();
    if (isRecord(body) && typeof body.message === 'string' && typeof body.error === 'string') {
      return body as unknown as ErrorResponse;
    }
  } catch {
    // A non-JSON body from a proxy or a gateway. The status carries the meaning.
  }
  return undefined;
}

export class IdpClient {
  private csrfToken = '';

  constructor(
    private readonly base: string,
    private readonly fetchImpl: typeof fetch = (...args) => fetch(...args),
  ) {}

  private url(path: string): string {
    return `${this.base.replace(/\/$/, '')}${path}`;
  }

  private headers(json: boolean): HeadersInit {
    const headers: Record<string, string> = { accept: 'application/json' };
    if (json) headers['content-type'] = 'application/json';
    // The provider requires this on every write. Its own page reads the token
    // out of the HTML it serves; ours takes it from `session()` below, which
    // is the path the provider's frontend itself uses when it is not served by
    // the backend (its `IS_DEV` branch).
    if (this.csrfToken) headers['x-csrf-token'] = this.csrfToken;
    return headers;
  }

  /**
   * Establish a session and take its CSRF token.
   *
   * The provider normally does this while serving its login HTML. We are not
   * serving its HTML, so we ask for it directly — same endpoint, same result.
   */
  async session(): Promise<SessionInfoResponse> {
    const response = await this.fetchImpl(this.url('/oidc/session'), {
      method: 'POST',
      credentials: 'same-origin',
      headers: this.headers(false),
    });
    if (!response.ok) {
      const body = await errorBody(response);
      throw new IdpError(body?.message ?? `the identity provider answered ${response.status}`);
    }
    const session = (await response.json()) as SessionInfoResponse;
    if (session.csrf_token) this.csrfToken = session.csrf_token;
    return session;
  }

  /** A proof-of-work challenge, as plain text. */
  async challenge(): Promise<string> {
    const response = await this.fetchImpl(this.url('/pow'), {
      method: 'POST',
      credentials: 'same-origin',
      headers: this.headers(false),
    });
    if (!response.ok) {
      throw new IdpError(`the identity provider refused a proof of work (${response.status})`);
    }
    return (await response.text()).trim();
  }

  /** One login attempt. */
  async authorize(payload: LoginRequest): Promise<AuthorizeOutcome> {
    const response = await this.fetchImpl(this.url('/oidc/authorize'), {
      method: 'POST',
      credentials: 'same-origin',
      headers: this.headers(true),
      body: JSON.stringify(payload),
      // 202 is not a redirect, so nothing is followed here — but say so, since
      // the whole point of the call is to read a `Location` header ourselves.
      redirect: 'manual',
    });

    switch (response.status) {
      case 202: {
        const location = response.headers.get('location');
        if (!location) {
          throw new IdpError('the identity provider accepted the login but sent nowhere to go');
        }
        return { kind: 'redirect', location };
      }
      case 200:
        return { kind: 'passkey', challenge: (await response.json()) as WebauthnLoginResponse };
      case 205:
        return { kind: 'update-required' };
      case 206: {
        const body = (await response.json()) as { tos_await_code: string };
        return { kind: 'terms-required', code: body.tos_await_code };
      }
      case 406:
        return { kind: 'mfa-required' };
      case 429: {
        const header = response.headers.get('x-retry-not-before');
        const notBefore = header ? Number.parseInt(header, 10) : Number.NaN;
        return { kind: 'rate-limited', notBefore: Number.isFinite(notBefore) ? notBefore : undefined };
      }
      case 400:
        return { kind: 'rejected', message: (await errorBody(response))?.message ?? 'Refused.' };
      case 403: {
        const body = await errorBody(response);
        if (body?.error === 'PasswordRefresh') return { kind: 'password-expired' };
        return { kind: 'rejected', message: body?.message ?? 'Refused.' };
      }
      default:
        return { kind: 'unauthorized' };
    }
  }

  /**
   * Ask for a passkey challenge.
   *
   * The purpose carries the code the 200 answer to `authorize` gave us, which
   * is what ties this ceremony to that half-finished login. The session cookie
   * says who is signing in; nothing here repeats the email.
   */
  async webauthnStart(purpose: WebauthnPurpose): Promise<WebauthnStartResponse> {
    const response = await this.fetchImpl(this.url('/users/webauthn_start'), {
      method: 'POST',
      credentials: 'same-origin',
      headers: this.headers(true),
      body: JSON.stringify({ purpose }),
    });
    if (!response.ok) {
      const body = await errorBody(response);
      throw new IdpError(body?.message ?? `the identity provider refused the challenge`);
    }
    return (await response.json()) as WebauthnStartResponse;
  }

  /**
   * Hand back the signed assertion, and find out what it bought.
   *
   * The answers are the login's answers, so they are the login's type: this
   * can finish the sign-in, or land on the same new-terms and account-update
   * steps a password can. Unlike `authorize`, where the destination is a
   * `Location` header, here it is `loc` in the body.
   */
  async webauthnFinish(code: string, data: Record<string, unknown>): Promise<AuthorizeOutcome> {
    const response = await this.fetchImpl(this.url('/users/webauthn_finish'), {
      method: 'POST',
      credentials: 'same-origin',
      headers: this.headers(true),
      body: JSON.stringify({ code, data }),
    });

    switch (response.status) {
      case 202:
      case 206: {
        const body = (await response.json()) as { loc?: string; tos_await_code?: string };
        if (body.loc) return { kind: 'redirect', location: body.loc };
        if (body.tos_await_code) return { kind: 'terms-required', code: body.tos_await_code };
        throw new IdpError('the identity provider accepted the key but sent nowhere to go');
      }
      case 205:
        return { kind: 'update-required' };
      case 429: {
        const header = response.headers.get('x-retry-not-before');
        const notBefore = header ? Number.parseInt(header, 10) : Number.NaN;
        return { kind: 'rate-limited', notBefore: Number.isFinite(notBefore) ? notBefore : undefined };
      }
      default:
        return { kind: 'rejected', message: (await errorBody(response))?.message ?? 'That key was refused.' };
    }
  }

  /**
   * The terms in force, or nothing if a deployment has none.
   *
   * 204 is the provider saying there are none — which, when it has just
   * refused a login for want of accepting them, is a deployment that removed
   * its terms mid-flight rather than an error.
   */
  async terms(): Promise<Terms | undefined> {
    const response = await this.fetchImpl(this.url('/tos/latest'), {
      method: 'GET',
      credentials: 'same-origin',
      headers: this.headers(false),
    });
    if (response.status === 204) return undefined;
    if (!response.ok) {
      const body = await errorBody(response);
      throw new IdpError(body?.message ?? `the identity provider answered ${response.status}`);
    }
    return (await response.json()) as Terms;
  }

  /**
   * Accept them, or decline them, and carry on with the login.
   *
   * Both answers resume the same authorization request — declining is only
   * offered while the terms are optional, and then it is as valid an answer as
   * accepting — so both come back as the login's own outcome.
   */
  async answerTerms(accept: boolean, code: string, ts: number): Promise<AuthorizeOutcome> {
    const response = await this.fetchImpl(this.url(accept ? '/tos/accept' : '/tos/deny'), {
      method: 'POST',
      credentials: 'same-origin',
      headers: this.headers(true),
      body: JSON.stringify({ accept_code: code, tos_ts: ts }),
      redirect: 'manual',
    });

    switch (response.status) {
      case 200:
      case 202: {
        const location = response.headers.get('location');
        if (location) return { kind: 'redirect', location };
        const body = (await response.json().catch(() => ({}))) as { loc?: string };
        if (body.loc) return { kind: 'redirect', location: body.loc };
        throw new IdpError('the identity provider took the answer but sent nowhere to go');
      }
      case 205:
        return { kind: 'update-required' };
      case 406:
        return { kind: 'mfa-required' };
      default:
        return { kind: 'rejected', message: (await errorBody(response))?.message ?? 'Refused.' };
    }
  }

  /**
   * "I would like an account."
   *
   * Whether anyone may is the provider's decision, not this page's: a
   * deployment that keeps registration closed answers 403, and saying so is
   * more use than hiding the form and leaving people to guess.
   */
  async register(payload: RegisterRequest): Promise<RegisterOutcome> {
    const response = await this.fetchImpl(this.url('/users/register'), {
      method: 'POST',
      credentials: 'same-origin',
      headers: this.headers(true),
      body: JSON.stringify(payload),
    });
    if (response.ok) return { kind: 'sent' };
    if (response.status === 403) return { kind: 'closed' };
    if (response.status === 429) return { kind: 'rate-limited' };
    return { kind: 'rejected', message: (await errorBody(response))?.message ?? 'Refused.' };
  }

  /** "Email me a reset link." */
  async requestReset(payload: RequestResetRequest): Promise<ResetOutcome> {
    const response = await this.fetchImpl(this.url('/users/request_reset'), {
      method: 'POST',
      credentials: 'same-origin',
      headers: this.headers(true),
      body: JSON.stringify(payload),
    });
    if (response.ok) return { kind: 'sent' };
    if (response.status === 429) return { kind: 'rate-limited' };
    return { kind: 'rejected', message: (await errorBody(response))?.message ?? 'Refused.' };
  }
}
