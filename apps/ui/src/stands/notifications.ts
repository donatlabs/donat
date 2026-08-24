import { defineGroup, type NavGroupDef } from '@refinest/core';
import { defineResource } from '@refinest/field-types';
import type { StandResource } from './types';

/**
 * The two screens `modules/notifications` earns a panel.
 *
 * They render what the *engine* serves under the `notification_user` role —
 * `notification_inbox` and `notification_preference` — so a stand only gets
 * them when its role inherits that one. Nothing here is a second permission
 * system: every column named below is a column the module's
 * `select_permissions` already grants, and every writable one is in its
 * `update_permissions`. That duplication is deliberate and is the same bargain
 * every resource in this panel makes — introspecting permissions to avoid it
 * would rebuild the admin API this engine deleted
 * (`knowledgebase/platform/decisions/001-*`).
 *
 * What is absent is a Delivery screen. `notification.delivery` is the
 * operational log — who was sent what and what came back — and no role reads it
 * over the API today: the module grants it only to a command. Publishing it is
 * a decision about who may see other people's mail, and it belongs to a
 * deployment rather than to this file.
 */

/**
 * The section they live in.
 *
 * The name is not any resource's: a group and a resource sharing one name makes
 * `hrefFor` resolve a node to itself and the sidebar recurses until the stack
 * ends.
 */
export const NOTIFICATIONS_GROUP = 'notifications-section';

export function notificationsGroup(): NavGroupDef {
  return defineGroup({
    name: NOTIFICATIONS_GROUP,
    label: { single: 'Notifications', plural: 'Notifications' },
    layout: 'flat',
    collapsible: true,
    defaultOpen: true,
  });
}

/**
 * The feed, as its owner sees it.
 *
 * Read-mostly: the only columns a recipient may write are the three that record
 * what they did with a notification, which is exactly the module's
 * `update_permissions` column list. `seen` is generated in the database, so it
 * is readable and never writable — naming it in `updatableFields` would send a
 * column `notification_inbox_set_input` does not have and fail the whole
 * document at GraphQL validation.
 */
function inboxResource(): StandResource {
  return {
    name: 'notification_inbox',
    mapping: {
      table: 'notification_inbox',
      selectFields: [
        'id',
        'title',
        'body',
        'url',
        'created_at',
        'seen_at',
        'seen',
        'read_at',
        'archived_at',
      ],
      updatableFields: ['seen_at', 'read_at', 'archived_at'],
      pkType: 'uuid',
      pkField: 'id',
      orderByField: 'created_at',
      // The module sets `allow_aggregations: true` on this permission, which
      // is what makes the unread count one round trip instead of a full fetch.
      aggregate: 'notification_inbox_aggregate',
    },
    definition: defineResource('notification_inbox', {
      basePath: 'notifications',
      group: NOTIFICATIONS_GROUP,
      label: { single: 'Notification', plural: 'Inbox' },
      displayField: 'title',
      fields: (f) =>
        ({
          id: f.string({ system: true, label: 'ID' }),
          title: f.string({ label: 'Title', write: 'never' }),
          body: f.string({ label: 'Body', write: 'never' }),
          url: f.string({ label: 'Link', write: 'never' }),
          created_at: f.dateTime({ label: 'Received', write: 'never' }),
          seen: f.boolean({ label: 'Seen', write: 'never' }),
          seen_at: f.dateTime({ label: 'Seen at' }),
          read_at: f.dateTime({ label: 'Read at' }),
          archived_at: f.dateTime({ label: 'Archived at' }),
        }) as never,
      views: {
        list: {
          columns: ['title', 'created_at', 'read_at'],
          requiredColumns: ['title'],
        },
      },
    }),
  };
}

/**
 * Opt-out, one row per workflow and channel.
 *
 * `recipient_id` is absent on purpose: the module makes it an insert preset, so
 * it is not in the role's insert column list and not in the input type at all.
 * A form that offered it would be offering a field the engine refuses.
 */
function preferenceResource(): StandResource {
  return {
    name: 'notification_preference',
    mapping: {
      table: 'notification_preference',
      selectFields: ['recipient_id', 'workflow', 'channel', 'enabled', 'updated_at'],
      updatableFields: ['enabled'],
      // The table's key is (recipient_id, workflow, channel) rather than a
      // surrogate id, so there is no single-row root to read one by.
      pkField: 'workflow',
      pkType: 'String',
      byPkRoot: false,
      orderByField: 'workflow',
      // The module grants no `allow_aggregations` here: a count of one's own
      // opt-outs is not a question worth a permission.
      aggregate: false,
    },
    definition: defineResource('notification_preference', {
      basePath: 'notification-preferences',
      group: NOTIFICATIONS_GROUP,
      label: { single: 'Preference', plural: 'Preferences' },
      displayField: 'workflow',
      fields: (f) =>
        ({
          recipient_id: f.string({ system: true, label: 'Recipient' }),
          workflow: f.string({ label: 'Notification', required: true }),
          channel: f.string({ label: 'Channel', required: true }),
          enabled: f.boolean({ label: 'Enabled' }),
          updated_at: f.dateTime({ label: 'Updated', write: 'never' }),
        }) as never,
      views: {
        list: {
          columns: ['workflow', 'channel', 'enabled'],
          requiredColumns: ['workflow'],
        },
      },
    }),
  };
}

/** Both screens, for a stand whose role inherits `notification_user`. */
export function notificationResources(): StandResource[] {
  return [inboxResource(), preferenceResource()];
}
