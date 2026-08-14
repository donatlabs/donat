import { defineGroup, type NavGroupDef } from '@refinest/core';
import { defineResource } from '@refinest/field-types';
import type { StandResource } from './types';

/**
 * The settings a platform has when its identity provider is the one the engine
 * knows.
 *
 * These are not this panel's screens invented from scratch: each one renders a
 * field the **engine** serves — `idp_roles`, `idp_groups`, `idp_scopes`,
 * `idp_clients` — which exist when a deployment configures
 * `DONAT_OIDC.admin_key` and are visible only to the role it names
 * (`knowledgebase/platform/decisions/003-*`). Configure no key and the engine
 * has no such fields; the screens then fail loudly at the engine rather than
 * pretending, which is the same bargain every resource here makes.
 *
 * They are the questions an identity provider answers: who the people are
 * (Users, declared beside them in `types.ts`), what a token may say about them
 * (Roles), how they are grouped (Groups), what else may be said about them
 * (Attributes), what an application may ask for (Scopes), which applications
 * may ask at all (Applications), who is refused at the door (Blocked IPs) and
 * who is signed in right now (Sessions).
 *
 * What is absent is what an API key cannot reach or should not carry. The
 * provider's own password policy, terms and key-value store answer only to an
 * *admin session*, and the engine holds a key rather than someone's password —
 * that is a boundary, not a gap. Its API keys refuse to be managed by another
 * API key, which is the provider's own rule and a good one. Its event stream
 * is a stream rather than a resource. Those stay in the provider's interface.
 *
 * All five sit under one collapsible sidebar section, Users included: they are
 * one subject — the identity provider — and a deployment that adds its own
 * application's resources should see its screens beside this section rather
 * than mixed into it.
 */

/**
 * The section they live in.
 *
 * The name is not any resource's: a group and a resource sharing one name
 * makes `hrefFor` resolve a node to itself, and the sidebar recurses until the
 * stack ends. It also contributes no URL segment (`path` absent), so a user is
 * still at `/users` rather than `/identity/users` — the grouping is
 * navigation, not addressing, and existing links keep working.
 */
export const IDENTITY_GROUP = 'identity';

export function identityGroup(): NavGroupDef {
  return defineGroup({
    name: IDENTITY_GROUP,
    label: { single: 'Identity', plural: 'Identity' },
    layout: 'flat',
    collapsible: true,
    defaultOpen: true,
  });
}

/** Roles, groups and scopes are the same screen three times: one name. */
function namedResource(options: {
  name: string;
  basePath: string;
  single: string;
  plural: string;
  fields: { list: string; create: string; update: string; delete: string };
}): StandResource {
  return {
    name: options.name,
    mapping: {
      table: options.fields.list,
      fields: {
        list: options.fields.list,
        create: options.fields.create,
        update: options.fields.update,
        delete: options.fields.delete,
        args: { id: 'id', input: 'input' },
        inputType: 'IdpNameInput!',
        // A deployment's roles, groups and scopes are a page of names; the
        // provider offers no field for one of them, so the record page reads
        // the collection.
        oneFromList: true,
      },
      selectFields: ['id', 'name'],
      updatableFields: ['name'],
      pkType: 'String',
      pkField: 'id',
      aggregate: false,
      orderByField: 'name',
    },
    definition: defineResource(options.name, {
      basePath: options.basePath,
      group: IDENTITY_GROUP,
      label: { single: options.single, plural: options.plural },
      displayField: 'name',
      fields: (f) => {
        const fields: Record<string, unknown> = {
          id: f.string({ system: true, label: 'ID' }),
          name: f.string({ label: 'Name', required: true }),
        };
        return fields as never;
      },
      views: {
        list: { columns: ['name'], requiredColumns: ['name'] },
      },
    }),
  };
}

function clientsResource(): StandResource {
  return {
    name: 'clients',
    mapping: {
      table: 'idp_clients',
      fields: {
        list: 'idp_clients',
        one: 'idp_client',
        create: 'idp_client_create',
        update: 'idp_client_update',
        delete: 'idp_client_delete',
        args: { id: 'id', input: 'input' },
        inputType: 'IdpClientInput!',
        // Registering one and changing one take different shapes: the
        // provider assigns no id, so a registration carries the caller's, and
        // the rest of the record gets its defaults and is edited afterwards.
        createInputType: 'IdpClientCreateInput!',
      },
      selectFields: [
        'id',
        'name',
        'enabled',
        'confidential',
        'redirect_uris',
        'allowed_origins',
        'flows_enabled',
        'scopes',
        'default_scopes',
        'force_mfa',
        'client_uri',
      ],
      // Everything the provider replaces on a write. A client's secret is not
      // among them and is never sent here.
      updatableFields: [
        'name',
        'enabled',
        'confidential',
        'redirect_uris',
        'allowed_origins',
        'flows_enabled',
        'scopes',
        'default_scopes',
        'force_mfa',
        'client_uri',
      ],
      pkType: 'String',
      pkField: 'id',
      aggregate: false,
      orderByField: 'id',
    },
    definition: defineResource('clients', {
      basePath: '/clients',
      group: IDENTITY_GROUP,
      label: { single: 'Application', plural: 'Applications' },
      displayField: 'name',
      fields: (f) => {
        const fields: Record<string, unknown> = {
          // Not `system`: a registration chooses it, and it is what the
          // application will present as its `client_id`. It cannot be changed
          // afterwards, which the write rule below states.
          id: f.string({ label: 'Client ID', required: true, write: 'create' }),
          name: f.string({ label: 'Name' }),
          // `write: 'update'` on everything the provider does not accept when
          // registering one. Not a preference: these are absent from
          // `IdpClientCreateInput`, and a form offering them would be a form
          // whose answers the engine rejects.
          enabled: f.boolean({ label: 'Enabled', write: 'update' }),
          confidential: f.boolean({ label: 'Confidential' }),
          force_mfa: f.boolean({ label: 'Require a second factor', write: 'update' }),
          client_uri: f.url({ label: 'Application URL', write: 'update' }),
          // Lists of strings the provider replaces wholesale. `json` rather
          // than a relation: they are addresses and flow names, not rows in
          // anything this panel can offer to pick from.
          redirect_uris: f.json({ label: 'Redirect URIs' }),
          allowed_origins: f.json({ label: 'Allowed origins', write: 'update' }),
          flows_enabled: f.json({ label: 'Flows', write: 'update' }),
          scopes: f.json({ label: 'Scopes', write: 'update' }),
          default_scopes: f.json({ label: 'Default scopes', write: 'update' }),
        };
        return fields as never;
      },
      views: {
        list: {
          columns: ['name', 'id', 'enabled', 'force_mfa'],
          requiredColumns: ['name'],
        },
      },
    }),
  };
}


/**
 * Custom claims. The name is the key the provider stores under, so it is the
 * primary key here too — renaming one is deleting it and making another, and
 * the form says so by leaving the name alone once it exists.
 */
function attributesResource(): StandResource {
  return {
    name: 'attributes',
    mapping: {
      table: 'idp_user_attributes',
      fields: {
        list: 'idp_user_attributes',
        create: 'idp_user_attribute_create',
        update: 'idp_user_attribute_update',
        delete: 'idp_user_attribute_delete',
        args: { id: 'id', input: 'input' },
        inputType: 'IdpUserAttributeInput!',
        oneFromList: true,
      },
      selectFields: ['name', 'desc', 'user_editable'],
      updatableFields: ['desc'],
      pkType: 'String',
      pkField: 'name',
      aggregate: false,
      orderByField: 'name',
    },
    definition: defineResource('attributes', {
      basePath: '/attributes',
      group: IDENTITY_GROUP,
      label: { single: 'Attribute', plural: 'Attributes' },
      displayField: 'name',
      fields: (f) => {
        const fields: Record<string, unknown> = {
          name: f.string({ label: 'Name' }),
          desc: f.string({ label: 'Description' }),
          user_editable: f.boolean({ system: true, label: 'Editable by the person' }),
        };
        return fields as never;
      },
      views: { list: { columns: ['name', 'desc'], requiredColumns: ['name'] } },
    }),
  };
}

/**
 * The addresses the provider is refusing. Operational rather than decorative:
 * this is the screen someone opens during an incident, which is why it can be
 * added to and cleared but not edited — an entry is one decision, and changing
 * it means making a different one.
 */
function blockedIpsResource(): StandResource {
  return {
    name: 'blocked-ips',
    mapping: {
      table: 'idp_blocked_ips',
      fields: {
        list: 'idp_blocked_ips',
        create: 'idp_blocked_ip_create',
        delete: 'idp_blocked_ip_delete',
        args: { id: 'id', input: 'input' },
        inputType: 'IdpBlockedIpInput!',
        oneFromList: true,
      },
      selectFields: ['ip', 'exp'],
      updatableFields: [],
      pkType: 'String',
      pkField: 'ip',
      aggregate: false,
      orderByField: 'ip',
    },
    definition: defineResource('blocked-ips', {
      basePath: '/blocked-ips',
      group: IDENTITY_GROUP,
      label: { single: 'Blocked address', plural: 'Blocked addresses' },
      displayField: 'ip',
      fields: (f) => {
        const fields: Record<string, unknown> = {
          ip: f.string({ label: 'Address' }),
          // Unix seconds, as the provider speaks them. Rendered as a number
          // rather than a date because that is what has to be typed back.
          exp: f.number({ label: 'Blocked until (unix seconds)' }),
        };
        return fields as never;
      },
      views: { list: { columns: ['ip', 'exp'], requiredColumns: ['ip'] } },
    }),
  };
}

/**
 * Who is signed in. Read and end, and nothing else: there is nothing to edit
 * about a session, and ending one is the only thing an operator ever wants
 * from this screen.
 */
function sessionsResource(): StandResource {
  return {
    name: 'sessions',
    mapping: {
      table: 'idp_sessions',
      fields: {
        list: 'idp_sessions',
        delete: 'idp_session_delete',
        args: { id: 'id' },
      },
      selectFields: ['id', 'user_id', 'is_mfa', 'state', 'exp', 'last_seen', 'remote_ip'],
      updatableFields: [],
      pkType: 'String',
      pkField: 'id',
      aggregate: false,
      orderByField: 'id',
    },
    definition: defineResource('sessions', {
      basePath: '/sessions',
      group: IDENTITY_GROUP,
      label: { single: 'Session', plural: 'Sessions' },
      // A list and an end, and nothing else. There is no field for one session
      // and there can be a great many of them, so a record page would read
      // every session to show one — and there is nothing on it to read anyway.
      operations: { show: false, create: false, edit: false },
      displayField: 'id',
      fields: (f) => {
        const fields: Record<string, unknown> = {
          id: f.string({ system: true, label: 'Session' }),
          user_id: f.string({ label: 'Person' }),
          state: f.string({ label: 'State' }),
          is_mfa: f.boolean({ label: 'Second factor' }),
          remote_ip: f.string({ label: 'Address' }),
          last_seen: f.number({ label: 'Last seen (unix seconds)' }),
          exp: f.number({ label: 'Expires (unix seconds)' }),
        };
        return fields as never;
      },
      views: {
        list: {
          columns: ['user_id', 'state', 'is_mfa', 'remote_ip', 'last_seen'],
          requiredColumns: ['user_id'],
        },
      },
    }),
  };
}

/**
 * The settings screens, in the order an operator meets them.
 *
 * Users is not here: it is the one screen every stand has, identity provider
 * or not, and it is built beside the stand's own declaration — with this
 * group's name, so it joins the section.
 */
export function identityResources(): StandResource[] {
  return [
    namedResource({
      name: 'roles',
      basePath: '/roles',
      single: 'Role',
      plural: 'Roles',
      fields: {
        list: 'idp_roles',
        create: 'idp_role_create',
        update: 'idp_role_update',
        delete: 'idp_role_delete',
      },
    }),
    namedResource({
      name: 'groups',
      basePath: '/groups',
      single: 'Group',
      plural: 'Groups',
      fields: {
        list: 'idp_groups',
        create: 'idp_group_create',
        update: 'idp_group_update',
        delete: 'idp_group_delete',
      },
    }),
    namedResource({
      name: 'scopes',
      basePath: '/scopes',
      single: 'Scope',
      plural: 'Scopes',
      fields: {
        list: 'idp_scopes',
        create: 'idp_scope_create',
        update: 'idp_scope_update',
        delete: 'idp_scope_delete',
      },
    }),
    clientsResource(),
    attributesResource(),
    blockedIpsResource(),
    sessionsResource(),
  ];
}
