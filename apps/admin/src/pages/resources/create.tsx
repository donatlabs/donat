import { useNavigate } from 'react-router';
import { hrefFor } from '@refinest/core';
import { useApp, useCreateOne } from '@refinest/react';
import { ResourceCreate, toast } from '@refinest/ui-shadcn';

/**
 * Data-connected create body for a registry resource.
 *
 * `<ResourceCreate>` builds its payload from the resource's own non-`system`
 * fields, so `id` and friends never reach `onCreate` — no stripping needed
 * here, unlike the edit body whose source record already carries those keys.
 */
export function ResourceCreateBody({ resource }: { resource: string }): React.ReactElement {
  const navigate = useNavigate();
  const app = useApp();
  const { mutateAsync } = useCreateOne(resource);
  const listHref = `/${resource}`;

  return (
    <ResourceCreate
      resource={resource}
      onCancel={() => navigate(listHref)}
      onCreate={async (values) => {
        const res = await mutateAsync({ values });
        toast.success('Record created');
        navigate(hrefFor(app.registry, resource, { action: 'show', id: String(res.id) }));
      }}
    />
  );
}
