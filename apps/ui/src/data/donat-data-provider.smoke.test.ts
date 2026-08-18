// @vitest-environment node
//
// Needs Node's real `fetch`, not jsdom's shim: this suite talks to a running
// engine instead of a mocked `fetchImpl`.
import { describe, expect, it } from 'vitest';
import { createDonatDataProvider } from './donat-data-provider';
import { standFromConfig } from '../stands';
import type { RequestAuth } from '../auth/session';

/**
 * The de-risk gate: proves that the documents this provider emits are ones a
 * real donat engine answers, under a real role's real permissions. The unit
 * suite next door asserts the SHAPE of those documents against a mocked fetch;
 * only this one can catch a shape the engine rejects — a root field that does
 * not exist, an argument type that does not match, a column the role may not
 * select.
 *
 * It runs against **your** deployment, because the panel ships none: give it a
 * stand exactly as `VITE_DONAT_STANDS` would, plus a token for the role.
 *
 *   DONAT_SMOKE_URL=http://localhost:8080/v1/graphql \
 *   DONAT_SMOKE_TOKEN=$TOKEN \
 *   DONAT_SMOKE_STAND='{"role":"support","users":{"table":"customer","nameField":"name","emailField":"email"}}' \
 *     npm test -- donat-data-provider.smoke
 *
 * Without `DONAT_SMOKE_STAND` the conventional shape is assumed: a `users`
 * table with `name` and `email`.
 */

const URL_ = process.env.DONAT_SMOKE_URL;
const TOKEN = process.env.DONAT_SMOKE_TOKEN;
const live = URL_ !== undefined && TOKEN !== undefined;

const stand = standFromConfig(
  process.env.DONAT_SMOKE_STAND ? JSON.parse(process.env.DONAT_SMOKE_STAND) : {},
  { graphqlUrl: URL_ ?? '', role: process.env.DONAT_SMOKE_ROLE ?? 'admin' },
);
const users = stand.resources[0];

type Provider = ReturnType<typeof createDonatDataProvider>;
type ListHandler = NonNullable<NonNullable<Provider['queries']>['list']>;
type OneHandler = NonNullable<NonNullable<Provider['queries']>['one']>;
type CountHandler = NonNullable<NonNullable<Provider['queries']>['count']>;
type Context = Parameters<ListHandler>[1];

const auth = (): RequestAuth => ({
  // In a browser the token rides in the cookie the engine's own login set;
  // here it is a bearer header, which the engine reads the same way.
  headers: { authorization: `Bearer ${TOKEN ?? ''}`, 'X-Donat-Role': stand.role },
  credentials: 'include',
});

function provider(role = stand.role): Provider {
  return createDonatDataProvider({
    endpoint: URL_ ?? '',
    authorize: () => ({ ...auth(), headers: { ...auth().headers, 'X-Donat-Role': role } }),
    resources: { [users.name]: users.mapping },
  });
}

function ctx(): Context {
  return {
    signal: new AbortController().signal,
    requestContext: {},
    session: { sessionKey: 'smoke', scopeKey: null, generation: 0 },
    providerServiceKey: 'smoke',
  } as Context;
}

describe.skipIf(!live)('against a running engine', () => {
  it('lists this deployment’s people through the configured role', async () => {
    const result = (await (provider().queries?.list as ListHandler)(
      { resource: users.name, pagination: { kind: 'offset', page: 1, pageSize: 5 } } as never,
      ctx(),
    )) as { data: Array<Record<string, unknown>> };
    expect(Array.isArray(result.data)).toBe(true);
    // Every column the screen shows is one the role could actually read: had
    // any of them been outside its permission, the document would have failed
    // validation rather than returning rows.
    if (result.data.length > 0) {
      expect(Object.keys(result.data[0]).length).toBeGreaterThan(0);
    }
  });

  it('reads one of them by primary key', async () => {
    const list = (await (provider().queries?.list as ListHandler)(
      { resource: users.name, pagination: { kind: 'offset', page: 1, pageSize: 1 } } as never,
      ctx(),
    )) as { data: Array<{ id: string | number }> };
    const first = list.data[0];
    if (!first) return; // an empty deployment proves nothing here
    const one = (await (provider().queries?.one as OneHandler)(
      { resource: users.name, id: first.id } as never,
      ctx(),
    )) as { data: { id: string | number } };
    expect(String(one.data.id)).toBe(String(first.id));
  });

  it('counts only where the role was granted aggregations', async () => {
    const counted = (await (provider().queries?.count as CountHandler)(
      { resource: users.name } as never,
      ctx(),
    )) as { total: number | null };
    // `null` is a decision (the mapping says the role cannot aggregate), a
    // number is an answer; a transient failure would have rejected instead.
    expect(counted.total === null || typeof counted.total === 'number').toBe(true);
  });

  it('is refused a role the token never granted', async () => {
    await expect(
      (provider('no-such-role').queries?.list as ListHandler)(
        { resource: users.name } as never,
        ctx(),
      ),
    ).rejects.toMatchObject({ kind: 'protocol' });
  });
});
