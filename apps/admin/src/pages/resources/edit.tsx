import { useNavigate, useParams } from 'react-router';
import { useApp, useOne, useUpdateOne } from '@refinest/react';
import { ResourceEditTabs, toast } from '@refinest/ui-shadcn';

/**
 * Data-connected edit body for a registry resource.
 *
 * `onSave` strips every `system: true` field before the mutation. The shipped
 * `<ResourceForm>` renders those read-only but still round-trips their
 * (unchanged) values into the payload — it disables the input, it does not
 * drop the key. donat types `<table>_set_input` from the role's
 * `update_permissions.columns`, so forwarding a generated or non-updatable
 * column fails GraphQL validation ("field 'x' not found in type") before a
 * single real edit could be saved. Stripping by the resource's own `system`
 * flag is resource-agnostic; the mapping's `updatableFields` is the second,
 * data-layer guard for the same rule.
 */
export function ResourceEditBody({ resource }: { resource: string }): React.ReactElement {
  const navigate = useNavigate();
  const { id = '' } = useParams();
  const app = useApp();
  const query = useOne(resource, { id });
  const { mutateAsync } = useUpdateOne(resource);
  const listHref = `/${resource}`;

  if (query.isLoading) {
    return <div className="p-6 text-muted-foreground">Loading…</div>;
  }

  if (query.isError) {
    return (
      <div className="space-y-4 p-6">
        <p className="text-destructive">Failed to load record.</p>
        <button type="button" className="text-sm underline" onClick={() => navigate(listHref)}>
          Back to list
        </button>
      </div>
    );
  }

  const record = query.data?.data as ({ id: unknown } & Record<string, unknown>) | undefined;
  if (!record) {
    return (
      <div className="space-y-4 p-6">
        <p className="text-muted-foreground">Record not found.</p>
        <button type="button" className="text-sm underline" onClick={() => navigate(listHref)}>
          Back to list
        </button>
      </div>
    );
  }

  const fields = (app.registry.get(resource)?.fields ?? {}) as Record<string, { system?: boolean }>;

  return (
    <ResourceEditTabs
      resource={resource}
      record={{ ...record, id: String(record.id) }}
      backHref={listHref}
      onCancel={() => navigate(listHref)}
      onSave={async (values) => {
        const writable = Object.fromEntries(
          Object.entries(values).filter(([key]) => fields[key]?.system !== true),
        );
        await mutateAsync({ id, values: writable });
        toast.success('Changes saved');
        navigate(listHref);
      }}
    />
  );
}
