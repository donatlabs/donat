import { render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AppRoutes } from './app';
import { BrowserRouter } from 'react-router';
import { Providers } from './components/providers';
import { standFromConfig, standsFromEnv } from './stands';

/**
 * Two stands, as a deployment would configure them: the same engine seen
 * through two roles. The panel ships no deployment of its own, so a test
 * supplies one exactly the way `VITE_DONAT_STANDS` does.
 */
const STANDS = [
  standFromConfig(
    {
      id: 'support',
      label: 'Support desk',
      role: 'support',
      users: { table: 'customer', nameField: 'name', emailField: 'email' },
    },
    { graphqlUrl: '/v1/graphql', role: 'admin' },
  ),
  standFromConfig(
    {
      id: 'back-office',
      label: 'Back office',
      role: 'operator',
      users: { table: 'account', nameField: 'full_name', emailField: 'login' },
    },
    { graphqlUrl: '/v1/graphql', role: 'admin' },
  ),
];

function App(): React.ReactElement {
  return (
    <BrowserRouter>
      <Providers stands={STANDS}>
        <AppRoutes />
      </Providers>
    </BrowserRouter>
  );
}

/**
 * Mount test for the whole application.
 *
 * The unit and smoke suites cover the data layer; nothing there would notice a
 * broken mount — a provider in the wrong order, a registry that never becomes
 * ready, a resource whose relation target is registered after it (that last
 * one is how the group/resource name collision that sent `hrefFor` into
 * infinite recursion was found). This renders the real `<App>` over a stubbed
 * engine.
 *
 * It also covers the thing a data-layer test cannot see at all: switching
 * stands has to rebuild the registry, the endpoint and the cache together.
 */

const PEOPLE = [{ id: 1, name: 'Alice', email: 'alice@example.com' }];
const OTHER_PEOPLE = [{ id: 9, full_name: 'Morgan', login: 'morgan@example.com' }];

/**
 * Answers whichever collection the document asked for, and answers
 * `/auth/session` the way a deployment with an unauthorized role does:
 * successfully, with `authenticated` saying which caller this is.
 */
function engineStub(status = 200, authenticated = true, roles?: string[]) {
  return vi.fn<typeof fetch>(async (input, init) => {
    if (String(input).includes('/auth/session')) {
      return new Response(
        JSON.stringify({
          authenticated,
          role: authenticated ? 'support' : 'anonymous',
          // Both stands' roles by default: a panel serving several stands is
          // one person whose token grants each of them, and a token that did
          // not would (correctly) be refused at the stand it does not cover.
          roles: roles ?? (authenticated ? ['support', 'operator'] : ['anonymous']),
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      );
    }
    if (status !== 200) return new Response('', { status });
    const body = JSON.parse((init?.body as string) ?? '{}') as { query: string };
    const data = body.query.includes('aggregate')
      ? { meta: { aggregate: { count: 1 } } }
      : { items: body.query.includes('customer(') ? PEOPLE : OTHER_PEOPLE };
    return new Response(JSON.stringify({ data }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  });
}

/** The panel navigates the whole browser to log in; jsdom implements neither. */
function stubNavigation() {
  const assign = vi.fn();
  Object.defineProperty(window, 'location', {
    configurable: true,
    value: { ...window.location, assign, pathname: '/users' },
  });
  return assign;
}

function roleOf(call: [unknown, RequestInit | undefined] | undefined): unknown {
  return (call?.[1]?.headers as Record<string, string> | undefined)?.['X-Donat-Role'];
}

describe('the panel', () => {
  beforeEach(() => {
    window.localStorage.clear();
    window.history.pushState({}, '', '/users');
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    window.localStorage.clear();
  });

  it('opens on the platform view: this deployment’s people', async () => {
    vi.stubGlobal('fetch', engineStub());
    render(<App />);

    await waitFor(
      () => {
        expect(screen.getByText('Alice')).toBeDefined();
      },
      { timeout: 5000 },
    );

    // The sidebar is registry-driven, and the stand switcher names every
    // deployment this panel serves.
    expect(screen.getAllByText(/Users/i).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/Back office/i).length).toBeGreaterThan(0);

    // The active role is named in the chrome, because it is the only thing
    // that decides what this panel can do — and it is the stand's role, not a
    // global one.
    expect(screen.getByText('role: support')).toBeDefined();

    // The first call establishes who the caller is; the data plane is only
    // asked afterwards, and asked as the stand's role.
    const calls = (globalThis.fetch as ReturnType<typeof engineStub>).mock.calls;
    // Two calls establish who the caller is — the engine's session and the
    // provider's CSRF token — and they go together, so neither is "first".
    expect(calls.some((call) => String(call[0]).includes('/auth/session'))).toBe(true);
    const dataCalls = calls.filter((call) => !String(call[0]).startsWith('/auth/'));
    expect(dataCalls.length).toBeGreaterThan(0);
    expect(roleOf(dataCalls[0] as never)).toBe('support');
    // Every request carries the cookie; the panel holds no credential itself.
    expect(dataCalls[0]?.[1]?.credentials).toBe('include');
  });

  it('switches stands, and the role switches with it', async () => {
    vi.stubGlobal('fetch', engineStub());
    const { getAllByText } = render(<App />);

    await waitFor(() => expect(screen.getByText('Alice')).toBeDefined(), { timeout: 5000 });

    getAllByText(/Back office/i)[0].click();

    await waitFor(
      () => {
        expect(screen.getByText('role: operator')).toBeDefined();
      },
      { timeout: 5000 },
    );
    // The other deployment's people are what the screen shows now.
    await waitFor(() => expect(screen.getByText('Morgan')).toBeDefined());

    // Requests from the stand that was replaced may still be settling, so the
    // claim is about what the new stand asked as, not about which call landed
    // last: the catalogue stand read something, and read it as `staff`.
    const calls = (globalThis.fetch as ReturnType<typeof engineStub>).mock.calls;
    const asOperator = calls.filter((call) => roleOf(call as never) === 'operator');
    expect(asOperator.length).toBeGreaterThan(0);
    // A stand IS a role: the second deployment's table was never asked for as
    // the first one's role, and neither the other way round.
    expect(
      asOperator.every((call) => !String((call[1]?.body as string) ?? '').includes('customer(')),
    ).toBe(true);
  });

  it('hands the browser to the login route when the engine says it is nobody', async () => {
    const assign = stubNavigation();
    // The case that motivated the check: the engine answers *successfully* as
    // its unauthorized role, so nothing is refused and there is no 401 to
    // react to. The panel still has to send the operator to log in.
    vi.stubGlobal('fetch', engineStub(200, false));
    render(<App />);

    await waitFor(
      () => {
        expect(assign).toHaveBeenCalled();
      },
      { timeout: 5000 },
    );
    const target = assign.mock.calls[0]?.[0] as string;
    expect(target).toContain('/auth/login');
    // …and asks to come back where the operator was.
    expect(target).toContain(`redirect=${encodeURIComponent('/users')}`);

    // Nothing was asked of the data plane before the caller was known.
    const calls = (globalThis.fetch as ReturnType<typeof engineStub>).mock.calls;
    // Nothing of the data plane was asked: only the two calls that establish
    // who the caller is.
    expect(calls.every((call) => String(call[0]).startsWith('/auth/'))).toBe(true);
  });
  /**
   * Signed in, and still not allowed: a token that grants some role other than
   * the one this stand runs as. Every query would come back "your requested
   * role is not in allowed roles", so the panel says it once instead — and
   * offers the only thing that helps, which is signing out.
   */
  it('says so when the account holds none of the roles this stand runs as', async () => {
    vi.stubGlobal('fetch', engineStub(200, true, ['reader']));

    render(<App />);

    await waitFor(() => {
      expect(screen.getByTestId('wrong-role')).toBeTruthy();
    });
    expect(screen.getByTestId('wrong-role').textContent).toMatch(/support/);
    expect(screen.getByTestId('wrong-role').textContent).toMatch(/reader/);
    expect(screen.getByTestId('wrong-role-sign-out')).toBeTruthy();
    // Nothing was asked of the data plane: the answer was already known.
    const calls = (globalThis.fetch as ReturnType<typeof engineStub>).mock.calls;
    // Nothing of the data plane was asked: only the two calls that establish
    // who the caller is.
    expect(calls.every((call) => String(call[0]).startsWith('/auth/'))).toBe(true);
  });

  it('does not refuse anyone on the strength of a field an older engine never sent', async () => {
    // No `roles` at all — which is not the same as an empty list, and must not
    // be read as "this account holds none".
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(async (input) => {
        if (String(input).includes('/auth/session')) {
          return new Response(JSON.stringify({ authenticated: true, role: 'support' }), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          });
        }
        return new Response(JSON.stringify({ data: { items: [], total: { aggregate: { count: 0 } } } }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        });
      }),
    );

    render(<App />);

    await waitFor(() => {
      expect(screen.getAllByText(/Users/i).length).toBeGreaterThan(0);
    });
    expect(screen.queryByTestId('wrong-role')).toBeNull();
  });

  /**
   * The platform view: a stand that declares no people of its own manages the
   * identity provider's, and those five screens are one section rather than
   * five siblings. Worth a mount test twice over — a group and a resource
   * sharing a name is what once sent `hrefFor` into infinite recursion, and a
   * collapsed section that never renders its children looks identical to a
   * registry that never got them.
   */
  it('gathers the identity screens under one section', async () => {
    vi.stubGlobal('fetch', engineStub());
    // Deliberately the *unconfigured* path — no VITE_DONAT_STANDS at all,
    // which is how the platform example runs. A stand with no id of its own
    // derives one from its role and endpoint (`support@/v1/graphql`), and that
    // string then has to survive being turned into a registry key.
    const platform = standsFromEnv(undefined, { graphqlUrl: '/v1/graphql', role: 'support' });

    render(
      <BrowserRouter>
        <Providers stands={platform}>
          <AppRoutes />
        </Providers>
      </BrowserRouter>,
    );

    await waitFor(
      () => {
        expect(screen.getAllByText(/Identity/i).length).toBeGreaterThan(0);
      },
      { timeout: 5000 },
    );
    for (const label of [
      'Users',
      'Roles',
      'Groups',
      'Scopes',
      'Applications',
      'Attributes',
      'Blocked addresses',
      'Sessions',
    ]) {
      expect(
        screen.getAllByText(new RegExp(`^${label}$`, 'i')).length,
        `${label} is not in the sidebar`,
      ).toBeGreaterThan(0);
    }
  });
});
