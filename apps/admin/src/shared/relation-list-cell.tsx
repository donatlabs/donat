import { isRelationField, resourceKey } from '@refinest/core';
import type { AppRuntime } from '@refinest/core';
import { useRelatedRecord } from '@refinest/react';
import { recordDisplayName } from '@refinest/ui-shadcn';

/**
 * Renders a resolved relation value as PLAIN TEXT (no `<a>`), for use as a
 * `renderCell` override on any list surface that already wraps its rows in
 * its own navigation link — `@refinest/ui-shadcn`'s `RelationDisplay`
 * renders an `<a>` unconditionally, and nesting it inside a row link is
 * invalid HTML (`<a>` in `<a>`, React's `validateDOMNesting` warning).
 * Reached here whenever a list's first visible column is a relation, which
 * `order_line` and `shipment` both are (`order_id`).
 *
 * Resolves the name through the SAME `useRelatedRecord` aggregator
 * `RelationDisplay` uses internally (`RelationAggregatorProvider`, batched
 * `getList(id in [...])`), so the related record's name still shows — never
 * a raw id — it just isn't a link. Mirrors `RelationDisplay`'s own loading /
 * unregistered / error / not-found fallbacks so the two read identically
 * save for the anchor.
 */
export function RelationCellText({
  app,
  to,
  id,
}: {
  app: AppRuntime;
  to: string;
  id: string | number;
}): React.ReactElement {
  const targetDef = app.registry.get(to);
  const { record, isLoading, error } = useRelatedRecord(to, id);
  const idStr = String(id);

  if (!targetDef) {
    return <span className="text-muted-foreground">{idStr}</span>;
  }
  if (isLoading) {
    return (
      <span className="inline-block h-5 w-20 animate-pulse rounded bg-muted" aria-busy="true" />
    );
  }
  if (error || !record) {
    return (
      <span className="text-muted-foreground" title={error?.message}>
        {idStr}
      </span>
    );
  }
  return <span>{recordDisplayName(record, targetDef.displayField)}</span>;
}

/**
 * `renderCell` factory for `kind: 'relation'` columns of `resource`. Return
 * `undefined` for every other column so the caller's default cell renderer
 * (enum badges, dates, etc.) is untouched — this ONLY intercepts relation
 * columns, generically, for whichever resource is passed in.
 *
 * Used by the list body (`pages/resources/list.tsx`, `<ResourceListPage
 * renderCell>`), the one place a relation column's cell link can nest inside
 * a row link.
 */
export function createRelationRenderCell(
  app: AppRuntime,
  resource: string,
): (col: string, value: unknown, row: Record<string, unknown>) => React.ReactNode | undefined {
  return (col, value) => {
    const field = app.registry.get(resource)?.fields[col];
    if (!field || !isRelationField(field) || value == null || value === '') {
      return undefined;
    }
    // `isRelationField` isn't a TS type predicate (the framework's
    // signature takes a structural `{ type?, format?, kind? }` shape, not
    // the full `RelationFieldDef`), so `field.meta` is still typed as an
    // optional generic record here — read `to` defensively.
    const rawTo = (field as { meta?: Record<string, unknown> }).meta?.to;
    if (typeof rawTo !== 'string') {
      return undefined;
    }
    const to = resourceKey(rawTo);
    return <RelationCellText app={app} to={to} id={value as string | number} />;
  };
}
