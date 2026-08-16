import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';

import { AccountScreen } from './account';
import { AccountClient } from '../idp/account';

/**
 * The account screen against a stubbed provider.
 *
 * Every case here is one the provider's own page has, and the reason to pin
 * them is that this screen is reached in two very different states: signed in
 * with the panel around it, and mid-login with only a provider session. What
 * it may do has to come from the provider's answers either way.
 */

const account = {
  id: 'u-1',
  email: 'operator@example.test',
  given_name: 'Olive',
  family_name: 'Operator',
  roles: ['support'],
};

const policy = { length_min: 8, length_max: 128, include_upper_case: 1, include_digits: 1 };

function stub(over: Record<string, () => Response> = {}) {
  const calls: { url: string; init: RequestInit | undefined }[] = [];
  const json = (body: unknown, status = 200) =>
    new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });

  let keys = [{ name: 'This laptop', registered: 1_700_000_000, last_used: 1_700_000_100 }];
  const client = new AccountClient('/auth/v1', (input, init) => {
    const url = String(input);
    calls.push({ url, init });
    for (const [suffix, responder] of Object.entries(over)) {
      if (url.endsWith(suffix)) return Promise.resolve(responder());
    }
    if (url.endsWith('/oidc/sessioninfo')) return Promise.resolve(json({ id: 's', user_id: 'u-1', exp: 1, timeout: 1 }));
    if (url.endsWith('/xsrf')) return Promise.resolve(json({ csrf_token: 'csrf-1' }));
    if (url.endsWith('/password_policy')) return Promise.resolve(json(policy));
    if (url.endsWith('/webauthn')) return Promise.resolve(json(keys));
    if (url.endsWith('/register/start')) return Promise.resolve(json({ publicKey: { challenge: 'AQ', user: { id: 'AQ', name: 'a', displayName: 'A' } } }));
    if (url.endsWith('/register/finish')) {
      keys = [...keys, { name: 'Phone', registered: 1_700_000_200, last_used: 0 }];
      return Promise.resolve(new Response(null, { status: 201 }));
    }
    if (url.includes('/webauthn/delete/')) {
      keys = [];
      return Promise.resolve(new Response(null, { status: 200 }));
    }
    if (url.endsWith('/self')) return Promise.resolve(json({ ...account, given_name: 'Olivia' }));
    if (url.endsWith('/users/u-1')) return Promise.resolve(json(account));
    return Promise.resolve(new Response(null, { status: 200 }));
  });
  return { client, calls };
}

const show = (over?: Record<string, () => Response>) => {
  const { client, calls } = stub(over);
  render(<AccountScreen client={client} />);
  return { calls };
};

describe('AccountScreen', () => {
  it('shows the account the provider says is signed in', async () => {
    show();

    await waitFor(() => expect(screen.getByTestId('account')).toBeTruthy());
    expect(screen.getByText('operator@example.test')).toBeTruthy();
    expect(screen.getByText('support')).toBeTruthy();
  });

  it('sends the whole record on a profile change, because the provider replaces it', async () => {
    const { calls } = show();
    await waitFor(() => expect(screen.getByTestId('account')).toBeTruthy());

    fireEvent.change(screen.getByTestId('account-given-name'), { target: { value: 'Olivia' } });
    fireEvent.click(screen.getByTestId('account-save-profile'));

    await waitFor(() => expect(screen.getByTestId('account-notice')).toBeTruthy());
    const sent = JSON.parse(String(calls.find((c) => c.url.endsWith('/self'))?.init?.body));
    expect(sent).toEqual({
      email: 'operator@example.test',
      given_name: 'Olivia',
      family_name: 'Operator',
    });
  });

  it('states every unmet rule at once and refuses to send until they are met', async () => {
    show();
    await waitFor(() => expect(screen.getByTestId('account')).toBeTruthy());

    fireEvent.change(screen.getByTestId('account-password'), { target: { value: 'short' } });

    await waitFor(() =>
      expect(screen.getByTestId('account-policy').textContent).toBe(
        'Needs at least 8 characters, an upper-case letter, a digit.',
      ),
    );
    expect(screen.getByTestId('account-save-password')).toBeDisabled();
  });

  it('will not send two passwords that differ', async () => {
    show();
    await waitFor(() => expect(screen.getByTestId('account')).toBeTruthy());

    fireEvent.change(screen.getByTestId('account-password'), { target: { value: 'Password1' } });
    fireEvent.change(screen.getByTestId('account-password-repeat'), { target: { value: 'Password2' } });

    await waitFor(() => expect(screen.getByTestId('account-mismatch')).toBeTruthy());
    expect(screen.getByTestId('account-save-password')).toBeDisabled();
  });

  it('changes a password that satisfies the policy', async () => {
    const { calls } = show();
    await waitFor(() => expect(screen.getByTestId('account')).toBeTruthy());

    fireEvent.change(screen.getByTestId('account-password'), { target: { value: 'Password1' } });
    fireEvent.change(screen.getByTestId('account-password-repeat'), { target: { value: 'Password1' } });
    fireEvent.click(screen.getByTestId('account-save-password'));

    await waitFor(() => expect(screen.getByTestId('account-notice')).toBeTruthy());
    const sent = JSON.parse(String(calls.filter((c) => c.url.endsWith('/self')).pop()?.init?.body));
    expect(sent.password).toBe('Password1');
  });

  it('lists the keys already enrolled, and enrols another', async () => {
    Object.defineProperty(navigator, 'credentials', {
      configurable: true,
      value: {
        get: () => Promise.resolve(null),
        create: () =>
          Promise.resolve({
            id: 'new-key',
            rawId: new Uint8Array([1]).buffer,
            type: 'public-key',
            response: {
              attestationObject: new Uint8Array([2]).buffer,
              clientDataJSON: new Uint8Array([3]).buffer,
            },
            getClientExtensionResults: () => ({}),
          }),
      },
    });

    const { calls } = show();
    await waitFor(() => expect(screen.getByTestId('account-passkey-This laptop')).toBeTruthy());

    fireEvent.change(screen.getByTestId('account-key-name'), { target: { value: 'Phone' } });
    fireEvent.click(screen.getByTestId('account-enrol'));

    await waitFor(() => expect(screen.getByTestId('account-passkey-Phone')).toBeTruthy());
    // The name is the provider's key for it, so it travels on both calls.
    const start = calls.find((c) => c.url.endsWith('/register/start'));
    expect(JSON.parse(String(start?.init?.body))).toEqual({ passkey_name: 'Phone' });
  });

  it('removes a key by name', async () => {
    const { calls } = show();
    await waitFor(() => expect(screen.getByTestId('account-passkey-This laptop')).toBeTruthy());

    fireEvent.click(screen.getByTestId('account-forget-This laptop'));

    await waitFor(() => expect(screen.getByTestId('account-no-passkeys')).toBeTruthy());
    expect(calls.some((c) => c.url.endsWith('/webauthn/delete/This%20laptop'))).toBe(true);
  });

  it('says the session is gone rather than showing an empty account', async () => {
    show({ '/oidc/sessioninfo': () => new Response(null, { status: 401 }) });

    await waitFor(() =>
      expect(screen.getByTestId('account-error').textContent).toMatch(/no longer signed in/),
    );
  });

  it('keeps the provider\'s own words when it refuses a password', async () => {
    show({ '/self': () => new Response(JSON.stringify({ message: 'used too recently' }), { status: 400, headers: { 'content-type': 'application/json' } }) });
    await waitFor(() => expect(screen.getByTestId('account')).toBeTruthy());

    fireEvent.change(screen.getByTestId('account-password'), { target: { value: 'Password1' } });
    fireEvent.change(screen.getByTestId('account-password-repeat'), { target: { value: 'Password1' } });
    fireEvent.click(screen.getByTestId('account-save-password'));

    await waitFor(() =>
      expect(screen.getByTestId('account-error').textContent).toMatch(/used too recently/),
    );
  });

  it('says so when the browser cannot hold a passkey at all', async () => {
    Object.defineProperty(navigator, 'credentials', { configurable: true, value: undefined });

    show();

    await waitFor(() =>
      expect(screen.getByText('This browser cannot hold a passkey.')).toBeTruthy(),
    );
  });
});

// Restore, so the order these run in cannot decide what they see.
afterEach(() => {
  vi.restoreAllMocks();
});
