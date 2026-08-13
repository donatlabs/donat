import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { IdpAuthorizeForm } from './idp-authorize';
import { IdpClient } from '../idp/client';
import { PowSolver } from '../idp/pow-solver';
import { parseAuthorizeParams } from '../idp/authorize-params';

/**
 * The sign-in screen, driven by the provider's answers.
 *
 * These go through the real client and the real solver against a stubbed
 * provider, because the interesting behaviour is the conversation, not the
 * markup: which field is on screen depends entirely on what the provider said
 * last, and getting that wrong strands an operator on a form that cannot
 * succeed.
 */

const params = parseAuthorizeParams(
  '?client_id=panel&redirect_uri=http%3A%2F%2Flocalhost%3A8080%2Fauth%2Fcallback' +
    '&scope=openid&state=abc&code_challenge=chal&code_challenge_method=S256',
)!;

const sessionResponse = () =>
  new Response(JSON.stringify({ id: 's', csrf_token: 'csrf', exp: '1', timeout: '1', state: 'Init' }), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });

/** A provider that answers `/oidc/authorize` from a script, one call at a time. */
function stubProvider(script: Response[], register?: Response) {
  const bodies: string[] = [];
  const client = new IdpClient('/auth/v1', (input, init) => {
    const url = String(input);
    if (url.endsWith('/oidc/session')) return Promise.resolve(sessionResponse());
    if (url.endsWith('/pow')) return Promise.resolve(new Response('1:10:1:salt:hash:'));
    if (url.endsWith('/users/register')) {
      bodies.push(String(init?.body));
      return Promise.resolve(register ?? new Response(null, { status: 200 }));
    }
    if (url.endsWith('/oidc/authorize')) {
      bodies.push(String(init?.body));
      return Promise.resolve(script.shift() ?? new Response(null, { status: 401 }));
    }
    return Promise.resolve(new Response(null, { status: 200 }));
  });
  // The real solver, but solving inline: a worker is not available in jsdom,
  // and the arithmetic is covered by `pow.test.ts`.
  const solver = new PowSolver(
    () => client.challenge(),
    (challenge) => Promise.resolve(`${challenge}0`),
  );
  return { client, solver, bodies };
}

function renderForm(
  script: Response[],
  options: { registration?: boolean; register?: Response } = {},
) {
  const provider = stubProvider(script, options.register);
  render(
    <IdpAuthorizeForm
      params={params}
      client={provider.client}
      solver={provider.solver}
      providerUrl="/auth/v1/oidc/authorize?client_id=panel"
      registration={options.registration}
    />,
  );
  return provider;
}

const submit = () => fireEvent.click(screen.getByTestId('idp-submit'));
const type = (testId: string, value: string) =>
  fireEvent.change(screen.getByTestId(testId), { target: { value } });

let replace: ReturnType<typeof vi.fn>;

beforeEach(() => {
  replace = vi.fn();
  Object.defineProperty(window, 'location', {
    value: { ...window.location, replace },
    writable: true,
    configurable: true,
  });
});

describe('IdpAuthorizeForm', () => {
  it('asks for the email first, and for a password only once the provider wants one', async () => {
    renderForm([new Response(null, { status: 401 })]);
    await waitFor(() => expect(screen.getByTestId('idp-submit')).not.toBeDisabled());

    expect(screen.queryByTestId('idp-password')).toBeNull();

    type('idp-email', 'operator@example.test');
    submit();

    await waitFor(() => expect(screen.getByTestId('idp-password')).toBeTruthy());
    // A first refusal is not a wrong password — the provider answers the same
    // way to an account it has never seen — so nothing is claimed about it.
    expect(screen.queryByTestId('idp-error')).toBeNull();
  });

  it('sends the login and follows the provider back to the engine', async () => {
    const provider = renderForm([
      new Response(null, { status: 401 }),
      new Response(null, {
        status: 202,
        headers: { location: 'http://localhost:8080/auth/callback?code=abc&state=abc' },
      }),
    ]);
    await waitFor(() => expect(screen.getByTestId('idp-submit')).not.toBeDisabled());

    type('idp-email', 'operator@example.test');
    submit();
    await waitFor(() => expect(screen.getByTestId('idp-password')).toBeTruthy());
    type('idp-password', 'correct horse');
    submit();

    await waitFor(() =>
      expect(replace).toHaveBeenCalledWith('http://localhost:8080/auth/callback?code=abc&state=abc'),
    );

    // The second attempt carries the password, the proof of work and the PKCE
    // challenge the engine minted — all three, or the login cannot complete.
    const sent = JSON.parse(provider.bodies[1]);
    expect(sent).toMatchObject({
      email: 'operator@example.test',
      password: 'correct horse',
      client_id: 'panel',
      code_challenge: 'chal',
      code_challenge_method: 'S256',
      state: 'abc',
    });
    expect(sent.pow).toMatch(/^1:10:/);
  });

  it('says so plainly when the credentials were wrong, and offers a reset', async () => {
    renderForm([new Response(null, { status: 401 }), new Response(null, { status: 401 })]);
    await waitFor(() => expect(screen.getByTestId('idp-submit')).not.toBeDisabled());

    type('idp-email', 'operator@example.test');
    submit();
    await waitFor(() => expect(screen.getByTestId('idp-password')).toBeTruthy());
    type('idp-password', 'wrong');
    submit();

    await waitFor(() => expect(screen.getByTestId('idp-error').textContent).toMatch(/did not match/));
    expect(screen.getByTestId('idp-reset')).toBeTruthy();
  });

  it('hands over to the provider for a screen it does not implement', async () => {
    renderForm([
      new Response(JSON.stringify({ code: 'mfa', user_id: 'u', exp: 1 }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    ]);
    await waitFor(() => expect(screen.getByTestId('idp-submit')).not.toBeDisabled());

    type('idp-email', 'operator@example.test');
    submit();

    await waitFor(() => expect(screen.getByTestId('idp-handoff')).toBeTruthy());
    expect(screen.getByTestId('idp-handoff-continue').getAttribute('href')).toBe(
      '/auth/v1/oidc/authorize?client_id=panel',
    );
    // Nothing was signed in, so nothing was navigated.
    expect(replace).not.toHaveBeenCalled();
  });

  it('stops trying when the provider has had enough', async () => {
    renderForm([new Response(null, { status: 429, headers: { 'x-retry-not-before': '4102444800' } })]);
    await waitFor(() => expect(screen.getByTestId('idp-submit')).not.toBeDisabled());

    type('idp-email', 'operator@example.test');
    submit();

    await waitFor(() => expect(screen.getByTestId('idp-error').textContent).toMatch(/Too many attempts/));
    expect(screen.getByTestId('idp-submit')).toBeDisabled();
  });

  it('cannot be used at all if the provider will not start a session', async () => {
    const client = new IdpClient('/auth/v1', () =>
      Promise.resolve(
        new Response(JSON.stringify({ error: 'Internal', message: 'no session for you' }), {
          status: 500,
          headers: { 'content-type': 'application/json' },
        }),
      ),
    );
    render(
      <IdpAuthorizeForm
        params={params}
        client={client}
        solver={new PowSolver(() => client.challenge(), (c) => Promise.resolve(c))}
        providerUrl="/auth/v1/oidc/authorize"
      />,
    );

    await waitFor(() => expect(screen.getByTestId('idp-error').textContent).toBe('no session for you'));
  });
});

describe('creating an account', () => {
  it('is not offered unless the deployment says the provider allows it', async () => {
    renderForm([]);
    await waitFor(() => expect(screen.getByTestId('idp-submit')).not.toBeDisabled());
    expect(screen.queryByTestId('idp-signup-open')).toBeNull();
  });

  it('asks the provider, and says what it answered', async () => {
    const provider = renderForm([], { registration: true });
    await waitFor(() => expect(screen.getByTestId('idp-submit')).not.toBeDisabled());

    fireEvent.click(screen.getByTestId('idp-signup-open'));
    type('idp-signup-email', 'newcomer@example.test');
    type('idp-signup-given', 'New');
    fireEvent.click(screen.getByTestId('idp-signup-submit'));

    await waitFor(() => expect(screen.getByTestId('idp-notice').textContent).toMatch(/email/i));
    // The proof of work goes with it, as with every other ask of the provider.
    const sent = JSON.parse(provider.bodies[0]);
    expect(sent).toMatchObject({ email: 'newcomer@example.test', given_name: 'New' });
    expect(sent.pow).toMatch(/^1:10:/);
  });

  it('says so plainly when the deployment does not allow it', async () => {
    renderForm([], { registration: true, register: new Response(null, { status: 403 }) });
    await waitFor(() => expect(screen.getByTestId('idp-submit')).not.toBeDisabled());

    fireEvent.click(screen.getByTestId('idp-signup-open'));
    type('idp-signup-email', 'newcomer@example.test');
    type('idp-signup-given', 'New');
    fireEvent.click(screen.getByTestId('idp-signup-submit'));

    await waitFor(() =>
      expect(screen.getByTestId('idp-error').textContent).toMatch(/does not let people/i),
    );
  });
});
