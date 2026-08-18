import { describe, expect, it, vi } from 'vitest';
import {
  aggregateRoot,
  createDonatDataProvider,
  filtersToWhere,
  mergeWhere,
  searchTerm,
  type ResourceMapping,
} from './donat-data-provider';
import type { RequestAuth } from '../auth/session';

const okFetch = (...payloads: unknown[]) => {
  let call = 0;
  return vi.fn<typeof fetch>(async () => {
    const data = payloads[Math.min(call, payloads.length - 1)];
    call += 1;
    return new Response(JSON.stringify({ data }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  });
};

/**
 * The real `@refinest/core` handler signatures are keyed to descriptor types
 * that need the framework's own builders to construct. These tests exercise
 * the provider directly, so the fixtures set only the fields the handlers
 * read and are cast to the handler's parameter type.
 */
type Provider = ReturnType<typeof createDonatDataProvider>;
type ListHandler = NonNullable<NonNullable<Provider['queries']>['list']>;
type OneHandler = NonNullable<NonNullable<Provider['queries']>['one']>;
type CountHandler = NonNullable<NonNullable<Provider['queries']>['count']>;
type CreateOneHandler = NonNullable<NonNullable<Provider['mutations']>['createOne']>;
type UpdateOneHandler = NonNullable<NonNullable<Provider['mutations']>['updateOne']>;
type DeleteOneHandler = NonNullable<NonNullable<Provider['mutations']>['deleteOne']>;
type Context = Parameters<ListHandler>[1];

/**
 * Mappings a deployment would configure. The panel ships none of its own —
 * a stand declares what it has — so the suite declares what it tests against.
 */
const RESOURCE_MAPPINGS: Record<string, ResourceMapping> = {
  category: {
    table: 'category',
    selectFields: ['id', 'slug', 'name'],
    updatableFields: ['slug', 'name'],
    orderByField: 'name',
  },
  product: {
    table: 'product',
    selectFields: ['id', 'category_id', 'slug', 'title', 'description', 'status'],
    updatableFields: ['category_id', 'slug', 'title', 'description', 'status'],
  },
  // A table whose role holds no `allow_aggregations`, so nothing may be
  // counted through it.
  orders: {
    table: 'orders',
    pkType: 'uuid',
    selectFields: ['id', 'customer_id', 'order_status'],
    orderByField: 'created_at',
    aggregate: false,
  },
  shipment: {
    table: 'shipment',
    selectFields: ['id', 'order_id', 'tracking_number'],
    aggregate: false,
  },
  // A table keyed by something other than `id`, selected twice so the record
  // carries the key the framework indexes by.
  inventory_stock: {
    table: 'inventory_stock',
    pkField: 'variant_id',
    selectFields: ['id: variant_id', 'variant_id', 'on_hand', 'reserved', 'available_quantity'],
    updatableFields: ['on_hand', 'reserved'],
    orderByField: 'variant_id',
    aggregate: false,
  },
};

const SESSION: RequestAuth = {
  headers: { 'X-Donat-Role': 'staff' },
  credentials: 'include',
};

function makeProvider(
  fetchImpl: typeof fetch,
  overrides: Partial<Parameters<typeof createDonatDataProvider>[0]> = {},
) {
  return createDonatDataProvider({
    endpoint: 'http://engine.test/v1/graphql',
    authorize: () => SESSION,
    resources: RESOURCE_MAPPINGS,
    fetchImpl,
    ...overrides,
  });
}

function makeContext(): Context {
  return {
    signal: new AbortController().signal,
    requestContext: {},
    session: { sessionKey: 'test-session', scopeKey: null, generation: 0 },
    providerServiceKey: 'test-provider',
  } as Context;
}

/** The GraphQL document of the nth fetch call. */
function documentOf(fetchImpl: ReturnType<typeof okFetch>, call = 0): string {
  const init = fetchImpl.mock.calls[call]?.[1] as RequestInit;
  return (JSON.parse(init.body as string) as { query: string }).query;
}

function variablesOf(fetchImpl: ReturnType<typeof okFetch>, call = 0): Record<string, unknown> {
  const init = fetchImpl.mock.calls[call]?.[1] as RequestInit;
  return (JSON.parse(init.body as string) as { variables: Record<string, unknown> }).variables;
}

describe('filtersToWhere', () => {
  it('maps a contains filter to a wrapped _ilike', () => {
    expect(filtersToWhere({ field: 'title', op: 'contains', value: 'kibble' } as never)).toEqual({
      title: { _ilike: '%kibble%' },
    });
  });

  it('anchors startsWith and endsWith on the correct side', () => {
    expect(filtersToWhere({ field: 'sku', op: 'startsWith', value: 'DOG' } as never)).toEqual({
      sku: { _ilike: 'DOG%' },
    });
    expect(filtersToWhere({ field: 'sku', op: 'endsWith', value: '1KG' } as never)).toEqual({
      sku: { _ilike: '%1KG' },
    });
  });

  it('drops empty leaves instead of emitting a clause that matches everything', () => {
    expect(filtersToWhere({ field: 'title', op: 'eq', value: '' } as never)).toBeUndefined();
    expect(filtersToWhere({ and: [] } as never)).toBeUndefined();
  });

  it('composes and/or/not', () => {
    const where = filtersToWhere({
      and: [
        { field: 'status', op: 'eq', value: 'published' },
        { not: { field: 'slug', op: 'eq', value: 'x' } },
      ],
    } as never);
    expect(where).toEqual({
      _and: [{ status: { _eq: 'published' } }, { _not: { slug: { _eq: 'x' } } }],
    });
  });

  it('reads isNull as an _is_null probe, not as a value comparison', () => {
    expect(filtersToWhere({ field: 'quality_grade', op: 'isNull' } as never)).toEqual({
      quality_grade: { _is_null: true },
    });
  });
});

describe('mergeWhere', () => {
  it('is a no-op when one side is absent', () => {
    expect(mergeWhere(undefined, { a: 1 })).toEqual({ a: 1 });
    expect(mergeWhere({ a: 1 }, undefined)).toEqual({ a: 1 });
  });

  it('ANDs two clauses', () => {
    expect(mergeWhere({ a: 1 }, { b: 2 })).toEqual({ _and: [{ a: 1 }, { b: 2 }] });
  });
});

describe('aggregateRoot', () => {
  it('derives <table>_aggregate by default', () => {
    expect(aggregateRoot({ table: 'product', selectFields: [] })).toBe('product_aggregate');
  });

  it('answers null when the role holds no allow_aggregations', () => {
    expect(aggregateRoot(RESOURCE_MAPPINGS.orders as ResourceMapping)).toBeNull();
  });
});

describe('list', () => {
  it('emits offset pagination, an order_by and an aggregate count', async () => {
    const fetchImpl = okFetch(
      { items: [{ id: '1', title: 'Dog Kibble' }] },
      { meta: { aggregate: { count: 3 } } },
    );
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    const result = await (provider.queries?.list as ListHandler)(
      {
        resource: 'product',
        pagination: { kind: 'offset', page: 2, pageSize: 10 },
      } as never,
      makeContext(),
    );

    const doc = documentOf(fetchImpl);
    expect(doc).toContain('items: product(limit: $limit, offset: $offset, order_by: {id: desc})');
    expect(variablesOf(fetchImpl)).toEqual({ limit: 10, offset: 10 });
    expect(documentOf(fetchImpl, 1)).toContain('meta: product_aggregate  { aggregate { count } }');
    expect(result).toEqual({ data: [{ id: '1', title: 'Dog Kibble' }], total: 3 });
  });

  it('honours the requested sort over the mapping default', async () => {
    const fetchImpl = okFetch({ items: [] }, { meta: { aggregate: { count: 0 } } });
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    await (provider.queries?.list as ListHandler)(
      { resource: 'product', sort: [{ field: 'title', direction: 'asc' }] } as never,
      makeContext(),
    );
    expect(documentOf(fetchImpl)).toContain('order_by: [{title: asc}]');
  });

  it('threads a filter into both the page and its count, as one clause', async () => {
    const fetchImpl = okFetch({ items: [] }, { meta: { aggregate: { count: 0 } } });
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    await (provider.queries?.list as ListHandler)(
      {
        resource: 'product',
        filter: { field: 'status', op: 'eq', value: 'published' },
      } as never,
      makeContext(),
    );
    const where = { status: { _eq: 'published' } };
    expect(variablesOf(fetchImpl).where).toEqual(where);
    expect(variablesOf(fetchImpl, 1).where).toEqual(where);
  });

  it('omits total for a table the role cannot aggregate, and never substitutes the page length', async () => {
    const fetchImpl = okFetch({ items: [{ id: 'a' }, { id: 'b' }] });
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    const result = (await (provider.queries?.list as ListHandler)(
      { resource: 'orders' } as never,
      makeContext(),
    )) as { data: unknown[]; total?: number };

    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(result.total).toBeUndefined();
    expect(result.data).toHaveLength(2);
  });

  it('refuses cursor pagination rather than silently paging from the start', async () => {
    const fetchImpl = okFetch({ items: [] });
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    await expect(
      (provider.queries?.list as ListHandler)(
        { resource: 'product', pagination: { kind: 'cursor' } } as never,
        makeContext(),
      ),
    ).rejects.toMatchObject({ code: 'cursor-pagination-unsupported' });
    expect(fetchImpl).not.toHaveBeenCalled();
  });
});

describe('count', () => {
  it('answers total: null without a round trip when the mapping declares no aggregate', async () => {
    const fetchImpl = okFetch({});
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    await expect(
      (provider.queries?.count as CountHandler)({ resource: 'shipment' } as never, makeContext()),
    ).resolves.toEqual({ total: null });
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it('answers total: null for a resource this provider does not map', async () => {
    const fetchImpl = okFetch({});
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    await expect(
      (provider.queries?.count as CountHandler)({ resource: 'nope' } as never, makeContext()),
    ).resolves.toEqual({ total: null });
  });
});

describe('one', () => {
  it('reads through <table>_by_pk with the declared pk type', async () => {
    const fetchImpl = okFetch({ item: { id: 'u-1', order_status: 'paid' } });
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    await (provider.queries?.one as OneHandler)(
      { resource: 'orders', id: 'u-1' } as never,
      makeContext(),
    );
    const doc = documentOf(fetchImpl);
    expect(doc).toContain('($id: uuid!)');
    expect(doc).toContain('item: orders_by_pk(id: $id)');
  });

  it("names the mapping's own pk column, not a hard-coded id", async () => {
    const fetchImpl = okFetch({ item: { id: 7, variant_id: 7, on_hand: 2 } });
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    await (provider.queries?.one as OneHandler)(
      { resource: 'inventory_stock', id: 7 } as never,
      makeContext(),
    );
    expect(documentOf(fetchImpl)).toContain('item: inventory_stock_by_pk(variant_id: $id)');
  });

  it('rejects a missing row as record-not-found rather than resolving with null', async () => {
    const fetchImpl = okFetch({ item: null });
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    await expect(
      (provider.queries?.one as OneHandler)(
        { resource: 'product', id: 404 } as never,
        makeContext(),
      ),
    ).rejects.toMatchObject({ code: 'record-not-found' });
  });
});

describe('mutations', () => {
  it('creates through insert_<table>_one', async () => {
    const fetchImpl = okFetch({ item: { id: 9, slug: 'birds', name: 'Birds' } });
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    const record = await (provider.mutations?.createOne as CreateOneHandler)(
      { resource: 'category', idField: 'id', input: { slug: 'birds', name: 'Birds' } } as never,
      makeContext(),
    );
    expect(documentOf(fetchImpl)).toContain('item: insert_category_one(object: $object)');
    expect(variablesOf(fetchImpl).object).toEqual({ slug: 'birds', name: 'Birds' });
    expect(record).toMatchObject({ kind: 'record', id: 9, completeness: 'partial' });
  });

  it('strips columns the role cannot update before sending _set', async () => {
    // The row comes back carrying `variant_id`, the mapping's pk field —
    // that is what the returned record is checked against.
    const fetchImpl = okFetch({ item: { id: 5, variant_id: 5, on_hand: 4, reserved: 1 } });
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    await (provider.mutations?.updateOne as UpdateOneHandler)(
      {
        resource: 'inventory_stock',
        idField: 'id',
        id: 5,
        // `available_quantity` is generated and `variant_id` is the key;
        // neither is in the role's update_permissions.
        input: { on_hand: 4, reserved: 1, available_quantity: 3, variant_id: 5 },
      } as never,
      makeContext(),
    );
    expect(variablesOf(fetchImpl).set).toEqual({ on_hand: 4, reserved: 1 });
    expect(documentOf(fetchImpl)).toContain(
      'item: update_inventory_stock_by_pk(pk_columns: {variant_id: $id}, _set: $set)',
    );
  });

  it('re-reads instead of mutating when nothing writable remains', async () => {
    const fetchImpl = okFetch({ item: { id: 5, variant_id: 5, on_hand: 4 } });
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    await (provider.mutations?.updateOne as UpdateOneHandler)(
      {
        resource: 'inventory_stock',
        idField: 'id',
        id: 5,
        input: { available_quantity: 3 },
      } as never,
      makeContext(),
    );
    const doc = documentOf(fetchImpl);
    expect(doc).toContain('query One_inventory_stock');
    expect(doc).not.toContain('mutation');
  });

  it('refuses a mutation that came back describing another row', async () => {
    const fetchImpl = okFetch({ item: { id: 99, slug: 'x', name: 'X' } });
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    await expect(
      (provider.mutations?.updateOne as UpdateOneHandler)(
        { resource: 'category', idField: 'id', id: 5, input: { name: 'X' } } as never,
        makeContext(),
      ),
    ).rejects.toMatchObject({ code: 'unexpected-record-id' });
  });

  it('deletes through delete_<table>_by_pk and answers with the identity', async () => {
    const fetchImpl = okFetch({ item: { id: 5, slug: 'x', name: 'X' } });
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    const result = await (provider.mutations?.deleteOne as DeleteOneHandler)(
      { resource: 'category', idField: 'id', id: 5 } as never,
      makeContext(),
    );
    expect(documentOf(fetchImpl)).toContain('item: delete_category_by_pk(id: $id)');
    expect(result).toEqual({ kind: 'identity', id: 5 });
  });
});

describe('authentication', () => {
  it('attaches exactly the headers and credentials mode the transport returned', async () => {
    const fetchImpl = okFetch({ items: [] }, { meta: { aggregate: { count: 0 } } });
    const provider = makeProvider(fetchImpl as unknown as typeof fetch);
    await (provider.queries?.list as ListHandler)({ resource: 'product' } as never, makeContext());
    const init = fetchImpl.mock.calls[0]?.[1] as RequestInit;
    expect(init.headers).toEqual({
      'content-type': 'application/json',
      'X-Donat-Role': 'staff',
    });
    // The session cookie is what authenticates the request, and it only
    // travels when credentials are included.
    expect(init.credentials).toBe('include');
  });

  it('retries once after a recovered 401, then reports the rejection if it repeats', async () => {
    const unauthorized = () => new Response('', { status: 401 });
    const fetchImpl = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(unauthorized())
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ data: { items: [] } }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
      )
      .mockResolvedValue(
        new Response(JSON.stringify({ data: { meta: { aggregate: { count: 0 } } } }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
      );
    const recover = vi.fn(async () => true);
    const provider = makeProvider(fetchImpl, { recover });
    await (provider.queries?.list as ListHandler)({ resource: 'product' } as never, makeContext());
    expect(recover).toHaveBeenCalledTimes(1);
    expect(fetchImpl).toHaveBeenCalledTimes(3);
  });

  it('surfaces a 403 as an auth error when recovery declines', async () => {
    const fetchImpl = vi
      .fn<typeof fetch>()
      .mockResolvedValue(new Response('', { status: 403 }));
    const provider = makeProvider(fetchImpl, { recover: async () => false });
    await expect(
      (provider.queries?.list as ListHandler)({ resource: 'product' } as never, makeContext()),
    ).rejects.toMatchObject({ kind: 'auth', status: 403 });
  });

  it('surfaces the engine error message verbatim', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(
        JSON.stringify({
          errors: [{ message: 'x-donat-role header is required (this engine has no admin role)' }],
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      ),
    );
    const provider = makeProvider(fetchImpl);
    await expect(
      (provider.queries?.list as ListHandler)({ resource: 'product' } as never, makeContext()),
    ).rejects.toMatchObject({
      code: 'graphql-error',
      message: 'x-donat-role header is required (this engine has no admin role)',
    });
  });
});


/**
 * A resource served by root fields instead of a table: donat proxies a REST
 * API into GraphQL with an action, and the panel binds to the action. This is
 * how the platform's users are managed without the panel ever holding the
 * identity provider's credential.
 */
const IDP_USERS: ResourceMapping = {
  table: 'idp_user',
  pkType: 'String',
  pkField: 'id',
  selectFields: ['id', 'email', 'name', 'roles', 'enabled'],
  updatableFields: ['name', 'roles', 'enabled'],
  fields: {
    list: 'idp_users',
    one: 'idp_user',
    create: 'idp_create_user',
    update: 'idp_update_user',
    delete: 'idp_delete_user',
    args: { id: 'id', input: 'user', limit: 'limit', offset: 'offset', search: 'search' },
    inputType: 'IdpUserInput!',
  },
};

function idpProvider(fetchImpl: typeof fetch) {
  return createDonatDataProvider({
    endpoint: 'http://engine.test/v1/graphql',
    authorize: () => SESSION,
    resources: { ...RESOURCE_MAPPINGS, idp_user: IDP_USERS },
    fetchImpl,
  });
}

describe('searchTerm', () => {
  it('reads the one free-text leaf a search argument can carry', () => {
    expect(searchTerm({ field: 'email', op: 'contains', value: 'sam' } as never)).toBe('sam');
    expect(searchTerm({ and: [{ field: 'email', op: 'contains', value: 'sam' }] } as never)).toBe(
      'sam',
    );
  });

  it('does not flatten a richer expression into one term', () => {
    expect(
      searchTerm({
        and: [
          { field: 'email', op: 'contains', value: 'sam' },
          { field: 'enabled', op: 'eq', value: true },
        ],
      } as never),
    ).toBeUndefined();
    expect(searchTerm({ field: 'created', op: 'gt', value: '2026' } as never)).toBeUndefined();
    expect(searchTerm(undefined)).toBeUndefined();
  });
});

describe('a resource served by root fields', () => {
  it('lists through the declared field, passing only the arguments it has', async () => {
    const fetchImpl = okFetch({ items: [{ id: 'u-1', email: 'sam@example.com' }] });
    const result = (await (idpProvider(fetchImpl as unknown as typeof fetch).queries
      ?.list as ListHandler)(
      {
        resource: 'idp_user',
        pagination: { kind: 'offset', page: 2, pageSize: 25 },
        filter: { field: 'email', op: 'contains', value: 'sam' },
      } as never,
      makeContext(),
    )) as { data: unknown[]; total?: number };

    const doc = documentOf(fetchImpl);
    expect(doc).toContain('($limit: Int, $offset: Int, $search: String)');
    expect(doc).toContain('items: idp_users(limit: $limit, offset: $offset, search: $search)');
    expect(variablesOf(fetchImpl)).toEqual({ limit: 25, offset: 25, search: 'sam' });
    expect(result.data).toHaveLength(1);
    // An action reports no count, and the page length is not one.
    expect(result.total).toBeUndefined();
  });

  it('refuses what the field cannot express instead of answering about other rows', async () => {
    const fetchImpl = okFetch({ items: [] });
    const unpaged: ResourceMapping = {
      ...IDP_USERS,
      fields: { list: 'idp_users', args: {} },
    };
    const provider = createDonatDataProvider({
      endpoint: 'http://engine.test/v1/graphql',
      authorize: () => SESSION,
      resources: { idp_user: unpaged },
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    const list = provider.queries?.list as ListHandler;

    await expect(
      list(
        { resource: 'idp_user', filter: { field: 'email', op: 'contains', value: 'x' } } as never,
        makeContext(),
      ),
    ).rejects.toMatchObject({ code: 'filter-unsupported' });
    await expect(
      list(
        { resource: 'idp_user', sort: [{ field: 'email', direction: 'asc' }] } as never,
        makeContext(),
      ),
    ).rejects.toMatchObject({ code: 'sort-unsupported' });
    await expect(
      list(
        { resource: 'idp_user', pagination: { kind: 'offset', page: 2, pageSize: 25 } } as never,
        makeContext(),
      ),
    ).rejects.toMatchObject({ code: 'pagination-unsupported' });
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it('never counts or batch-reads a field-served resource', async () => {
    const fetchImpl = okFetch({});
    const provider = idpProvider(fetchImpl as unknown as typeof fetch);
    await expect(
      (provider.queries?.count as CountHandler)({ resource: 'idp_user' } as never, makeContext()),
    ).resolves.toEqual({ total: null });
    await expect(
      (provider.queries?.many as NonNullable<NonNullable<Provider['queries']>['many']>)(
        { resource: 'idp_user', ids: ['u-1'] } as never,
        makeContext(),
      ),
    ).rejects.toMatchObject({ code: 'batch-read-unsupported' });
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it('reads one record through the declared single-record field', async () => {
    const fetchImpl = okFetch({ item: { id: 'u-1', email: 'sam@example.com' } });
    await (idpProvider(fetchImpl as unknown as typeof fetch).queries?.one as OneHandler)(
      { resource: 'idp_user', id: 'u-1' } as never,
      makeContext(),
    );
    expect(documentOf(fetchImpl)).toContain('item: idp_user(id: $id)');
    expect(documentOf(fetchImpl)).toContain('($id: String!)');
  });

  it('writes through the declared mutation fields, stripping what the role may not set', async () => {
    const fetchImpl = okFetch({ item: { id: 'u-1', email: 'sam@example.com', name: 'Sam' } });
    await (idpProvider(fetchImpl as unknown as typeof fetch).mutations
      ?.updateOne as UpdateOneHandler)(
      {
        resource: 'idp_user',
        idField: 'id',
        id: 'u-1',
        // `email` is outside `updatableFields`: the identity provider owns it.
        input: { name: 'Sam', email: 'forged@example.com' },
      } as never,
      makeContext(),
    );
    const doc = documentOf(fetchImpl);
    expect(doc).toContain('($id: String!, $user: IdpUserInput!)');
    expect(doc).toContain('item: idp_update_user(id: $id, user: $user)');
    expect(variablesOf(fetchImpl).user).toEqual({ name: 'Sam' });
  });

  it('refuses an operation the resource never declared', async () => {
    const fetchImpl = okFetch({});
    const readOnly: ResourceMapping = {
      ...IDP_USERS,
      fields: { list: 'idp_users', one: 'idp_user', args: { id: 'id' } },
    };
    const provider = createDonatDataProvider({
      endpoint: 'http://engine.test/v1/graphql',
      authorize: () => SESSION,
      resources: { idp_user: readOnly },
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    await expect(
      (provider.mutations?.deleteOne as DeleteOneHandler)(
        { resource: 'idp_user', idField: 'id', id: 'u-1' } as never,
        makeContext(),
      ),
    ).rejects.toMatchObject({ code: 'delete-unsupported' });
    expect(fetchImpl).not.toHaveBeenCalled();
  });
});

describe('a resource whose engine offers only a collection', () => {
  it('reads one record out of the collection, because there is no field for one', async () => {
    const documents: string[] = [];
    const fetchImpl = vi.fn<typeof fetch>(async (_url, init) => {
      documents.push(JSON.parse((init as RequestInit).body as string).query);
      return new Response(
        JSON.stringify({
          data: {
            items: [
              { id: 'a', name: 'first' },
              { id: 'b', name: 'second' },
            ],
          },
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      );
    });
    const provider = makeProvider(fetchImpl, {
      resources: {
        roles: {
          table: 'idp_roles',
          fields: { list: 'idp_roles', create: 'idp_role_create', oneFromList: true },
          selectFields: ['id', 'name'],
          pkType: 'String',
        },
      },
    });

    const record = await (provider.queries?.one as (d: unknown, c: unknown) => Promise<unknown>)(
      { resource: 'roles', id: 'b' },
      makeContext(),
    );

    expect(record).toMatchObject({ data: { id: 'b', name: 'second' } });
    // One document, and it is the collection: there is no single-record field
    // to ask for, and inventing one would be refused by the engine.
    expect(documents).toHaveLength(1);
    expect(documents[0]).toContain('idp_roles');
  });

  it('refuses rather than reading a collection it was not told is small', async () => {
    const fetchImpl = vi.fn<typeof fetch>(async () => {
      throw new Error('nothing should have been asked');
    });
    const provider = makeProvider(fetchImpl, {
      resources: {
        sessions: {
          table: 'idp_sessions',
          // No `one`, and no `oneFromList`: a collection that can be large.
          fields: { list: 'idp_sessions', delete: 'idp_session_delete' },
          selectFields: ['id'],
          pkType: 'String',
        },
      },
    });

    await expect(
      (provider.queries?.one as (d: unknown, c: unknown) => Promise<unknown>)(
        { resource: 'sessions', id: 'x' },
        makeContext(),
      ),
    ).rejects.toMatchObject({ code: 'read-one-unsupported' });
    expect(fetchImpl).not.toHaveBeenCalled();
  });
});
