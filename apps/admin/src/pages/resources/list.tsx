import { useApp } from '@refinest/react';
import { ResourceListPage } from '@refinest/ui-shadcn';
import { createRelationRenderCell } from '../../shared/relation-list-cell';

/**
 * Data-connected list body for a registry resource: toolbar, filter chips,
 * saved views, table, pagination and bulk bar are all framework-owned.
 *
 * `renderCell` overrides relation columns to render the resolved name as
 * plain text — see `shared/relation-list-cell.tsx` for why.
 */
export function ResourceListBody({ resource }: { resource: string }): React.ReactElement {
  const app = useApp();
  return <ResourceListPage resource={resource} renderCell={createRelationRenderCell(app, resource)} />;
}
