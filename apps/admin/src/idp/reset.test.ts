import { describe, expect, it } from 'vitest';

import { parseReset, ResetClient, ResetError, templateValue } from './reset';

const page = (values: Record<string, string>) =>
  `<!doctype html><html><body>${Object.entries(values)
    .map(([id, value]) => `<template id="${id}">${value}</template>`)
    .join('')}<div>Set a new password</div></body></html>`;

const good = {
  tpl_csrf_token: 'pwd-csrf',
  tpl_magic_link_id: 'link-1',
  tpl_user_id: 'u-1',
  tpl_password_policy: '{"length_min":8,"length_max":128}',
};

describe('templateValue', () => {
  it('reads the channel the provider hands its own frontend', () => {
    expect(templateValue(page(good), 'tpl_csrf_token')).toBe('pwd-csrf');
  });

  it('is undefined rather than empty for one that is not there', () => {
    expect(templateValue(page(good), 'tpl_nothing')).toBeUndefined();
  });

  it('parses rather than pattern-matches, so a value with markup in it survives', () => {
    expect(templateValue(page({ tpl_x: 'a &lt; b' }), 'tpl_x')).toBe('a < b');
  });
});

describe('parseReset', () => {
  it('takes everything the form needs from one request', () => {
    expect(parseReset(page(good), 'fallback')).toEqual({
      userId: 'u-1',
      magicLinkId: 'link-1',
      csrfToken: 'pwd-csrf',
      needsMfa: false,
      policy: { length_min: 8, length_max: 128 },
    });
  });

  it('falls back to the id in the link when the page does not repeat it', () => {
    const { tpl_user_id: _, ...without } = good;
    expect(parseReset(page(without), 'from-url').userId).toBe('from-url');
  });

  it('notices when a second factor is wanted before the change', () => {
    expect(parseReset(page({ ...good, tpl_needs_mfa: 'true' }), 'u-1').needsMfa).toBe(true);
  });

  it('says a link is spent rather than rendering a form that cannot work', () => {
    // The provider answers a used or expired link with a page carrying no
    // token, because there is nothing left to authorise.
    expect(() => parseReset(page({ tpl_error_text: 'gone' }), 'u-1')).toThrow(ResetError);
    expect(() => parseReset('<html><body>no</body></html>', 'u-1')).toThrow(/used already/);
  });
});

describe('ResetClient', () => {
  const client = (responder: (url: string, init?: RequestInit) => Response) => {
    const calls: { url: string; init: RequestInit | undefined }[] = [];
    const instance = new ResetClient('/auth/v1', (input, init) => {
      calls.push({ url: String(input), init });
      return Promise.resolve(responder(String(input), init));
    });
    return { instance, calls };
  };

  it('follows the emailed link itself, which is what sets the binding cookie', async () => {
    const { instance, calls } = client(() => new Response(page(good), { status: 200 }));

    await expect(instance.open('u-1', 'link-1')).resolves.toMatchObject({ csrfToken: 'pwd-csrf' });

    expect(calls[0].url).toBe('/auth/v1/users/u-1/reset/link-1');
    expect(calls[0].init?.credentials).toBe('same-origin');
  });

  it('sends the reset token in its own header, not the session one', async () => {
    const { instance, calls } = client((url) =>
      url.endsWith('/reset/link-1')
        ? new Response(page(good), { status: 200 })
        : new Response(null, { status: 200, headers: { location: '/auth/v1/account' } }),
    );

    const info = await instance.open('u-1', 'link-1');
    await expect(instance.change(info, 'Password1')).resolves.toBe('/auth/v1/account');

    const put = calls[1];
    expect(put.url).toBe('/auth/v1/users/u-1/reset');
    expect(put.init?.method).toBe('PUT');
    expect(new Headers(put.init?.headers).get('x-pwd-csrf-token')).toBe('pwd-csrf');
    expect(JSON.parse(String(put.init?.body))).toEqual({
      password: 'Password1',
      magic_link_id: 'link-1',
      mfa_code: undefined,
    });
  });

  it('goes where the provider says, and to the account when it says nothing', async () => {
    const { instance } = client((url) =>
      url.endsWith('/reset/link-1')
        ? new Response(page(good), { status: 200 })
        : new Response(null, { status: 200 }),
    );

    const info = await instance.open('u-1', 'link-1');
    await expect(instance.change(info, 'Password1')).resolves.toBe('/account');
  });

  it('keeps the provider\'s wording when it refuses the new password', async () => {
    const { instance } = client((url) =>
      url.endsWith('/reset/link-1')
        ? new Response(page(good), { status: 200 })
        : new Response(JSON.stringify({ message: 'must not be one of the last 4 used' }), {
            status: 400,
            headers: { 'content-type': 'application/json' },
          }),
    );

    const info = await instance.open('u-1', 'link-1');
    await expect(instance.change(info, 'Password1')).rejects.toThrow(/last 4 used/);
  });

  it('treats a refused link as spent', async () => {
    const { instance } = client(() => new Response(null, { status: 404 }));

    await expect(instance.open('u-1', 'gone')).rejects.toThrow(/used already/);
  });
});
