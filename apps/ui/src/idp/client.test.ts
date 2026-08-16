import { describe, expect, it } from 'vitest';
import { IdpClient, IdpError } from './client';
import type { LoginRequest } from './types';

/**
 * What the provider's status codes mean.
 *
 * Every case here is one the provider's own login page handles, and the
 * mapping is checked rather than assumed: this is the file that has to change
 * if the provider ever changes its mind, and a wrong reading of `200` versus
 * `202` is the difference between signing someone in and stranding them.
 */

interface Call {
  url: string;
  init: RequestInit | undefined;
}

function client(responder: (call: Call) => Response) {
  const calls: Call[] = [];
  const instance = new IdpClient('/auth/v1', (input, init) => {
    const call = { url: String(input), init };
    calls.push(call);
    return Promise.resolve(responder(call));
  });
  return { instance, calls };
}

const login: LoginRequest = {
  email: 'operator@example.test',
  pow: '1:20:…:0',
  client_id: 'panel',
  redirect_uri: 'http://localhost:8080/auth/callback',
};

const session = (csrf: string | undefined) =>
  new Response(JSON.stringify({ id: 's', csrf_token: csrf, exp: '1', timeout: '1', state: 'Init' }), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });

describe('IdpClient.session', () => {
  it('establishes a session at the provider and keeps its CSRF token', async () => {
    const { instance, calls } = client((call) =>
      call.url.endsWith('/oidc/session') ? session('token-1') : new Response(null, { status: 202, headers: { location: '/done' } }),
    );

    await instance.session();
    await instance.authorize(login);

    expect(calls[0].url).toBe('/auth/v1/oidc/session');
    expect(calls[0].init?.method).toBe('POST');
    // The provider requires this header on every write. Its own page reads the
    // token out of the HTML it serves; ours takes it from the session call.
    expect(new Headers(calls[1].init?.headers).get('x-csrf-token')).toBe('token-1');
  });

  it('reports the provider\'s own words when it refuses', async () => {
    const { instance } = client(
      () =>
        new Response(JSON.stringify({ error: 'Internal', message: 'database is down' }), {
          status: 500,
          headers: { 'content-type': 'application/json' },
        }),
    );

    await expect(instance.session()).rejects.toThrow(IdpError);
    await expect(instance.session()).rejects.toThrow('database is down');
  });
});

describe('IdpClient.authorize', () => {
  const outcomeFor = async (response: Response) => {
    const { instance } = client(() => response);
    return instance.authorize(login);
  };

  it('202 is the login: the code travels back in the Location header', async () => {
    const outcome = await outcomeFor(
      new Response(null, {
        status: 202,
        headers: { location: 'http://localhost:8080/auth/callback?code=abc&state=xyz' },
      }),
    );

    expect(outcome).toEqual({
      kind: 'redirect',
      location: 'http://localhost:8080/auth/callback?code=abc&state=xyz',
    });
  });

  it('refuses a 202 with nowhere to go rather than pretending it worked', async () => {
    const { instance } = client(() => new Response(null, { status: 202 }));
    await expect(instance.authorize(login)).rejects.toThrow(IdpError);
  });

  it('200 means the password was right and a passkey is still needed', async () => {
    const outcome = await outcomeFor(
      new Response(JSON.stringify({ code: 'mfa-code', user_id: 'u', exp: 1 }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    );

    expect(outcome).toEqual({ kind: 'passkey', challenge: { code: 'mfa-code', user_id: 'u', exp: 1 } });
  });

  it('205 and 206 are the provider\'s own follow-up screens', async () => {
    expect(await outcomeFor(new Response(null, { status: 205 }))).toEqual({ kind: 'update-required' });
    expect(
      await outcomeFor(
        new Response(JSON.stringify({ tos_await_code: 'tos-1' }), {
          status: 206,
          headers: { 'content-type': 'application/json' },
        }),
      ),
    ).toEqual({ kind: 'terms-required', code: 'tos-1' });
  });

  it('406 is the client demanding a second factor the account lacks', async () => {
    expect(await outcomeFor(new Response(null, { status: 406 }))).toEqual({ kind: 'mfa-required' });
  });

  it('403 separates an expired password from any other refusal', async () => {
    expect(
      await outcomeFor(
        new Response(JSON.stringify({ error: 'PasswordRefresh', message: 'expired' }), {
          status: 403,
          headers: { 'content-type': 'application/json' },
        }),
      ),
    ).toEqual({ kind: 'password-expired' });

    expect(
      await outcomeFor(
        new Response(JSON.stringify({ error: 'Forbidden', message: 'contact your Administrator' }), {
          status: 403,
          headers: { 'content-type': 'application/json' },
        }),
      ),
    ).toEqual({ kind: 'rejected', message: 'contact your Administrator' });
  });

  it('429 carries when the provider will listen again', async () => {
    expect(
      await outcomeFor(new Response(null, { status: 429, headers: { 'x-retry-not-before': '1802682422' } })),
    ).toEqual({ kind: 'rate-limited', notBefore: 1802682422 });

    // The header is documented but the page must survive its absence.
    expect(await outcomeFor(new Response(null, { status: 429 }))).toEqual({
      kind: 'rate-limited',
      notBefore: undefined,
    });
  });

  it('400 is passed through in the provider\'s own words', async () => {
    expect(
      await outcomeFor(
        new Response(JSON.stringify({ error: 'BadRequest', message: 'invalid redirect_uri' }), {
          status: 400,
          headers: { 'content-type': 'application/json' },
        }),
      ),
    ).toEqual({ kind: 'rejected', message: 'invalid redirect_uri' });
  });

  it('leaves 401 unnamed, because the provider will not say which it is', async () => {
    expect(await outcomeFor(new Response(null, { status: 401 }))).toEqual({ kind: 'unauthorized' });
  });
});

describe('IdpClient.requestReset', () => {
  it('asks the provider to send the email', async () => {
    const { instance, calls } = client(() => new Response(null, { status: 200 }));

    await expect(instance.requestReset({ email: 'a@b.test', pow: '1:20:…:0' })).resolves.toEqual({
      kind: 'sent',
    });
    expect(calls[0].url).toBe('/auth/v1/users/request_reset');
  });

  it('distinguishes too-many-attempts from a refusal', async () => {
    const limited = client(() => new Response(null, { status: 429 }));
    await expect(limited.instance.requestReset({ email: 'a@b.test', pow: 'p' })).resolves.toEqual({
      kind: 'rate-limited',
    });

    const refused = client(
      () =>
        new Response(JSON.stringify({ error: 'BadRequest', message: 'no' }), {
          status: 400,
          headers: { 'content-type': 'application/json' },
        }),
    );
    await expect(refused.instance.requestReset({ email: 'a@b.test', pow: 'p' })).resolves.toEqual({
      kind: 'rejected',
      message: 'no',
    });
  });
});

/**
 * The passkey half.
 *
 * `webauthn_finish` answers the same questions as `authorize` does — signed
 * in, new terms, account to update — but it answers them differently: the
 * destination is `loc` in the body rather than a `Location` header. Reading
 * one where the provider sends the other strands somebody mid-login with no
 * error to show, so both readings are pinned here.
 */
describe('IdpClient.webauthnStart', () => {
  it('asks with the code the 200 answer carried', async () => {
    const { instance, calls } = client(
      () =>
        new Response(JSON.stringify({ code: 'ceremony', exp: 60, rcr: { publicKey: {} } }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
    );

    await expect(instance.webauthnStart({ Login: 'from-authorize' })).resolves.toMatchObject({
      code: 'ceremony',
      exp: 60,
    });
    expect(calls[0].url).toBe('/auth/v1/users/webauthn_start');
    expect(JSON.parse(String(calls[0].init?.body))).toEqual({
      purpose: { Login: 'from-authorize' },
    });
  });

  it('says so in the provider\'s words when there is no challenge to give', async () => {
    const { instance } = client(
      () =>
        new Response(JSON.stringify({ error: 'BadRequest', message: 'no such session' }), {
          status: 400,
          headers: { 'content-type': 'application/json' },
        }),
    );

    await expect(instance.webauthnStart({ Login: 'x' })).rejects.toThrow(IdpError);
  });
});

describe('IdpClient.webauthnFinish', () => {
  const answer = (status: number, body: unknown) =>
    new Response(body === undefined ? null : JSON.stringify(body), {
      status,
      headers: { 'content-type': 'application/json' },
    });

  it('sends the signed assertion under the ceremony code', async () => {
    const { instance, calls } = client(() => answer(202, { loc: '/auth/callback?code=1' }));

    await instance.webauthnFinish('ceremony', { id: 'the-key' });

    expect(calls[0].url).toBe('/auth/v1/users/webauthn_finish');
    expect(JSON.parse(String(calls[0].init?.body))).toEqual({
      code: 'ceremony',
      data: { id: 'the-key' },
    });
  });

  it('follows `loc` from the body, where authorize uses a header', async () => {
    const { instance } = client(() => answer(202, { loc: '/auth/callback?code=1' }));

    await expect(instance.webauthnFinish('c', {})).resolves.toEqual({
      kind: 'redirect',
      location: '/auth/callback?code=1',
    });
  });

  it('carries a 206 into the terms step, key or no key', async () => {
    const { instance } = client(() => answer(206, { tos_await_code: 'tos-1' }));

    await expect(instance.webauthnFinish('c', {})).resolves.toEqual({
      kind: 'terms-required',
      code: 'tos-1',
    });
  });

  it('reads 205 as the account needing an update', async () => {
    const { instance } = client(() => answer(205, undefined));

    await expect(instance.webauthnFinish('c', {})).resolves.toEqual({ kind: 'update-required' });
  });

  it('refuses to guess when the provider accepts the key but says nowhere to go', async () => {
    const { instance } = client(() => answer(202, {}));

    await expect(instance.webauthnFinish('c', {})).rejects.toThrow(IdpError);
  });

  it('keeps the provider\'s wording for a key it did not accept', async () => {
    const { instance } = client(() =>
      answer(401, { error: 'Unauthorized', message: 'invalid credential' }),
    );

    await expect(instance.webauthnFinish('c', {})).resolves.toEqual({
      kind: 'rejected',
      message: 'invalid credential',
    });
  });
});
