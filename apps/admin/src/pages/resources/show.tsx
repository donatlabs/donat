import { useRef, useState } from 'react';
import { useNavigate, useParams } from 'react-router';
import { hrefFor } from '@refinest/core';
import { useApp, useDeleteOne, useOne } from '@refinest/react';
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  Button,
  buttonVariants,
  Icon,
  recordDisplayName,
  ResourceShowTabs,
  toast,
  useLinkComponent,
} from '@refinest/ui-shadcn';

/**
 * Data-connected show body for a registry resource.
 *
 * Edit and Delete render only where the resource permits the operation
 * (`operations.edit` / `operations.delete`), which the resource declarations
 * set from the role's actual permissions — the order-side resources are
 * read-only for `staff`, so they show neither.
 *
 * `ResourceShowTabs` also auto-renders every registry action whose `showIn`
 * includes `'show'`, so a resource that declares one needs no wiring here.
 */
export function ResourceShowBody({ resource }: { resource: string }): React.ReactElement {
  const navigate = useNavigate();
  const { id = '' } = useParams();
  const app = useApp();
  const RouterLink = useLinkComponent();
  const deleteTriggerRef = useRef<HTMLButtonElement>(null);
  const query = useOne(resource, { id });
  const { mutateAsync: deleteOne } = useDeleteOne(resource);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);
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

  const def = app.registry.get(resource);
  const canDelete = def?.operations?.delete !== false;
  const canEdit = def?.operations?.edit !== false;
  const editHref = hrefFor(app.registry, resource, { action: 'edit', id });

  async function handleConfirmDelete() {
    setDeleting(true);
    try {
      await deleteOne({ id });
      toast.success('Record deleted');
      navigate(listHref);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : 'Delete failed');
    } finally {
      setDeleting(false);
    }
  }

  return (
    <>
      <ResourceShowTabs
        resource={resource}
        record={{ ...record, id: String(record.id) }}
        extraActions={
          <>
            {canEdit && (
              <Button asChild variant="outline" size="sm" className="gap-1.5">
                <RouterLink href={editHref}>
                  <Icon name="Pencil" />
                  Edit
                </RouterLink>
              </Button>
            )}
            {canDelete && (
              <Button
                ref={deleteTriggerRef}
                variant="destructive"
                size="sm"
                className="gap-1.5"
                onClick={() => setConfirmOpen(true)}
              >
                <Icon name="Trash2" />
                Delete
              </Button>
            )}
          </>
        }
      />
      {canDelete && (
        // The dialog is controlled and its trigger lives outside the Radix
        // <AlertDialogTrigger>, so Radix has no element to hand focus back to
        // and drops it on <body> on close. Restore focus to the opener.
        <AlertDialog
          open={confirmOpen}
          onOpenChange={(open) => {
            setConfirmOpen(open);
            if (!open) requestAnimationFrame(() => deleteTriggerRef.current?.focus());
          }}
        >
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle className="break-words">
                Delete {def?.label?.single ?? resource}?
              </AlertDialogTitle>
              <AlertDialogDescription className="space-y-2">
                {recordDisplayName(record, def?.displayField) && (
                  <span className="block font-medium text-foreground break-words line-clamp-2">
                    {recordDisplayName(record, def?.displayField)}
                  </span>
                )}
                <span className="block">
                  This action cannot be undone. This record will be permanently removed.
                </span>
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel disabled={deleting}>Cancel</AlertDialogCancel>
              <AlertDialogAction
                onClick={handleConfirmDelete}
                disabled={deleting}
                className={buttonVariants({ variant: 'destructive' })}
              >
                {deleting ? 'Deleting…' : 'Delete'}
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      )}
    </>
  );
}
