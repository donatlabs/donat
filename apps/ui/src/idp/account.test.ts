import { describe, expect, it } from 'vitest';

import { AccountClient, AccountError, policyFailures, type PasswordPolicy } from './account';

interface Call {
  url: string;
  init: RequestInit | undefined;
}

function client(responder: (call: Call) => Response) {
  const calls: Call[] = [];
  const instance = new AccountClient('/auth/v1', (input, init) => {
    const call = { url: String(input), init };
    calls.push(call);
    return Promise.resolve(responder(call));
  });
  return { instance, calls };
}

const json = (body: unknown, status = 200) =>
  new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  });

describe('AccountClient.session', () => {
  it('takes the CSRF token and sends it on every later write', async () => {
    const { instance, calls } = client((call) =>
      call.url.endsWith('/xsrf')
        ? json({ csrf_token: 'token-1' })
        : json({ id: 's', user_id: 'u-1', exp: 1, timeout: 1 }),
    );

    await expect(instance.session()).resolves.toMatchObject({ user_id: 'u-1' });
    await instance.update('u-1', { given_name: 'A' });

    expect(new Headers(calls[2].init?.headers).get('x-csrf-token')).toBe('token-1');
    expect(calls[2].init?.credentials).toBe('same-origin');
  });

  it('is explicit that a refused call means the session ended', async () => {
    const { instance } = client(() => json({ message: 'no' }, 401));

    await expect(instance.session()).rejects.toThrow(AccountError);
    await expect(instance.session()).rejects.toThrow(/no longer signed in/);
  });
});

describe('AccountClient', () => {
  it('escapes the account id rather than pasting it into a path', async () => {
    const { instance, calls } = client(() => json([]));

    await instance.passkeys('a/../b');

    expect(calls[0].url).toBe('/auth/v1/users/a%2F..%2Fb/webauthn');
  });

  it('escapes a passkey name on the way to being deleted', async () => {
    const { instance, calls } = client(() => new Response(null, { status: 200 }));

    await instance.passkeyDelete('u-1', 'my key/1');

    expect(calls[0].url).toBe('/auth/v1/users/u-1/webauthn/delete/my%20key%2F1');
    expect(calls[0].init?.method).toBe('DELETE');
  });

  it('takes 201 for an enrolled key, which is the only success it has', async () => {
    const { instance } = client(() => new Response(null, { status: 201 }));

    await expect(instance.passkeyFinish('u-1', 'key', { id: 'x' })).resolves.toBeUndefined();
  });

  it('keeps the provider\'s own words when it refuses a change', async () => {
    const { instance } = client(() => json({ message: 'that password was used recently' }, 400));

    await expect(instance.update('u-1', { password: 'x' })).rejects.toThrow(/used recently/);
  });
});

describe('policyFailures', () => {
  const policy = (over: Partial<PasswordPolicy> = {}): PasswordPolicy => ({
    length_min: 8,
    length_max: 128,
    include_lower_case: 1,
    include_upper_case: 1,
    include_digits: 1,
    ...over,
  });

  it('is empty for a password that satisfies the policy', () => {
    expect(policyFailures('Password1', policy())).toEqual([]);
  });

  it('reports every reason at once, not one per attempt', () => {
    expect(policyFailures('abc', policy())).toEqual([
      'at least 8 characters',
      'an upper-case letter',
      'a digit',
    ]);
  });

  it('counts, when a policy asks for more than one of something', () => {
    expect(policyFailures('Password1', policy({ include_digits: 3 }))).toEqual(['3 digits']);
  });

  it('says so when a password is too long, which policies do set', () => {
    expect(policyFailures('Password1'.repeat(3), policy({ length_max: 10 }))).toEqual([
      'at most 10 characters',
    ]);
  });

  it('ignores a rule the deployment did not set', () => {
    expect(policyFailures('password1', policy({ include_upper_case: 0 }))).toEqual([]);
  });
});
