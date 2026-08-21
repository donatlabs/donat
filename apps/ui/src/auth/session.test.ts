import { afterEach, describe, expect, it, vi } from 'vitest';

import { createTransport } from './session';

/**
 * The panel reads the tenant to *say* which store it is looking at. It never
 * sends one: a tenant reaches the engine in the token and nowhere else, so a
 * value the browser could set would be a value the browser could change.
 */
describe('the session a tenanted engine reports', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  const answer = (body: unknown) => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify(body), { status: 200 })),
    );
  };

  it('carries the store the caller is signed into', async () => {
    answer({ authenticated: true, role: 'staff', roles: ['staff'], tenant: 'tenant-alpha' });
    const state = await createTransport().session();
    expect(state.tenant).toBe('tenant-alpha');
    expect(state.role).toBe('staff');
  });

  /**
   * Signed in and in no store yet is the state a store switcher exists for, so
   * it must be distinguishable from being signed out.
   */
  it('says null for somebody signed in and not in a store', async () => {
    answer({ authenticated: true, role: 'platform_visitor', roles: ['platform_visitor'], tenant: null });
    const state = await createTransport().session();
    expect(state.authenticated).toBe(true);
    expect(state.tenant).toBeNull();
  });

  /** A deployment with no tenants omits the field entirely. */
  it('says null when the engine does not mention tenants at all', async () => {
    answer({ authenticated: true, role: 'support', roles: ['support'] });
    const state = await createTransport().session();
    expect(state.tenant).toBeNull();
  });

  it('says null when the engine cannot be reached', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new Error('offline');
      }),
    );
    const state = await createTransport().session();
    expect(state.authenticated).toBe(false);
    expect(state.tenant).toBeNull();
  });
});
