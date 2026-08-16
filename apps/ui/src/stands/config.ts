import type { ResourceMapping } from '../data/donat-data-provider';
import { IDENTITY_GROUP, identityGroup, identityResources } from './identity';
import { usersResource, type Stand, type StandUsers } from './types';

/**
 * Stands, from configuration.
 *
 * The panel is not written against any one deployment. What it needs to know
 * is where an engine serves GraphQL, **which role to run as**, and how that
 * deployment spells its people — all three are configuration, because the
 * answers differ per deployment and none of them belongs in this repository.
 *
 * The role is the important one. Every deployment calls its operator role
 * something: `admin`, `support`, `staff`, `operator`. Whatever it is, it is an
 * ordinary role that deployment's own metadata grants permissions to — this
 * engine has no admin role, so naming one here grants nothing at all. The
 * panel simply asserts the name it was configured with, and the engine answers
 * with exactly that role's permissions.
 */

/** One stand as it is written in `VITE_DONAT_STANDS`. */
export interface StandConfig {
  id?: string;
  label?: string;
  /** Defaults to `VITE_DONAT_GRAPHQL_URL`, i.e. this panel's own origin. */
  graphqlUrl?: string;
  /** Defaults to `VITE_DONAT_ROLE`. */
  role?: string;
  /**
   * How to reach this deployment's people. Omit it and the stand manages the
   * identity provider's accounts, which is what the engine serves by default;
   * a deployment whose people are rows in its own database says so here.
   */
  users?: Partial<StandUsers> & { mapping?: Partial<ResourceMapping> };
}

/**
 * The shape assumed when a stand declares no `users` block.
 *
 * Not a guess any more: these are the fields the **engine** serves when a
 * deployment names the role allowed to administer its identity provider
 * (`DONAT_OIDC.admin_role`, `crates/server/src/idp_admin.yaml`). The people who
 * can sign in are accounts in that provider rather than rows in a database, so
 * this is what a platform's Users screen is by default, and a deployment
 * configures nothing here to get it.
 *
 * A deployment whose people *are* rows says so — `users: { table: 'customer',
 * … }` — and that declaration replaces all of this, `fields` included.
 */
const CONVENTIONAL_USERS: StandUsers = {
  table: 'idp_users',
  nameField: 'email',
  emailField: 'email',
  identityField: 'id',
  extraFields: [
    // The provider will not create an account without one, so neither will
    // this form — the alternative is its validator's Debug output as an error
    // message.
    { name: 'given_name', label: 'First name', required: true },
    { name: 'family_name', label: 'Last name' },
    { name: 'roles', label: 'Roles (comma separated)', kind: 'nameList' },
    { name: 'enabled', label: 'Enabled', kind: 'boolean' },
    { name: 'email_verified', label: 'Email verified', kind: 'boolean' },
    { name: 'language', label: 'Language' },
    { name: 'account_type', label: 'Account type', readonly: true },
    // A number, and declared as one. The provider sends unix seconds; calling
    // it a string makes the whole form fail to save with no message against
    // any field — which is exactly what it did.
    { name: 'last_login', label: 'Last login (unix seconds)', kind: 'number', readonly: true },
    // Absent leaves the password alone; the provider treats a null as "no
    // change" rather than as an empty password.
    { name: 'password', label: 'Set a new password', kind: 'secret' },
  ],

  mapping: {
    // Root fields rather than table roots: an identity provider's accounts
    // are reached through actions, which is what makes the provider
    // replaceable without the panel knowing.
    fields: {
      list: 'idp_users',
      one: 'idp_user',
      create: 'idp_user_create',
      update: 'idp_user_update',
      delete: 'idp_user_delete',
      args: { id: 'id', input: 'input' },
      // An update replaces the whole record and demands every field of it; a
      // create is what somebody types. Two inputs, because they are two
      // different things.
      inputType: 'IdpUserInput!',
      createInputType: 'IdpUserCreateInput!',
    },
    selectFields: [
      'id',
      'email',
      'given_name',
      'family_name',
      'roles',
      'enabled',
      'email_verified',
      'language',
      'account_type',
      'last_login',
    ],
    // Carried by the engine as a list and edited here as one line; the
    // provider replaces the whole set on a write either way.
    listFields: ['roles'],
    // `password` is not selected — it is never returned — but it is written.
    updatableFields: [
      'email',
      'given_name',
      'family_name',
      'roles',
      'enabled',
      'email_verified',
      'password',
    ],
    pkType: 'String',
    pkField: 'id',
    aggregate: false,
    orderByField: 'id',
  },
};

function usersFrom(config: StandConfig['users']): StandUsers {
  if (!config) return CONVENTIONAL_USERS;
  // A declared stand describes its own people from scratch. Falling back to
  // the identity provider's field names here would send a table deployment's
  // queries to actions that do not exist, and the failure would look like a
  // permission problem rather than a configuration one.
  const nameField = config.nameField ?? 'name';
  const emailField = config.emailField ?? 'email';
  const mapping: Partial<ResourceMapping> = config.mapping ?? {};
  return {
    table: config.table ?? 'users',
    nameField,
    emailField,
    identityField: config.identityField,
    extraFields: config.extraFields,
    mapping: {
      // Selecting a column the role cannot read fails the whole document, so
      // the default is the smallest set the screen needs.
      selectFields: mapping.selectFields ?? [
        'id',
        nameField,
        ...(emailField ? [emailField] : []),
        ...(config.identityField ? [config.identityField] : []),
        ...(config.extraFields ?? []).map((extra) => extra.name),
      ],
      // Absent means "write nothing": a panel that guesses at writable
      // columns discovers it was wrong by failing an operator's save.
      updatableFields: mapping.updatableFields ?? [],
      aggregate: mapping.aggregate ?? false,
      orderByField: mapping.orderByField ?? 'id',
      pkType: mapping.pkType,
      pkField: mapping.pkField,
      byPkRoot: mapping.byPkRoot,
      fixedFilter: mapping.fixedFilter,
      fields: mapping.fields,
    },
  };
}

/** Turn one configured entry into a stand. */
export function standFromConfig(config: StandConfig, defaults: Required<Pick<StandConfig, 'graphqlUrl' | 'role'>>): Stand {
  const role = config.role ?? defaults.role;
  const graphqlUrl = config.graphqlUrl ?? defaults.graphqlUrl;
  const id = config.id ?? `${role}@${graphqlUrl}`;
  return {
    id,
    label: config.label ?? role,
    graphqlUrl,
    role,
    // A stand that declares its own people is an application's, and the
    // identity screens would be pointing at fields its engine does not serve.
    // One that declares none is the platform's, and gets all of them.
    // The section exists only where its screens do: a group with no members
    // is an empty heading in the sidebar.
    groups: config.users ? undefined : [identityGroup()],
    resources: config.users
      ? [usersResource(usersFrom(config.users))]
      : [usersResource(usersFrom(undefined), IDENTITY_GROUP), ...identityResources()],
  };
}

/**
 * Read `VITE_DONAT_STANDS`. Anything unparseable is reported and ignored: a
 * panel that silently falls back to one stand when it was configured with four
 * is worse than one that says so in the console and shows the default.
 */
export function standsFromEnv(
  raw: string | undefined,
  defaults: Required<Pick<StandConfig, 'graphqlUrl' | 'role'>>,
): Stand[] {
  if (!raw || raw.trim() === '') return [standFromConfig({}, defaults)];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    console.error('[donat-admin] VITE_DONAT_STANDS is not valid JSON; using the default stand', error);
    return [standFromConfig({}, defaults)];
  }
  const entries = Array.isArray(parsed) ? parsed : [parsed];
  const stands = entries
    .filter((entry): entry is StandConfig => entry !== null && typeof entry === 'object')
    .map((entry) => standFromConfig(entry, defaults));
  return stands.length > 0 ? stands : [standFromConfig({}, defaults)];
}
