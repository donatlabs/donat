import { navHrefFor, resolveStaticAvailability } from '@refinest/core';
import { useApp, useOne, useRecordTitle } from '@refinest/react';
import { Breadcrumbs, useMatch, useNavVisible } from '@refinest/ui-shadcn';
import type * as React from 'react';

/**
 * Header breadcrumbs derived from the resolved route (`useMatch()`) rather
 * than parsed out of the pathname. Trimmed to what this panel has: flat
 * resources in flat groups, so there is no group chain to walk, no i18n
 * label lookup, and no custom-page crumb to render.
 *
 * The framework's own `<Breadcrumbs resource pageLabel>` draws the resource
 * segment (a link to its list) and the trailing page label; we only compute
 * the trailing label — "Create"/"Edit", or the record's resolved title on a
 * show page.
 */
export function AppBreadcrumbs(): React.ReactElement | null {
  const app = useApp();
  const isVisible = useNavVisible();
  const match = useMatch();

  // ⚠️ Hooks run before the early return: `useOne`/`useRecordTitle` are hooks,
  // so bailing out first would make the hook count depend on the route and
  // tear the tree down on the first soft navigation between route kinds.
  const recordId =
    (match?.kind === 'resource' || match?.kind === 'singleton') && match.action !== 'create'
      ? match.id
      : undefined;
  const resourceName = match?.name ?? 'platform_event';
  const query = useOne(resourceName, {
    id: recordId ?? '',
    enabled: !!match && !!recordId,
  });
  const title = useRecordTitle(match?.name, recordId, query.data?.data);

  if (!match || (match.kind !== 'resource' && match.kind !== 'singleton')) return null;

  const pageLabel =
    match.action === 'create'
      ? 'Create'
      : match.action === 'edit'
        ? 'Edit'
        : recordId
          ? title || recordId
          : undefined;

  // On edit, give the record its own crumb linking back to show — otherwise
  // "Events › Edit" drops the record and the only way back is the browser.
  // Gated on the resource actually having a show view (`resolveStaticAvailability`).
  const showAvailable =
    resolveStaticAvailability(app.registry, match.name, 'show').state === 'available';
  const middle =
    match.action === 'edit' && recordId && showAvailable
      ? [
          {
            label: title || recordId,
            href: navHrefFor(app.registry, match.name, { isVisible, action: 'show', id: recordId }),
          },
        ]
      : undefined;

  return (
    <Breadcrumbs
      resource={match.name}
      recordId={recordId}
      pageLabel={pageLabel}
      middleSegments={middle}
      data-testid="header-breadcrumbs"
    />
  );
}
