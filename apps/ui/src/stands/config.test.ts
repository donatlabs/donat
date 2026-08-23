import { describe, expect, it, vi } from 'vitest';
import { standFromConfig, standsFromEnv } from './config';

const DEFAULTS = { graphqlUrl: '/v1/graphql', role: 'admin' };

describe('stands from configuration', () => {
  it('gives an unconfigured panel one stand at its own origin', () => {
    const [stand] = standsFromEnv(undefined, DEFAULTS);
    expect(stand.graphqlUrl).toBe('/v1/graphql');
    expect(stand.role).toBe('admin');
    // One resource, and it is the platform's question: who can get in.
    // The platform's screens: the people, and the three things the provider
    // decides about them, and the applications that may ask.
    expect(stand.resources.map((r) => r.name)).toEqual([
      'users',
      'roles',
      'groups',
      'scopes',
      'clients',
      'attributes',
      'blocked-ips',
      'sessions',
    ]);
  });

  it('takes whatever the deployment calls its operator role', () => {
    // `admin`, `support`, `operator` — the name is a per-deployment fact, and
    // asserting it grants nothing: the deployment's own permissions decide.
    for (const role of ['support', 'operator', 'back_office']) {
      const stand = standFromConfig({ role }, DEFAULTS);
      expect(stand.role).toBe(role);
      expect(stand.label).toBe(role);
    }
  });

  it('serves several deployments from one panel', () => {
    const stands = standsFromEnv(
      JSON.stringify([
        { id: 'eu', label: 'EU', graphqlUrl: 'https://eu.example/v1/graphql', role: 'support' },
        { id: 'us', label: 'US', graphqlUrl: 'https://us.example/v1/graphql', role: 'admin' },
      ]),
      DEFAULTS,
    );
    expect(stands.map((s) => s.id)).toEqual(['eu', 'us']);
    expect(stands.map((s) => s.role)).toEqual(['support', 'admin']);
  });

  it('reads a deployment that spells its people differently', () => {
    const [stand] = standsFromEnv(
      JSON.stringify([
        {
          role: 'operator',
          users: {
            table: 'account',
            nameField: 'full_name',
            emailField: 'login',
            identityField: 'external_id',
            extraFields: [{ name: 'created_at', label: 'Joined', kind: 'dateTime' }],
            mapping: { updatableFields: ['full_name'], pkType: 'uuid' },
          },
        },
      ]),
      DEFAULTS,
    );
    const users = stand.resources[0];
    expect(users.mapping.table).toBe('account');
    // The selection is derived from what the screen shows, so a column the
    // role cannot read is never asked for by accident.
    expect(users.mapping.selectFields).toEqual([
      'id',
      'full_name',
      'login',
      'external_id',
      'created_at',
    ]);
    expect(users.mapping.updatableFields).toEqual(['full_name']);
    expect(users.mapping.pkType).toBe('uuid');
  });

  it('manages the identity provider\'s accounts when a stand declares no people of its own', () => {
    const [stand] = standsFromEnv(JSON.stringify([{ role: 'admin' }]), DEFAULTS);
    const mapping = stand.resources[0].mapping;
    // The fields the engine itself serves for a configured provider — not a
    // guess at a `users` table, which is what this used to be.
    expect(mapping.fields?.list).toBe('idp_users');
    expect(mapping.fields?.update).toBe('idp_user_update');
    expect(mapping.updatableFields).toEqual([
      'email',
      'given_name',
      'family_name',
      'roles',
      'enabled',
      'email_verified',
      // Written, never read: the provider treats an absent password as "leave
      // it alone", which is what an empty field in the form means.
      'password',
    ]);
    expect(mapping.selectFields).not.toContain('password');
  });

  it('leaves the identity screens out of a stand that has its own people', () => {
    const [stand] = standsFromEnv(
      JSON.stringify([{ role: 'admin', users: { table: 'customer', nameField: 'name' } }]),
      DEFAULTS,
    );
    // Roles, groups, scopes and applications are fields the engine serves for
    // a configured identity provider. A deployment whose people are rows has
    // not said it has one, and screens pointing at absent fields would fail on
    // opening rather than on configuration.
    expect(stand.resources.map((r) => r.name)).toEqual(['users']);
  });

  it('writes nothing unless a declared stand said which columns it may write', () => {
    const [stand] = standsFromEnv(
      JSON.stringify([{ role: 'admin', users: { table: 'customer', nameField: 'name' } }]),
      DEFAULTS,
    );
    // A deployment whose people are rows describes them from scratch, and
    // says nothing about writing until it means to.
    expect(stand.resources[0].mapping.table).toBe('customer');
    expect(stand.resources[0].mapping.fields).toBeUndefined();
    expect(stand.resources[0].mapping.updatableFields).toEqual([]);
  });

  it('says so and carries on when the configuration cannot be read', () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => {});
    const stands = standsFromEnv('{not json', DEFAULTS);
    expect(stands).toHaveLength(1);
    expect(stands[0].role).toBe('admin');
    expect(error).toHaveBeenCalled();
    error.mockRestore();
  });

  it('accepts a single object as well as an array', () => {
    const stands = standsFromEnv(JSON.stringify({ role: 'support' }), DEFAULTS);
    expect(stands.map((s) => s.role)).toEqual(['support']);
  });

  it('leaves the notification screens out until a stand says it has them', () => {
    const stand = standFromConfig({ users: { table: 'customer' } }, DEFAULTS);
    expect(stand.resources.map((r) => r.name)).not.toContain('notification_inbox');
    expect(stand.groups ?? []).toHaveLength(0);
  });

  it('adds the inbox and its preferences to a stand that adopted the module', () => {
    const stand = standFromConfig(
      { users: { table: 'customer' }, notifications: true },
      DEFAULTS,
    );
    expect(stand.resources.map((r) => r.name)).toEqual([
      'users',
      'notification_inbox',
      'notification_preference',
    ]);
    const inbox = stand.resources.find((r) => r.name === 'notification_inbox')!;
    // The unread count is a permission fact: the module grants
    // `allow_aggregations` on the feed and not on the preferences.
    expect(inbox.mapping.aggregate).toBe('notification_inbox_aggregate');
    // `seen` is generated in the database, so it is readable and never
    // writable — sending it would fail GraphQL validation, not permissions.
    expect(inbox.mapping.selectFields).toContain('seen');
    expect(inbox.mapping.updatableFields).toEqual(['seen_at', 'read_at', 'archived_at']);
    const preferences = stand.resources.find((r) => r.name === 'notification_preference')!;
    expect(preferences.mapping.aggregate).toBe(false);
    // `recipient_id` is an insert preset in the module, so it is not a field a
    // form may offer.
    expect(preferences.mapping.updatableFields).toEqual(['enabled']);
  });
});
