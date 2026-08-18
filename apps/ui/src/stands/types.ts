import { defineResource } from '@refinest/field-types';
import type { ResourceMapping } from '../data/donat-data-provider';

/**
 * A **stand** is one donat deployment seen through one role.
 *
 * The role is part of the identity of a stand, not a setting inside it,
 * because this engine has no admin role: the panel *is* a role, and what it
 * can see is exactly that role's per-role permissions. Two roles against the
 * same endpoint are two stands, and that is the honest description — they show
 * different data and can do different things.
 */
export interface Stand {
  /** Stable key; used in storage and as the app runtime's remount key. */
  id: string;
  label: string;
  /** Where this deployment serves GraphQL. Relative stays same-origin. */
  graphqlUrl: string;
  /** The role every request from this stand asserts. */
  role: string;
  /**
   * Sidebar groups this stand's resources join by name. Registered before the
   * resources, so every `group:` reference resolves against one that exists —
   * an unresolved reference rejects the whole app definition at startup.
   *
   * A group name shares a namespace with the resource names: a group called
   * `orders` beside a resource of that name sends `hrefFor` into infinite
   * recursion, because it resolves the group first and then to its own first
   * child forever.
   */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  groups?: any[];
  /** What the panel offers for this stand. */
  resources: StandResource[];
}

/** One registry resource together with how it reaches the engine. */
export interface StandResource {
  /** The `defineResource` declaration. */
  // The framework's resource type is deeply generic over its own field map;
  // a heterogeneous list of them has no useful common type, and every use here
  // is `setup.use(...)`, which erases it anyway.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  definition: any;
  /** The registry name it was declared with. */
  name: string;
  mapping: ResourceMapping;
}

/**
 * How one deployment's people are reached.
 *
 * Every deployment has them and every deployment spells them differently: a
 * `customer` table here, an `account` there, and — where the panel manages the
 * *login* rather than the row — an action proxying the identity provider's
 * REST API into GraphQL (see `ResourceFields`). The panel renders one Users
 * screen from this declaration whichever it is, because "who can get in" is
 * the platform's question, not any one application's.
 */
export interface StandUsers {
  /** Table root, or the `table` name a field-served resource reports as. */
  table: string;
  /** Column carrying the person's display name. */
  nameField: string;
  /** Column carrying the address they sign in with. */
  emailField?: string;
  /** Column carrying the identity the deployment knows them by. */
  identityField?: string;
  /** Anything else worth a column, in the order it should appear. */
  extraFields?: Array<{
    name: string;
    label: string;
    kind?: 'string' | 'number' | 'dateTime' | 'boolean' | 'nameList' | 'secret';
    /** The provider refuses a write without it, so the form should too. */
    required?: boolean;
    /**
     * The provider decides it; showing it is useful, offering to type it is a
     * lie. Orthogonal to `kind`, because a read-only field still has a type —
     * declaring a number as a string is how a form starts refusing to save
     * with no message at all.
     */
    readonly?: boolean;
  }>;
  mapping: Omit<ResourceMapping, 'table'>;
}

/**
 * Build the Users resource for a stand.
 *
 * Deliberately identical across stands: same registry name, same route, same
 * columns in the same order. An operator moving between deployments should be
 * looking at the same screen, and the differences should live in the
 * declaration rather than in what they have to learn.
 */
export function usersResource(users: StandUsers, group?: string): StandResource {
  const { nameField, emailField, identityField, extraFields = [] } = users;
  return {
    name: 'users',
    mapping: { ...users.mapping, table: users.table },
    definition: defineResource('users', {
      basePath: '/users',
      // Present only for a platform stand, where Users is one of the identity
      // provider's screens rather than the deployment's own people.
      ...(group ? { group } : {}),
      label: { single: 'User', plural: 'Users' },
      displayField: nameField,
      fields: (f) => {
        const fields: Record<string, unknown> = {
          id: f.string({ system: true, label: 'ID' }),
          [nameField]: f.string({ label: 'Name' }),
        };
        // An email, declared as one: the form then refuses `dawdaw` here
        // rather than at the provider, which answers such a thing with its own
        // validator's Debug output — true, and unreadable.
        if (emailField) fields[emailField] = f.email({ label: 'Email', required: true });
        if (identityField && identityField !== 'id') {
          fields[identityField] = f.string({ system: true, label: 'Identity' });
        }
        for (const extra of extraFields) {
          // A field the provider decides is shown and never sent: `system`
          // keeps it out of the way, and `write: 'never'` keeps it out of the
          // form's own validation — an empty number input is not a number, and
          // a create form full of fields nobody types is how a save fails with
          // no message against anything.
          const system = extra.readonly ?? false;
          const readOnly = system ? ({ write: 'never' } as const) : ({} as const);
          fields[extra.name] =
            extra.kind === 'dateTime'
              ? f.dateTime({ system: true, label: extra.label })
              : extra.kind === 'number'
                ? f.number({ system, ...readOnly, label: extra.label })
                : extra.kind === 'boolean'
                  ? f.boolean({ system, ...readOnly, label: extra.label })
                  : extra.kind === 'nameList'
                    ? // A list of names, edited as one comma-separated line and
                      // sent as a list — see `ResourceMapping.listFields`.
                      f.string({ system, label: extra.label })
                    : extra.kind === 'secret'
                      ? // Never read back, only written. Left blank it changes
                        // nothing.
                        f.string({ label: extra.label })
                      : f.string({
                          system,
                          ...readOnly,
                          label: extra.label,
                          required: extra.required ?? false,
                        });
        }
        return fields as never;
      },
      views: {
        list: {
          columns: [
            nameField,
            ...(emailField ? [emailField] : []),
            ...extraFields.map((extra) => extra.name),
          ],
          requiredColumns: [nameField],
        },
      },
    }),
  };
}
