import { defineAppDefinition, type CreateAppRuntimeOptions } from '@refinest/core';
import { fieldTypesPlugin } from '@refinest/field-types';
import type { Stand } from './stands';

/**
 * The resource registry for one stand.
 *
 * Registering a resource is also what puts it in the sidebar and what
 * generates its `list` / `show` / `edit` / `create` routes: `app.tsx` reads
 * `generateRouteDescriptors(app)` and `<NavMain>` reads the registry, so there
 * is no separate nav or route table to keep in sync.
 *
 * The registry is per stand rather than global, because a stand is a
 * deployment seen through one role and its resources are that role's. The app
 * runtime is rebuilt when the stand changes — switching stands is switching
 * backends, and pretending otherwise would leave one stand's cached rows
 * rendering under another's permissions.
 *
 * Nothing here can widen access. The registry decides what the panel
 * *offers*; the deployment's per-role permissions decide what the engine
 * *allows*, and the second always wins.
 */
/**
 * A stand's id, as a registry key.
 *
 * A stand that names no id of its own gets one describing what it is —
 * `support@/v1/graphql`, the role at the endpoint — which is right for
 * identity and for remembering a choice, and wrong for a contribution id: the
 * registry accepts letters, digits and `._:-` and refuses the rest. Two stands
 * that differ only in punctuation would collide here, which is why the id
 * itself is not changed to suit this and this is not used for anything else.
 *
 * The application's own `name` below is left as it is: it carries the raw id,
 * nothing has ever rejected it, and it is the kind of identifier persisted
 * state tends to key on — so changing it to match would trade a latent problem
 * for a real one.
 */
function registryKey(id: string): string {
  return id.replace(/[^\p{L}\p{N}._:-]+/gu, '-').replace(/^-+|-+$/g, '') || 'stand';
}

export function createAdminApp(stand: Stand, runtimeOptions: CreateAppRuntimeOptions = {}) {
  const definition = defineAppDefinition(
    {
      name: `donat-admin-${stand.id}`,
      plugins: [fieldTypesPlugin()],
    },
    (setup) => {
      if (stand.groups?.length) {
        setup.use(`donat.${registryKey(stand.id)}-groups`, ...stand.groups);
      }
      // Registration order is relation order: a `f.relation({ to })` resolves
      // against a resource that is already registered.
      stand.resources.forEach((entry, index) => {
        setup.use(`donat.${String(index).padStart(2, '0')}-${entry.name}`, entry.definition);
      });
    },
  );
  return definition.createRuntime(runtimeOptions);
}
