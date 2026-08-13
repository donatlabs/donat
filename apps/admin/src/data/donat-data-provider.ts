import {
  defineDataProvider,
  isDataError,
  type CountResult,
  type DataError,
  type DataProvider,
  type ErasedResourceHandle,
  type FilterExpression,
} from '@refinest/core';
import type { RequestAuth } from '../auth/session';

/**
 * A `@refinest/core` data provider speaking donat's GraphQL surface.
 *
 * donat serves the Donat v2 API shape, so every document this emits is the
 * ordinary one: `<table>(limit, offset, order_by, where)`,
 * `<table>_aggregate { aggregate { count } }`, `<table>_by_pk`,
 * `insert_<table>_one`, `update_<table>_by_pk`, `delete_<table>_by_pk`, with
 * `<table>_bool_exp` / `<table>_set_input` argument types. Root-field naming
 * is `crates/schema/src/naming.rs`; `configuration.custom_root_fields` can
 * rename any of them, which is why {@link ResourceMapping} lets a resource
 * name its own roots instead of always deriving them.
 *
 * What this provider deliberately does NOT do is decide anything about
 * authorization. It attaches whatever {@link DonatDataProviderConfig.authorize}
 * hands it and asks `recover` once on a 401/403. The engine has no admin role:
 * the panel's role is an ordinary role whose per-role permissions are declared
 * in the deployment's metadata, so a column this provider cannot read or write
 * is a permission decision made at deploy time, never something to work around
 * here.
 *
 * Ported from the Solar admin panel's Hasura provider
 * (`apps/admin/src/data/hasura-data-provider.ts` in solar-app-mono), trimmed
 * of what that deployment needed and this one does not: the 1:1 detail-table
 * upsert, the live scope-merging (`getScope`/`scopeFields`), and the
 * `on_conflict` bulk-insert path.
 */

export interface DonatDataProviderConfig {
  /** The engine's GraphQL endpoint. */
  endpoint: string;
  /**
   * What to attach to each request. The panel holds no credential: this is
   * the role header plus `credentials: 'include'`, and the session cookie the
   * engine set is what actually authenticates it.
   */
  authorize: () => RequestAuth;
  /**
   * Invoked once after the engine answers 401/403. Returning true permits a
   * single retry with freshly-obtained auth; false surfaces the rejection.
   */
  recover?: () => Promise<boolean>;
  /** Map a registry resource name onto a table and a field selection. */
  resources: Record<string, ResourceMapping>;
  fetchImpl?: typeof fetch;
}

/**
 * The root fields serving a resource that is NOT a table.
 *
 * donat proxies a REST API into GraphQL with an **action**: a root field
 * backed by an HTTP handler (`handler:` in the metadata), returning a declared
 * output type. That is how a platform surface an engine does not own — the
 * identity provider's users, a billing API, anything with a REST admin API —
 * becomes an ordinary resource in this panel: the credential stays in the
 * engine, and who may call it is an ordinary per-role permission.
 *
 * Names are given rather than derived, because an action names its own field
 * and its own arguments.
 */
export interface ResourceFields {
  /** Query root returning the collection. */
  list: string;
  /** Query root taking the primary key. Falls back to filtering `list`. */
  one?: string;
  /** Mutation roots. Absent means the resource does not offer that operation. */
  create?: string;
  update?: string;
  delete?: string;
  /**
   * What the fields' arguments are called. An action that declares none of
   * these simply gets none — and a list that cannot page, filter or sort says
   * so by refusing, rather than quietly answering about a different set of
   * rows than the caller asked for.
   */
  args?: {
    id?: string;
    input?: string;
    limit?: string;
    offset?: string;
    search?: string;
  };
  /**
   * Read one record by fetching the collection and finding it.
   *
   * Only where the collection is small and bounded — a deployment's roles or
   * scopes — because it is one request for every record opened, and it grows
   * with the collection rather than with what was asked for. A resource whose
   * engine offers no single-record field and whose collection is not small
   * should have no detail page at all (`operations: { show: false }`) rather
   * than a page that downloads everything.
   */
  oneFromList?: boolean;
  /** GraphQL type of the `input` argument, e.g. `IdpUserInput!`. */
  inputType?: string;
  /**
   * GraphQL type of `create`'s `input`, when it is not the one an update
   * takes.
   *
   * They differ more often than not: an update replaces a whole record and
   * demands every field of it, while a create is what somebody types. One name
   * for both is how a create ends up sending fields the engine's create input
   * does not declare.
   */
  createInputType?: string;
  /** GraphQL type of the `search` argument. Defaults to `String`. */
  searchType?: string;
}

export interface ResourceMapping {
  /**
   * GraphQL base name of the table — the `select` root field. Ignored when
   * {@link fields} is present, which is how a resource served by actions
   * rather than by a table is declared.
   */
  table: string;
  /** Serve this resource from named root fields instead of table roots. */
  fields?: ResourceFields;
  /**
   * Fields the engine carries as a list of names and a form edits as text.
   *
   * A form has no list-of-strings widget: the JSON one is declared as an
   * object, so an array is invalid in it and the whole form refuses to save —
   * with no message against any field, which is the worst way to find out.
   * Naming those fields here makes them a comma-separated line on the way out
   * and a list again on the way in, so the wire keeps the shape the schema
   * promised.
   */
  listFields?: string[];
  /**
   * Columns to select. These must be columns the panel's role holds a
   * `select_permission` for; asking for one it does not 400s the whole
   * document at schema validation, before permissions are even consulted.
   */
  selectFields: string[];
  /** Primary-key GraphQL scalar. Defaults to `bigint` (a `bigserial` pk). */
  pkType?: 'bigint' | 'uuid' | 'Int' | 'String';
  /** Primary-key field name in the row. Defaults to `id`. */
  pkField?: string;
  /**
   * Root field for a single-row read. Defaults to `<table>_by_pk`. Set
   * `false` when the role exposes only the collection root; `one` then
   * queries it with a pk `where` and `limit: 1`.
   */
  byPkRoot?: string | false;
  /** Field `list` orders by when the view requests no sort. Defaults to `id`. */
  orderByField?: string;
  /**
   * The aggregate root backing `list`'s inline count and `count`:
   *  - absent — derive `<table>_aggregate`;
   *  - `false` — none exists. donat exposes the aggregate root only for a
   *    role whose `select_permission` sets `allow_aggregations: true`, so
   *    this is a permission fact, declared once per resource. `count`
   *    answers `{ total: null }` without a round trip and `list` omits
   *    `total`;
   *  - a string — an explicitly named root (`custom_root_fields`).
   */
  aggregate?: false | string;
  /**
   * Always-on `where` fragment ANDed into every read and write for this
   * resource, after the user's own filters so they still filter WITHIN it.
   */
  fixedFilter?: Record<string, unknown>;
  /**
   * Columns `updateOne` may write. Absent sends the edited record through
   * unfiltered.
   *
   * Needed whenever `selectFields` carries a column the role cannot update:
   * a generated column, or one outside the role's `update_permission`
   * columns. donat types `<table>_set_input` from exactly those columns, so
   * anything else fails GraphQL validation ("field 'x' not found in type")
   * before the permission engine runs. Set it to the role's writable
   * columns.
   */
  updatableFields?: string[];
}

interface GqlResponse<T> {
  data?: T;
  errors?: Array<{ message: string }>;
}

interface ScopedQuery {
  readonly filter?: FilterExpression<ErasedResourceHandle>;
}

const OP_MAP: Record<string, string> = {
  eq: '_eq',
  neq: '_neq',
  ne: '_neq',
  lt: '_lt',
  lte: '_lte',
  gt: '_gt',
  gte: '_gte',
  in: '_in',
  notIn: '_nin',
  nin: '_nin',
  contains: '_ilike',
  containss: '_like',
  startsWith: '_ilike',
  endsWith: '_ilike',
  startswith: '_ilike',
  endswith: '_ilike',
  null: '_is_null',
};

/** Combine two `where` objects with `_and`. Either or both may be undefined. */
export function mergeWhere(
  a: Record<string, unknown> | undefined,
  b: Record<string, unknown> | undefined,
): Record<string, unknown> | undefined {
  if (!a) return b;
  if (!b) return a;
  return { _and: [a, b] };
}

/** Translate the framework's filter AST into a donat bool expression. */
export function filtersToWhere(
  filter?: FilterExpression<ErasedResourceHandle>,
): Record<string, unknown> | undefined {
  if (!filter || typeof filter !== 'object') return undefined;
  if ('and' in filter) {
    const clauses = filter.and
      .map(filtersToWhere)
      .filter((item): item is Record<string, unknown> => item !== undefined);
    if (clauses.length === 0) return undefined;
    return clauses.length === 1 ? clauses[0] : { _and: clauses };
  }
  if ('or' in filter) {
    const clauses = filter.or
      .map(filtersToWhere)
      .filter((item): item is Record<string, unknown> => item !== undefined);
    if (clauses.length === 0) return undefined;
    return clauses.length === 1 ? clauses[0] : { _or: clauses };
  }
  if ('not' in filter) {
    const clause = filtersToWhere(filter.not);
    return clause === undefined ? undefined : { _not: clause };
  }
  if (!('field' in filter) || typeof filter.field !== 'string' || !('op' in filter))
    return undefined;

  const { field, op } = filter;
  if (op === 'isNull') return { [field]: { _is_null: true } };
  if (op === 'isNotNull') return { [field]: { _is_null: false } };
  if (!('value' in filter) || filter.value === undefined || filter.value === '') return undefined;

  const operator = OP_MAP[op] ?? '_eq';
  let value: unknown = filter.value;
  if (operator === '_ilike' || operator === '_like') {
    const text = String(value);
    value = op === 'startsWith' ? `${text}%` : op === 'endsWith' ? `%${text}` : `%${text}%`;
  }
  return { [field]: { [operator]: value } };
}

function requestError(error: unknown, signal: AbortSignal, fallback: string): DataError {
  if (isDataError(error)) return error;
  if (signal.aborted) {
    return {
      kind: 'cancelled',
      code: 'request-aborted',
      message: 'The request to donat was aborted.',
      cause: error,
    };
  }
  return {
    kind: 'transport',
    code: 'donat-request-failed',
    message: error instanceof Error && error.message ? error.message : fallback,
    cause: error,
  };
}

/**
 * The aggregate root for a mapping, or null when it declares none. Which
 * tables can be counted is a per-role permission fact (`allow_aggregations`),
 * declared once on the mapping rather than discovered per query.
 */
export function aggregateRoot(m: ResourceMapping): string | null {
  if (m.aggregate === false) return null;
  return typeof m.aggregate === 'string' ? m.aggregate : `${m.table}_aggregate`;
}

/** Root field for a by-pk read, or null when the mapping declares none. */
export function byPkRoot(m: ResourceMapping): string | null {
  if (m.byPkRoot === false) return null;
  return m.byPkRoot ?? `${m.table}_by_pk`;
}


/** `($a: Int, $b: String)` for the declared arguments, or nothing. */
function variableList(declared: Array<[string, string, unknown]>): string {
  if (declared.length === 0) return '';
  return `(${declared.map(([name, type]) => `$${name}: ${type}`).join(', ')})`;
}

/** `(a: $a, b: $b)` for the declared arguments, or nothing. */
function argumentList(declared: Array<[string, string, unknown]>): string {
  if (declared.length === 0) return '';
  return `(${declared.map(([name]) => `${name}: $${name}`).join(', ')})`;
}

function variableValues(
  declared: Array<[string, string, unknown]>,
): Record<string, unknown> | undefined {
  if (declared.length === 0) return undefined;
  return Object.fromEntries(declared.map(([name, , value]) => [name, value]));
}

/**
 * The free-text term a filter expression carries, if it is one a search
 * argument can express: a single `contains` leaf. Anything richer belongs to a
 * table, which has a `where` to put it in.
 */
export function searchTerm(
  filter?: FilterExpression<ErasedResourceHandle>,
): string | undefined {
  if (!filter || typeof filter !== 'object') return undefined;
  if ('and' in filter) {
    const terms = filter.and.map(searchTerm).filter((t): t is string => t !== undefined);
    return terms.length === 1 ? terms[0] : undefined;
  }
  if (!('field' in filter) || !('op' in filter) || !('value' in filter)) return undefined;
  if (filter.op !== 'contains' && filter.op !== 'eq') return undefined;
  const value = filter.value;
  return value === undefined || value === '' ? undefined : String(value);
}

export function createDonatDataProvider(config: DonatDataProviderConfig): DataProvider {
  const fetchImpl = config.fetchImpl ?? fetch;

  async function gql<T>(
    query: string,
    variables: Record<string, unknown> | undefined,
    signal: AbortSignal,
  ): Promise<T> {
    try {
      const send = async (auth: RequestAuth) =>
        fetchImpl(config.endpoint, {
          method: 'POST',
          headers: { 'content-type': 'application/json', ...auth.headers },
          credentials: auth.credentials,
          body: JSON.stringify({ query, variables }),
          signal,
        });

      let res = await send(config.authorize());
      // 403 counts as well as 401: a session the engine no longer accepts is
      // an authentication failure, not a permission one.
      // `recover` cannot silently re-authenticate — only the identity
      // provider can — so a true answer means "a fresh session is in hand",
      // and today nothing returns it. The retry stays because the seam is the
      // right shape, not because it fires.
      if ((res.status === 401 || res.status === 403) && config.recover && (await config.recover())) {
        res = await send(config.authorize());
      }
      if (!res.ok) {
        throw {
          kind: res.status === 401 || res.status === 403 ? 'auth' : 'transport',
          code: 'graphql-http-error',
          message: `GraphQL HTTP ${res.status}`,
          status: res.status,
        } satisfies DataError;
      }
      const body = (await res.json()) as GqlResponse<T>;
      if (body.errors?.length) {
        // donat's error shape is part of its conformance contract; the
        // messages are surfaced verbatim rather than reworded.
        throw {
          kind: 'protocol',
          code: 'graphql-error',
          message: body.errors.map((e) => e.message).join('; '),
          details: body.errors,
        } satisfies DataError;
      }
      if (body.data === undefined) {
        throw {
          kind: 'protocol',
          code: 'empty-graphql-response',
          message: 'Empty GraphQL response.',
        } satisfies DataError;
      }
      return body.data;
    } catch (error) {
      throw requestError(error, signal, 'The request to donat failed.');
    }
  }

  function mapping(resource: string): ResourceMapping {
    const m = config.resources[resource];
    if (!m) {
      throw {
        kind: 'configuration',
        code: 'unknown-resource',
        message: `Unknown resource: ${resource}`,
      } satisfies DataError;
    }
    return m;
  }

  /**
   * The `where` shared by `list` and `count`: user filters ANDed with the
   * mapping's `fixedFilter`. `count` MUST reuse this rather than re-derive,
   * so a count can never describe a different set of rows than the page it
   * sits under.
   */
  function composeListWhere(
    m: ResourceMapping,
    descriptor: ScopedQuery,
  ): Record<string, unknown> | undefined {
    return mergeWhere(filtersToWhere(descriptor.filter), m.fixedFilter);
  }

  /** The aggregate COUNT, shared by `list`'s inline count and `count`. */
  async function runAggregateCount(
    m: ResourceMapping,
    root: string,
    where: Record<string, unknown> | undefined,
    signal: AbortSignal,
  ): Promise<number> {
    const whereArg = where ? `($where: ${m.table}_bool_exp)` : '';
    const whereParam = where ? '(where: $where)' : '';
    const query = `query Count_${m.table} ${whereArg} {
      meta: ${root} ${whereParam} { aggregate { count } }
    }`;
    const data = await gql<{ meta: { aggregate: { count: number } } }>(
      query,
      where ? { where } : undefined,
      signal,
    );
    return data.meta.aggregate.count;
  }

  function mutationRecord(
    m: ResourceMapping,
    idField: string,
    item: unknown,
    resource: string,
    expectedId?: string | number,
  ) {
    if (item === null || typeof item !== 'object' || Array.isArray(item)) {
      throw {
        kind: 'protocol',
        code: 'record-not-found',
        message: `Not found: ${resource}`,
      } satisfies DataError;
    }
    const record = item as Record<string, unknown>;
    const id = record[m.pkField ?? idField];
    if (typeof id !== 'string' && (typeof id !== 'number' || !Number.isFinite(id))) {
      throw {
        kind: 'protocol',
        code: 'mutation-record-id-missing',
        message: `Mutation for '${resource}' returned no valid '${m.pkField ?? idField}'.`,
      } satisfies DataError;
    }
    if (expectedId !== undefined && id !== expectedId) {
      throw {
        kind: 'protocol',
        code: 'unexpected-record-id',
        message: `Mutation for '${resource}' returned '${String(id)}' instead of '${String(expectedId)}'.`,
      } satisfies DataError;
    }
    // A mutation selects the declared fields only. Marking the record partial
    // makes callers invalidate dependent reads instead of writing an
    // incomplete row into a one-record cache.
    return { kind: 'record' as const, id, record: item, completeness: 'partial' as const };
  }

  async function readOne(
    m: ResourceMapping,
    id: string | number,
    signal: AbortSignal,
  ): Promise<unknown> {
    const fields = m.selectFields.join(' ');
    const pkType = m.pkType ?? 'bigint';
    const pkField = m.pkField ?? 'id';
    const root = byPkRoot(m);
    if (m.fixedFilter || root === null) {
      const where = mergeWhere({ [pkField]: { _eq: id } }, m.fixedFilter);
      const query = `query One_${m.table}($where: ${m.table}_bool_exp) {
        items: ${m.table}(where: $where, limit: 1) { ${fields} }
      }`;
      const data = await gql<{ items: unknown[] }>(query, { where }, signal);
      return data.items?.[0] ?? null;
    }
    const query = `query One_${m.table}($id: ${pkType}!) {
      item: ${root}(${pkField}: $id) { ${fields} }
    }`;
    const data = await gql<{ item: unknown }>(query, { id }, signal);
    return foldLists(m, data.item);
  }


  /**
   * Read a collection from a root field rather than a table.
   *
   * Paging and search are passed only where the field declares an argument
   * for them. When the caller asks for something the field cannot express,
   * this refuses: a list that silently drops a filter answers about a
   * different set of rows than the one the operator is looking at.
   */
  /** `["a","b"]` → `"a, b"`, for a form to edit as one line. */
  function foldLists(m: ResourceMapping, row: unknown): unknown {
    if (!m.listFields?.length || row === null || typeof row !== 'object') return row;
    const out: Record<string, unknown> = { ...(row as Record<string, unknown>) };
    for (const field of m.listFields) {
      const value = out[field];
      if (Array.isArray(value)) out[field] = value.join(', ');
    }
    return out;
  }

  /** `"a, b"` → `["a","b"]`, for a schema that promised a list. */
  function unfoldLists(
    m: ResourceMapping,
    input: Record<string, unknown> | undefined,
  ): Record<string, unknown> | undefined {
    if (!input || !m.listFields?.length) return input;
    const out: Record<string, unknown> = { ...input };
    for (const field of m.listFields) {
      const value = out[field];
      if (typeof value === 'string') {
        out[field] = value
          .split(',')
          .map((item) => item.trim())
          .filter(Boolean);
      }
    }
    return out;
  }

  async function listFromFields(
    m: ResourceMapping,
    f: ResourceFields,
    descriptor: ScopedQuery & {
      readonly pagination?:
        | { readonly kind: 'offset'; readonly page: number; readonly pageSize: number }
        | { readonly kind: 'cursor' };
      readonly sort?: ReadonlyArray<{ readonly field?: string }>;
    },
    context: { signal: AbortSignal },
  ) {
    const args = f.args ?? {};
    if (descriptor.sort?.some((s) => s.field)) {
      throw {
        kind: 'capability',
        code: 'sort-unsupported',
        message: `'${f.list}' returns the order its handler chose and cannot be sorted here.`,
      } satisfies DataError;
    }
    const search = searchTerm(descriptor.filter);
    if (search !== undefined && !args.search) {
      throw {
        kind: 'capability',
        code: 'filter-unsupported',
        message: `'${f.list}' declares no search argument, so it cannot be filtered.`,
      } satisfies DataError;
    }
    const paging = descriptor.pagination?.kind === 'offset' ? descriptor.pagination : undefined;
    if (paging && !args.limit) {
      // Page 1 of an unpaged field is simply everything it returned; asking
      // for page 2 of it is not.
      if (paging.page > 1) {
        throw {
          kind: 'capability',
          code: 'pagination-unsupported',
          message: `'${f.list}' declares no limit argument and cannot be paged.`,
        } satisfies DataError;
      }
    }

    const declared: Array<[string, string, unknown]> = [];
    if (paging && args.limit) {
      declared.push([args.limit, 'Int', paging.pageSize]);
      if (args.offset) {
        declared.push([args.offset, 'Int', (paging.page - 1) * paging.pageSize]);
      }
    }
    if (search !== undefined && args.search) {
      declared.push([args.search, f.searchType ?? 'String', search]);
    }

    const query = `query List_${f.list}${variableList(declared)} {
      items: ${f.list}${argumentList(declared)} { ${m.selectFields.join(' ')} }
    }`;
    const data = await gql<{ items: unknown[] | null }>(
      query,
      variableValues(declared),
      context.signal,
    );
    // `total` is omitted: an action reports no count, and the page length is
    // not one.
    return { data: (data.items ?? []).map((row) => foldLists(m, row)) };
  }

  async function readOneFromFields(
    m: ResourceMapping,
    f: ResourceFields,
    id: string | number,
    signal: AbortSignal,
  ): Promise<unknown> {
    if (!f.one) {
      if (!f.oneFromList) {
        throw {
          kind: 'capability',
          code: 'read-one-unsupported',
          message: `'${f.list}' declares no single-record field.`,
        } satisfies DataError;
      }
      // Declared, not assumed: the resource said its collection is small
      // enough to read whole. Without a field for one record the alternative
      // is a record page that cannot load, which is how this was found.
      const pk = m.pkField ?? 'id';
      const query = `query One_${f.list} {
        items: ${f.list} { ${m.selectFields.join(' ')} }
      }`;
      const data = await gql<{ items: Array<Record<string, unknown>> | null }>(query, {}, signal);
      const found = (data.items ?? []).find((row) => String(row[pk]) === String(id));
      if (!found) {
        // `protocol`, because the engine answered and the record the caller
        // named was not in what it answered with — there is no `notFound` in
        // this framework's vocabulary.
        throw {
          kind: 'protocol',
          code: 'not-found',
          message: `'${f.list}' has no record ${String(id)}.`,
        } satisfies DataError;
      }
      return foldLists(m, found);
    }
    const idArg = f.args?.id ?? 'id';
    const pkType = m.pkType ?? 'String';
    const query = `query One_${f.one}($id: ${pkType}!) {
      item: ${f.one}(${idArg}: $id) { ${m.selectFields.join(' ')} }
    }`;
    const data = await gql<{ item: unknown }>(query, { id }, signal);
    return foldLists(m, data.item);
  }

  /** Invoke one of the resource's declared mutation fields. */
  async function callFieldMutation(
    m: ResourceMapping,
    f: ResourceFields,
    operation: 'create' | 'update' | 'delete',
    id: string | number | undefined,
    input: Record<string, unknown> | undefined,
    signal: AbortSignal,
  ): Promise<unknown> {
    const field = f[operation];
    if (!field) {
      throw {
        kind: 'capability',
        code: `${operation}-unsupported`,
        message: `'${m.table}' declares no ${operation} field.`,
      } satisfies DataError;
    }
    const declared: Array<[string, string, unknown]> = [];
    if (id !== undefined) {
      declared.push([f.args?.id ?? 'id', `${m.pkType ?? 'String'}!`, id]);
    }
    if (input !== undefined) {
      const inputType =
        operation === 'create' ? (f.createInputType ?? f.inputType ?? 'json!') : (f.inputType ?? 'json!');
      declared.push([f.args?.input ?? 'input', inputType, unfoldLists(m, input)]);
    }
    // A delete returns whatever its handler returns; only the create and
    // update paths need the record back.
    const selection = operation === 'delete' ? '' : ` { ${m.selectFields.join(' ')} }`;
    const query = `mutation ${operation}_${field}${variableList(declared)} {
      item: ${field}${argumentList(declared)}${selection}
    }`;
    const data = await gql<{ item: unknown }>(query, variableValues(declared), signal);
    return foldLists(m, data.item);
  }

  return defineDataProvider({
    queries: {
      async list(descriptor, context) {
        const m = mapping(descriptor.resource);
        if (m.fields) return listFromFields(m, m.fields, descriptor, context);
        if (descriptor.pagination?.kind === 'cursor') {
          throw {
            kind: 'capability',
            code: 'cursor-pagination-unsupported',
            message: 'The donat provider supports offset pagination only.',
          } satisfies DataError;
        }
        const limit = descriptor.pagination?.kind === 'offset' ? descriptor.pagination.pageSize : 25;
        const page = descriptor.pagination?.kind === 'offset' ? descriptor.pagination.page : 1;
        const offset = (page - 1) * limit;
        const where = composeListWhere(m, descriptor);

        const sorters = descriptor.sort?.filter((s) => s.field);
        const orderByExpr =
          sorters && sorters.length > 0
            ? `[${sorters.map((s) => `{${s.field}: ${s.direction}}`).join(', ')}]`
            : `{${m.orderByField ?? 'id'}: desc}`;

        const fields = m.selectFields.join(' ');
        const whereArg = where ? `, $where: ${m.table}_bool_exp` : '';
        const whereParam = where ? ', where: $where' : '';
        const query = `query List_${m.table}($limit: Int, $offset: Int${whereArg}) {
          items: ${m.table}(limit: $limit, offset: $offset, order_by: ${orderByExpr}${whereParam}) { ${fields} }
        }`;
        const vars: Record<string, unknown> = { limit, offset };
        if (where) vars.where = where;
        const data = await gql<{ items: unknown[] }>(query, vars, context.signal);

        // `total` is OMITTED when no honest count is available. Never
        // substitute `items.length`: that is the current page's length, and
        // reporting it as the total collapses the list to a single page.
        let total: number | undefined;
        const root = aggregateRoot(m);
        if (root) {
          try {
            total = await runAggregateCount(m, root, where, context.signal);
          } catch (err) {
            console.warn(`[donat] count failed for ${m.table}; rendering unknown-total`, err);
          }
        }
        return { data: data.items, total };
      },

      /**
       * On-demand row count. `{ total: null }` means "not countable at all" —
       * the mapping says this table has no aggregate root (the role lacks
       * `allow_aggregations`), or the resource is not mapped here. That is a
       * decision. A transient backend failure instead REJECTS, so a retry
       * stays possible; the two are never conflated.
       */
      async count(descriptor, context): Promise<CountResult> {
        const m = config.resources[descriptor.resource];
        if (!m) return { total: null };
        // An action returns what it returns; there is no aggregate root behind
        // it to count with, and guessing would describe a different set of
        // rows than the page it sits under.
        if (m.fields) return { total: null };
        const root = aggregateRoot(m);
        if (!root) return { total: null };
        return {
          total: await runAggregateCount(m, root, composeListWhere(m, descriptor), context.signal),
        };
      },

      async one(descriptor, context) {
        const m = mapping(descriptor.resource);
        const item = m.fields
          ? await readOneFromFields(m, m.fields, descriptor.id, context.signal)
          : await readOne(m, descriptor.id, context.signal);
        if (item == null) {
          throw {
            kind: 'protocol',
            code: 'record-not-found',
            message: `Not found: ${descriptor.resource}/${descriptor.id}`,
          } satisfies DataError;
        }
        return { data: item };
      },

      /**
       * Batch fetch by ids, so relation aggregation collapses N `one` round
       * trips into one query. Like the Solar provider this came from, it does
       * not AND in `fixedFilter`; no mapped resource that opts into `many`
       * declares one.
       */
      async many(descriptor, context) {
        const m = mapping(descriptor.resource);
        if (descriptor.ids.length === 0) return { data: [] };
        if (m.fields) {
          // Relation aggregation batches by `_in`, which an action has no
          // equivalent of. Reading them one at a time would turn one relation
          // column into N round trips behind the caller's back.
          throw {
            kind: 'capability',
            code: 'batch-read-unsupported',
            message: `'${descriptor.resource}' is served by root fields and cannot be batch-read.`,
          } satisfies DataError;
        }
        const pkField = m.pkField ?? 'id';
        const pkType = m.pkType ?? 'bigint';
        const query = `query Many_${m.table}($ids: [${pkType}!]) {
          items: ${m.table}(where: { ${pkField}: { _in: $ids } }) { ${m.selectFields.join(' ')} }
        }`;
        const data = await gql<{ items: unknown[] }>(query, { ids: descriptor.ids }, context.signal);
        return { data: data.items };
      },
    },
    mutations: {
      async createOne(descriptor, context) {
        const m = mapping(descriptor.resource);
        if (m.fields) {
          const item = await callFieldMutation(
            m,
            m.fields,
            'create',
            undefined,
            descriptor.input as Record<string, unknown>,
            context.signal,
          );
          return mutationRecord(m, descriptor.idField, item, descriptor.resource);
        }
        const query = `mutation Create_${m.table}($object: ${m.table}_insert_input!) {
          item: insert_${m.table}_one(object: $object) { ${m.selectFields.join(' ')} }
        }`;
        const data = await gql<{ item: unknown }>(
          query,
          { object: descriptor.input as Record<string, unknown> },
          context.signal,
        );
        return mutationRecord(m, descriptor.idField, data.item, descriptor.resource);
      },

      async updateOne(descriptor, context) {
        const m = mapping(descriptor.resource);
        const fields = m.selectFields.join(' ');
        const input = descriptor.input as Record<string, unknown>;
        if (m.fields) {
          const writable = m.updatableFields
            ? Object.fromEntries(
                Object.entries(input).filter(([key]) => m.updatableFields?.includes(key)),
              )
            : input;
          const item = await callFieldMutation(
            m,
            m.fields,
            'update',
            descriptor.id,
            writable,
            context.signal,
          );
          return mutationRecord(m, descriptor.idField, item, descriptor.resource, descriptor.id);
        }
        // Strip whatever `updatableFields` disallows BEFORE the emptiness
        // check, so a save whose only change was to a stripped key takes the
        // same "nothing to write" path as a genuinely no-op save.
        const set = m.updatableFields
          ? Object.fromEntries(
              Object.entries(input).filter(([key]) => m.updatableFields?.includes(key)),
            )
          : input;
        const pkField = m.pkField ?? descriptor.idField;
        let item: unknown;
        if (Object.keys(set).length === 0) {
          item = await readOne(m, descriptor.id, context.signal);
        } else if (m.fixedFilter) {
          const where = mergeWhere({ [pkField]: { _eq: descriptor.id } }, m.fixedFilter);
          const query = `mutation Update_${m.table}($where: ${m.table}_bool_exp!, $set: ${m.table}_set_input!) {
            result: update_${m.table}(where: $where, _set: $set) { affected_rows returning { ${fields} } }
          }`;
          const data = await gql<{ result: { returning: unknown[] } }>(
            query,
            { where, set },
            context.signal,
          );
          item = data.result.returning?.[0] ?? null;
        } else {
          const pkType = m.pkType ?? 'bigint';
          const query = `mutation Update_${m.table}($id: ${pkType}!, $set: ${m.table}_set_input!) {
            item: update_${m.table}_by_pk(pk_columns: {${pkField}: $id}, _set: $set) { ${fields} }
          }`;
          const data = await gql<{ item: unknown }>(
            query,
            { id: descriptor.id, set },
            context.signal,
          );
          item = data.item;
        }
        return mutationRecord(m, descriptor.idField, item, descriptor.resource, descriptor.id);
      },

      async deleteOne(descriptor, context) {
        const m = mapping(descriptor.resource);
        const fields = m.selectFields.join(' ');
        const pkField = m.pkField ?? descriptor.idField;
        if (m.fields) {
          await callFieldMutation(m, m.fields, 'delete', descriptor.id, undefined, context.signal);
          return { kind: 'identity' as const, id: descriptor.id };
        }
        let item: unknown;
        if (m.fixedFilter) {
          const where = mergeWhere({ [pkField]: { _eq: descriptor.id } }, m.fixedFilter);
          const query = `mutation Delete_${m.table}($where: ${m.table}_bool_exp!) {
            result: delete_${m.table}(where: $where) { affected_rows returning { ${fields} } }
          }`;
          const data = await gql<{ result: { returning: unknown[] } }>(
            query,
            { where },
            context.signal,
          );
          item = data.result.returning?.[0] ?? null;
        } else {
          const pkType = m.pkType ?? 'bigint';
          const query = `mutation Delete_${m.table}($id: ${pkType}!) {
            item: delete_${m.table}_by_pk(${pkField}: $id) { ${fields} }
          }`;
          const data = await gql<{ item: unknown }>(query, { id: descriptor.id }, context.signal);
          item = data.item;
        }
        mutationRecord(m, descriptor.idField, item, descriptor.resource, descriptor.id);
        return { kind: 'identity' as const, id: descriptor.id };
      },
    },
  });
}
