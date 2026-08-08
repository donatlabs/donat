# 002 — Publish a JSON Schema for the metadata

**Written against:** `768f89c`
**Kind:** design spec, small enough to be nearly an implementation plan
**Effort:** M

## Why

The entire product surface is YAML. Tables, per-role permissions, validators,
commands, rules, durable processes, connectors, storage — all of it is
authored by hand in a directory of `.yaml` files, and the editor doing the
authoring knows nothing about any of it. There is no schema:
`schemars`, `JsonSchema` and `$schema` appear nowhere in the workspace.

The current feedback loop is `donat validate`, which is thorough but runs at
deploy time. Every misspelled key, every wrong nesting level and every
enum value that does not exist is found minutes after it was typed, by a
command, in a terminal — instead of underlined in place.

This is the cheapest large improvement available to the project, and it is
also the one a new user meets first.

## What exists to build on

`crates/metadata/src/types.rs` holds 215 public types, essentially all of them
`#[derive(Deserialize)]` over plain fields, many already carrying
`#[serde(deny_unknown_fields)]` — which is what makes a generated schema
strict rather than advisory.

Three types implement `Deserialize` by hand and will not derive a schema
mechanically. `Columns` (the `"*"`-or-list shape) is one of them. Each needs a
hand-written `impl JsonSchema` describing the union it accepts; there are only
three, and they are the interesting ones, so writing them by hand is correct
rather than a workaround.

## Shape of the work

1. Add `schemars` to `crates/metadata`, behind a non-default feature so the
   engine binary does not carry schema-generation code it never runs.
2. Derive `JsonSchema` alongside `Deserialize` on the metadata types; hand-write
   the three custom ones.
3. A small binary or `cargo xtask`-style target that writes the schema to a
   checked-in file — `metadata.schema.json` at the repo root, so editors can
   reference it by URL from the repository and a deployment can vendor it.
4. A test that regenerates the schema and fails if the checked-in copy differs.
   Without it the file rots the first time someone adds a field, and a stale
   schema is worse than none: it underlines correct metadata as wrong.
5. A line in the README and in `examples/petshop/README.md` showing the
   editor directive that binds YAML files to it (`# yaml-language-server:
   $schema=…`), because a schema nobody points at helps nobody.

## Trade-offs to decide

**One schema or several.** The metadata directory holds files of different
shapes — `databases/*.yaml`, `storage.yaml`, `version.yaml`, table files. One
schema per file kind gives precise completion; a single schema is simpler to
publish and to keep current. Per-file-kind is worth the extra structure here,
because the whole value is the editor knowing which keys are legal *in this
file*.

**Strictness.** `deny_unknown_fields` on the Rust side and `additionalProperties:
false` in the schema should agree. Where they disagree the schema is lying,
in one direction or the other. The regeneration test catches drift only if the
schema is generated from the same types the loader uses — which is the reason
to derive rather than hand-maintain.

**The `!include` extension.** The loader supports `!include`, a YAML tag JSON
Schema cannot express, and some fixtures include files as quoted strings. The
schema will flag valid metadata that uses it. This needs a decision: either
document the limitation, or have the editor directive apply only to files that
do not use includes. Do not discover this after publishing.

## Verification

- `cargo test -p donat-metadata` green, including the new regeneration test.
- The generated schema validates every metadata directory in the repository:
  `examples/petshop/metadata`, `examples/petshop-rest`, `examples/petshop-mcp`,
  and the conformance fixtures' metadata. If a real, working directory fails
  its own schema, the schema is wrong — that is the acceptance criterion, not
  a nice-to-have.
- The three hand-written `impl JsonSchema` each have a test asserting both
  accepted spellings validate.

## Escape hatch

If deriving `JsonSchema` across 215 types turns out to require restructuring
any of them, **stop**. The point is to describe the format that exists, not to
change the format so it can be described. Report which types resist and why.
