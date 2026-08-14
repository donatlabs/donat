/**
 * The account, as its owner.
 *
 * This is the one part of the panel that talks to the identity provider as a
 * *person* rather than as the deployment. The identity screens under
 * `Identity` go through the engine, which holds an API key and an ordinary
 * role — that is the deployment acting on somebody's account. These calls
 * carry the provider's own session cookie from the browser that signed in, so
 * the provider decides what this person may do to their own record, and the
 * engine is not in a position to impersonate anyone.
 *
 * Which is why it is a separate client from `IdpClient` despite sharing an
 * origin: that one runs before anybody is signed in and is written around a
 * login that has not happened yet. This one only exists afterwards.
 */

export class AccountError extends Error {
  constructor(
    message: string,
    readonly status = 0,
  ) {
    super(message);
    this.name = 'AccountError';
  }
}

/** `GET /oidc/sessioninfo` — who the provider thinks is here. */
export interface SessionInfo {
  id: string;
  user_id: string;
  roles?: string;
  groups?: string;
  exp: number;
  timeout: number;
  state?: string;
}

/** `GET /users/{id}` — the record itself. Only the parts a screen shows. */
export interface Account {
  id: string;
  email: string;
  given_name?: string;
  family_name?: string;
  language?: string;
  account_type?: string;
  roles?: string[];
  groups?: string[];
  enabled?: boolean;
  email_verified?: boolean;
  password_expires?: number;
  last_login?: number;
  created_at?: number;
  user_expires?: number;
  picture_id?: string;
}

/** `GET /users/{id}/webauthn` — one enrolled key. */
export interface Passkey {
  name: string;
  registered: number;
  last_used: number;
  user_verified?: boolean;
}

/** `GET /password_policy` — what a new password has to satisfy. */
export interface PasswordPolicy {
  length_min: number;
  length_max: number;
  include_lower_case?: number;
  include_upper_case?: number;
  include_digits?: number;
  include_special?: number;
  valid_days?: number;
  not_recently_used?: number;
}

/** `GET /users/{id}/devices` — a machine holding a refresh token. */
export interface Device {
  id: string;
  name: string;
  created: number;
  access_exp: number;
  refresh_exp?: number;
  peer_ip: string;
}

/**
 * What a self-update carries.
 *
 * The provider replaces the record, so a screen sends the fields it manages
 * and nothing else. `password` is only present when it is being changed.
 */
export interface SelfUpdate {
  email?: string;
  given_name?: string;
  family_name?: string;
  language?: string;
  password?: string;
  [key: string]: unknown;
}

export class AccountClient {
  private csrfToken = '';

  constructor(
    private readonly base = '/auth/v1',
    private readonly fetchImpl: typeof fetch = (...args) => fetch(...args),
  ) {}

  private url(path: string): string {
    return `${this.base.replace(/\/$/, '')}${path}`;
  }

  private headers(json: boolean): Record<string, string> {
    const headers: Record<string, string> = { accept: 'application/json' };
    if (json) headers['content-type'] = 'application/json';
    if (this.csrfToken) headers['x-csrf-token'] = this.csrfToken;
    return headers;
  }

  private async call(path: string, init: RequestInit = {}): Promise<Response> {
    const response = await this.fetchImpl(this.url(path), {
      credentials: 'same-origin',
      ...init,
      headers: { ...this.headers(init.body !== undefined), ...init.headers },
    });
    if (response.status === 401 || response.status === 403) {
      throw new AccountError('That session is no longer signed in.', response.status);
    }
    return response;
  }

  private async json<T>(path: string, init: RequestInit = {}): Promise<T> {
    const response = await this.call(path, init);
    if (!response.ok) throw await this.failure(response);
    return (await response.json()) as T;
  }

  private async failure(response: Response): Promise<AccountError> {
    try {
      const body: unknown = await response.json();
      if (body && typeof body === 'object' && 'message' in body) {
        return new AccountError(String((body as { message: unknown }).message), response.status);
      }
    } catch {
      // A gateway, or an empty body. The status is what there is.
    }
    return new AccountError(`The identity provider answered ${response.status}.`, response.status);
  }

  /**
   * Who is here, and the token every write needs.
   *
   * Two calls because the provider serves them separately, and the CSRF token
   * is the reason: its own page reads it out of the HTML it was served, and we
   * are not served by it.
   */
  async session(): Promise<SessionInfo> {
    const info = await this.json<SessionInfo>('/oidc/sessioninfo');
    const xsrf = await this.call('/oidc/sessioninfo/xsrf', { method: 'GET' });
    if (xsrf.ok) {
      const body = (await xsrf.json()) as { csrf_token?: string };
      if (body.csrf_token) this.csrfToken = body.csrf_token;
    }
    return info;
  }

  account(id: string): Promise<Account> {
    return this.json<Account>(`/users/${encodeURIComponent(id)}`);
  }

  policy(): Promise<PasswordPolicy> {
    return this.json<PasswordPolicy>('/password_policy');
  }

  /** Change what this person may change about themselves. */
  async update(id: string, changes: SelfUpdate): Promise<Account> {
    return this.json<Account>(`/users/${encodeURIComponent(id)}/self`, {
      method: 'PUT',
      body: JSON.stringify(changes),
    });
  }

  passkeys(id: string): Promise<Passkey[]> {
    return this.json<Passkey[]>(`/users/${encodeURIComponent(id)}/webauthn`);
  }

  /** Start enrolling a key. The challenge comes back for the browser. */
  async passkeyStart(id: string, name: string): Promise<unknown> {
    return this.json<unknown>(`/users/${encodeURIComponent(id)}/webauthn/register/start`, {
      method: 'POST',
      body: JSON.stringify({ passkey_name: name }),
    });
  }

  /** Finish enrolling it. The provider answers 201 and nothing else. */
  async passkeyFinish(id: string, name: string, data: Record<string, unknown>): Promise<void> {
    const response = await this.call(`/users/${encodeURIComponent(id)}/webauthn/register/finish`, {
      method: 'POST',
      body: JSON.stringify({ passkey_name: name, data }),
    });
    if (response.status !== 201 && !response.ok) throw await this.failure(response);
  }

  async passkeyDelete(id: string, name: string): Promise<void> {
    const response = await this.call(
      `/users/${encodeURIComponent(id)}/webauthn/delete/${encodeURIComponent(name)}`,
      { method: 'DELETE' },
    );
    if (!response.ok) throw await this.failure(response);
  }

  devices(id: string): Promise<Device[]> {
    return this.json<Device[]>(`/users/${encodeURIComponent(id)}/devices`);
  }

  async deviceForget(id: string, deviceId: string): Promise<void> {
    const response = await this.call(`/users/${encodeURIComponent(id)}/devices`, {
      method: 'DELETE',
      body: JSON.stringify({ device_id: deviceId }),
    });
    if (!response.ok) throw await this.failure(response);
  }
}

/**
 * Whether a password satisfies the policy, as the reasons a person can act on.
 *
 * Returned rather than thrown, and as a list, because a form that reports one
 * problem at a time makes somebody guess how many are left.
 */
export function policyFailures(password: string, policy: PasswordPolicy): string[] {
  const counted = (pattern: RegExp) => (password.match(pattern) ?? []).length;
  const failures: string[] = [];
  if (password.length < policy.length_min) {
    failures.push(`at least ${policy.length_min} characters`);
  }
  if (policy.length_max && password.length > policy.length_max) {
    failures.push(`at most ${policy.length_max} characters`);
  }
  const rules: [number | undefined, RegExp, string][] = [
    [policy.include_lower_case, /[a-z]/g, 'lower-case letter'],
    [policy.include_upper_case, /[A-Z]/g, 'upper-case letter'],
    [policy.include_digits, /\d/g, 'digit'],
    [policy.include_special, /[^a-zA-Z0-9]/g, 'special character'],
  ];
  for (const [required, pattern, noun] of rules) {
    if (required && counted(pattern) < required) {
      const article = /^[aeiou]/i.test(noun) ? 'an' : 'a';
      failures.push(required === 1 ? `${article} ${noun}` : `${required} ${noun}s`);
    }
  }
  return failures;
}
