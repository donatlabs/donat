---
name: donat-admin-ui
description: Use when someone asks for an admin panel, back office or internal UI over a donat application. Build it as TypeScript resource configs, not components - the config is derived from the metadata you already wrote.
---

# The admin UI is a config, not a codebase

A frontend request is not an exception to `declaring-not-coding`. The same
discipline extends past the API boundary: an admin panel is a set of
**resource definitions in TypeScript**, and the pages, routes, navigation,
tables, forms and filters are generated from them.

You do not write a list page. You declare a resource that has a list.

## The shape

One config per resource, one file each, registered in an app definition.
Routes and navigation are generated from the registry — adding a resource adds
its screens and its sidebar entry, with no page code touched.

```ts
// resources/venue.ts
import { defineResource } from '@refinest/field-types';

export const venueResource = defineResource('venue', {
  basePath: '/venue',
  label: { single: 'Venue', plural: 'Venues' },
  displayField: 'name',
  group: 'content',
  fields: (f) => ({
    id:         f.string({ system: true, label: 'ID' }),
    event_id:   f.string({ system: true, label: 'Event' }),
    name:       f.string({ required: true, label: 'Name' }),
    about:      f.textarea({ label: 'About' }),
    image_file_id: f.file({ accept: 'image/*', label: 'Image' }),
    available_for_check_in: f.boolean({ label: 'Check-in venue' }),
    created_at: f.dateTime({ system: true, label: 'Created' }),
  }),
  views: {
    list: { columns: ['name', 'address', 'available_for_check_in'],
            requiredColumns: ['name'] },
  },
});
```

```ts
// refinest-app.ts
const definition = defineAppDefinition(
  { name: 'my-admin', plugins: [fieldTypesPlugin(), fileStoragePlugin({ /* … */ })] },
  (setup) => {
    setup.use('app.groups', contentGroup, peopleGroup);
    setup.use('app.01-venue', venueResource);
    setup.use('app.02-invite', inviteResource);
  },
);
```

That is the whole surface for a screen. Reference implementation:
`apps/admin/src/` of a Refinest app — `resources/*.ts`, `refinest-app.ts`,
`data/resource-mappings.ts`, and four generic `pages/resources/*.tsx` shared by
every resource.

## Derive the config from the metadata you already wrote

This is the part that makes it cheap and keeps the two halves honest. Every
line of the admin config has a source in the donat metadata — do not invent it
from the database, and do not invent it from the screen.

| donat metadata | Admin config |
|---|---|
| tracked table | `defineResource('<table>', …)` |
| the role's `select` `columns` mask | which fields exist at all |
| a column the role cannot write | `system: true` |
| a `set:` preset | `system: true`, seeded on create, never a user input |
| `validate` entry with `not_null` | `required: true` |
| the validator's `message` | the sentence the user sees on failure |
| an enum from `rules.yaml` | `f.enum(values, { meta: { options } })` |
| an `attachments` file column | `f.file({ accept, … })` + the file-storage plugin |
| an object relationship | a relation field pointing at the related resource |
| a row filter (`filter:`) | **nothing** — see below |
| a saved query / REST endpoint | not needed; the provider talks GraphQL |

The last row is the important one. **A row filter has no UI counterpart.** The
admin does not filter by owner; the server does, because the request runs as a
role. If you find yourself adding a `where` to the config to hide other
people's rows, the permission is missing and you are papering over it.

## The admin never re-implements permissions

It runs as a declared role and sees exactly what that role sees. Everything
else follows:

- **Hiding a button is UX, not security.** Do it for tidiness; never rely on it.
- **No client-side check ever decides access.** If the UI can do it, the API
  allows it, and if the API allows it the UI hiding it changes nothing.
- **If a screen needs data the role cannot read, fix the role** — deliberately,
  with the reason written down — or accept that the screen cannot exist.
- **Do not add a service account for the admin.** There is no admin role in
  donat. The back office is one or more ordinary roles: `staff`, `support`,
  `billing`.

A Hasura-shaped data provider works against donat unchanged: the engine accepts
`x-hasura-role` and `x-hasura-admin-secret` alongside its own headers
(`crates/server/src/gql.rs:275`, `:297`, `:228`), and falls back to the
`x-donat-` spellings. In production the role comes from the JWT, not a header.

## What you may write

| Allowed | |
|---|---|
| `resources/*.ts` | one declarative config per resource |
| the app definition | registration, groups, plugins |
| resource → table mappings | table name, primary key, select fields, fixed filters |
| enum helpers | `{ value, label }` lists mirroring the declared types |
| env and build config | endpoint, role, storage URL |
| the four generic resource pages | copied once from the reference app, then left alone |

Everything else is an escalation. In particular: a bespoke page component, a
second data path, a hand-written form, or any component that decides what a
user may see.

## When a component is genuinely unavoidable

It happens. The honest version looks like this — from the reference app, a bulk
"paste a list of emails" dialog that the declarative action surface could not
express, because the action data access exposes no `createMany` and the
per-row create path would not apply the insert-on-conflict the feature depends
on.

What made it acceptable was not that it was small. It was that:

1. every declarative route was tried first, and the reason each failed is
   written down in the file;
2. it reaches the **same** data runtime through a public, typed hook — it is
   "the provider reached another supported way", not a parallel data path;
3. it adds no permission logic of its own.

Hold yourself to those three. If a component fails any of them, escalate
instead — plain sentence for your partner, forwardable spec for the engineer,
per `declaring-not-coding`.

## Asking a non-technical partner about the UI

The screens follow from what you already asked about permissions. A few extra
questions, in their language:

> - When someone opens the list of venues, what do they need to see at a
>   glance? Three or four columns, not everything.
> - What do they search or filter by most?
> - Which fields should they fill in, and which should the system fill in for
>   them?
> - Should the sidebar be grouped? What would you call the groups?
> - Is anything here read-only for them, even though they can see it?

Note what is missing: nobody is asked what a screen should *look* like. Layout
is the framework's; content is the config's; access is the permission's.

Report back as a screen walkthrough, not a component tree:

> Venues now has its own entry under Content. The list shows name, address and
> whether it's a check-in point. Opening one gives you the full form; the event
> it belongs to is filled in automatically and can't be edited by hand.

## Checklist

1. Every resource config traces to a tracked table and one role's permissions.
2. No field exists that the role cannot read; no editable field it cannot write.
3. `system: true` on everything the server presets.
4. No `where` in the config standing in for a row filter.
5. No client-side access decision anywhere.
6. Enum options match the declared types — mirror them, and test that they do.
7. Files outside `resources/`, the app definition and mappings: named and
   justified, or escalated.

## Where to look

The framework is published on npm and MIT-licensed — `@refinest/core`,
`@refinest/field-types`, `@refinest/react`, `@refinest/ui-shadcn`,
`@refinest/file-storage`.

A reference app has this shape, and it is worth copying wholesale:

```
apps/admin/src/
  refinest-app.ts            defineAppDefinition + groups + setup.use(...)
  resources/<name>.ts        one declarative config per resource
  resources/enums.ts         { value, label } lists mirroring the declared types
  data/<provider>.ts         the Hasura-shaped data provider
  data/resource-mappings.ts  table, primary key, select fields, fixed filters
  pages/resources/*.tsx      four generic pages — list, show, create, edit
  app.tsx                    routes from generateRouteDescriptors(app)
```

Note the ratio: the four pages and `app.tsx` are written once and then left
alone; everything after that is `resources/*.ts`. If a change makes you edit a
page, ask first whether it belongs in the config.

For the donat side of the wiring — roles, headers and what the provider talks
to — see
[`examples/petshop-rest`](https://github.com/donatlabs/donat/tree/main/examples/petshop-rest),
the smallest complete permission-checked API in the repository.
