---
name: donat-platform-ui
description: Use when someone needs screens over a donat application - the platform its users and operators work in. Build it as TypeScript resource configs, not components; the config is derived from the metadata you already wrote.
---

# The platform is a config, not a codebase

A frontend request is not an exception to `declaring-not-coding`. The same
discipline extends past the API boundary: the platform is a set of **resource
definitions in TypeScript**, and the pages, routes, navigation, tables, forms
and filters are generated from them.

You do not write a list page. You declare a resource that has a list.

## Call it the platform

Not "the admin panel", not "the back office". What you are building is the
place the business runs — a client manages their own subscription there, an
operator looks at every client, someone in finance reconciles. Those are all
*actions within the platform*, told apart by role, not by which tool they open.

Two reasons this wording is load-bearing, not decoration:

- **"Admin panel" sounds like a side tool** bolted on for internal use, and it
  invites a design where the real product is elsewhere and this thing gets a
  privileged shortcut. It does not get one.
- **"Admin" is already taken, and means the opposite.** There is no admin role
  in donat. A reader who hears "admin panel" and "no admin role" in the same
  session has to work out that these are unrelated. Say *platform*, and the
  collision disappears.

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
  { name: 'my-platform', plugins: [fieldTypesPlugin(), fileStoragePlugin({ /* … */ })] },
  (setup) => {
    setup.use('app.groups', contentGroup, peopleGroup);
    setup.use('app.01-venue', venueResource);
    setup.use('app.02-invite', inviteResource);
  },
);
```

That is the whole surface for a screen. Reference implementation:
`apps/ui/src/` of a Refinest app — `resources/*.ts`, `refinest-app.ts`,
`data/resource-mappings.ts`, and four generic `pages/resources/*.tsx` shared by
every resource.

## Derive the config from the metadata you already wrote

This is the part that makes it cheap and keeps the two halves honest. Every
line of the platform config has a source in the donat metadata — do not invent it
from the database, and do not invent it from the screen.

| donat metadata | Platform config |
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
platform does not filter by owner; the server does, because the request runs as
a role. If you find yourself adding a `where` to the config to hide other
people's rows, the permission is missing and you are papering over it.

## The platform never re-implements permissions

It runs as a declared role and sees exactly what that role sees. Everything
else follows:

- **Hiding a button is UX, not security.** Do it for tidiness; never rely on it.
- **No client-side check ever decides access.** If the UI can do it, the API
  allows it, and if the API allows it the UI hiding it changes nothing.
- **If a screen needs data the role cannot read, fix the role** — deliberately,
  with the reason written down — or accept that the screen cannot exist.
- **Do not add a service account for the platform.** There is no admin role in
  donat. The people who run the business are ordinary roles — `operator`,
  `support`, `billing` — each with an explicit list of what it may read and
  write. One platform, several roles, no privileged one.

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

## Asking a non-technical partner about the platform

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

> Venues now have their own section in the platform, under Content. The list
> shows the name, the address, and whether check-in happens there. Open one and
> you get the full form; the event it belongs to is filled in automatically and
> can't be edited by hand.

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
apps/ui/src/
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

## Serving it

Build it, and point the engine at the output:

```
DONAT_UI_DIR=/usr/share/donat/ui
```

The engine serves those files as a router fallback — after every one of its own
paths, never in front of one — so one container and one process carry the API
and the UI. Unset or empty, it serves none, which is what a deployment putting
the UI on a CDN would do.

Reach for that before reaching for a reverse proxy, because the reason is not
tidiness. Requests the UI makes are relative (`/v1/graphql`, `/auth/v1`), the
engine's session cookie only comes back to the origin that set it, and an
identity provider behind `/auth/v1` compares `Origin` against its own public
URL and sets a `__Host-`-prefixed cookie. All three want one origin. Served by
the engine, there is nothing to configure and so nothing to configure wrongly;
served separately, a `DONAT_UPSTREAM` pointing at the wrong place is a login
that refuses everything without saying why.

One thing to know before publishing an image: Vite inlines `VITE_*` at build
time, so the role the UI asserts is baked in. An image meant for more than one
deployment either takes it as a build argument or reads it from
`/auth/session`, which already reports the roles the caller's token granted.

For the donat side of the wiring — roles, headers and what the provider talks
to — see
[`examples/petshop-rest`](https://github.com/donatlabs/donat/tree/main/examples/petshop-rest),
the smallest complete permission-checked API in the repository.
