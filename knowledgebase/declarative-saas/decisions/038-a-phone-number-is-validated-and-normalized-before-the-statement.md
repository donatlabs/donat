---
type: decision
status: accepted
date: 2026-08-09
features:
  - "[[declarative-saas]]"
---

# A phone number is validated and normalized before the statement

## Context

Every validator so far (ADR-032) is a predicate the planner lowers to SQL and
the statement carries as a gate over the rows it wrote. That works because the
predicates are things a database can decide: a comparison, a length, a null.

A phone number is not such a thing. Whether `+49 1111 111111` is a real number
is a property of a versioned database of national numbering plans — the same
digits are a mobile number in one country, unassigned in another, and change
meaning when a regulator opens a range. No expression in the rule profile, no
`CHECK` constraint and no regex decides it. Applications therefore either store
whatever the caller typed, or push the question to the client, which means one
number arrives as `030 1234567`, `+49 30 1234567` and `+49 (0)30 123 4567` and
a unique index over the column stops meaning anything.

The two halves are inseparable: a value cannot be normalized without being
parsed, and once it has been parsed there is no reason to store the spelling
the caller happened to use.

## Decision

`validate` gains a third spelling, `phone: { column, region }`, and it is the
first one that is **not** lowered to SQL. It is compiled at deploy time like
the others — the column must exist and hold text, and the region must resolve
to a CLDR region code — and it is evaluated in the planner, over the value the
caller submitted, before the statement is built. A rejection is reported with
the entry's own `message` under `validation-failed`, the same shape and the
same path as every other validator, so the surface does not leak where the
check ran. An accepted value is **replaced** by its E.164 form, so the
statement carries the normalized literal and the column holds one spelling of
one number.

The region is deploy-time data and is enforced as such by a type: a
`PhoneRegion` can only be built by parsing a declared region code, so no
header, role, session variable or submitted column can produce one. A
declaration that tries to defer the choice to the request (`region:
X-Donat-Region`) fails to parse and refuses publication rather than resolving
from the request.

The engine keeps one statement per operation: the check costs one parse in
Rust, no round trip, and nothing in SQL. `phonenumber`'s embedded metadata
version is the build metadata of its crate version (`0.3.10+9.0.33`) and is
pinned exactly, because bumping it changes which numbers are valid.

## Alternatives

| Option | Why Not |
| --- | --- |
| A `CHECK` constraint with a regex | Decides syntax, never validity; binds every writer; answers with PostgreSQL text under `permission-error`. |
| A rule-profile expression over the column | The profile has no numbering-plan knowledge, and giving it one would put a versioned database inside a deploy-time expression language. |
| A `local.phone` capability (Spec 018) invoked from a process | An activity cannot reach write time, which is the only moment where rejecting and rewriting the value is still cheap and still total. |
| Normalize in SQL with a function | Needs the same metadata database inside Postgres, and makes an engine upgrade and a database migration have to agree on numbering plans. |
| Validate in Rust but store the submitted spelling | Leaves the uniqueness constraint meaningless, which is half the reason to check at all. |
| Reject a null as a violation | Presence is declared, never inferred (ADR-032). A `not_null` entry says it, with its own message. |

## Consequences

An ordering property from ADR-032 is deliberately narrowed. Entries are
evaluated in document order *among entries of the same kind*, but a `phone`
entry is decided before the statement runs, so a phone rejection is reported
ahead of any expression gate later in the list — and ahead of the permission's
`check`, which is SQL. A caller who may not write the row can therefore learn
that a number was malformed. That is the same exposure GraphQL input coercion
already has (a malformed UUID is refused before any permission is evaluated),
and it is the price of not sending an unusable value to the database at all.
Authors who need a permission failure to mask a value failure should keep the
row-level rule in `check`, where it always ran first.

Two limits follow from the check reading the submitted value rather than the
written row. A column filled by a database `DEFAULT` is not seen, and neither
is a nested object insert — nested inserts into a table whose role declares
validators are already refused. A permission preset (`set`) *is* seen and
normalized, because presets are resolved in the planner.

`validate` remains Postgres-only. A `phone` entry alone would in fact work on
any backend, since it never becomes SQL, but the refusal is per list rather
than per entry, and a rule that depends on which entries a list happens to
contain is worse than one that is simply narrow. Widening it is a later
decision.

The wasm plan core links the numbering-plan database too, because it runs the
same planner; the release artifact grows by roughly the size of that database.
This is not optional — a plan built in wasm that skipped normalization would
store a different value than one built natively.

The metadata version is pinned exactly and covered by a sentinel test, but it
does **not** yet appear in any deployment fingerprint: the engine has no
engine-wide fingerprint, only per connector operation and per process
revision ones. Spec 021 wants the same guarantee for the timezone database, so
the two should share one piece of plumbing when it is built.
