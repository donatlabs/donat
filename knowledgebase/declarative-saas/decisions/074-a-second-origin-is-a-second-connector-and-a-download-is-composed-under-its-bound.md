---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A second origin is a second connector, and a download is composed under the bound that carries it

## Context

Spec 025 (Batch I, storage and messaging) named two structural problems and
asked for a deliberate answer to each rather than a workaround.

**A provider whose content and metadata live on different hosts.** Dropbox's own
published specification carries the split as an attribute: `route download
(DownloadArg, FileMetadata, DownloadError)` has `host = "content"` and `style =
"download"`, and its HTTP reference says "RPC endpoints … are on the
`api.dropboxapi.com` domain" beside "Content-download endpoints … are on the
`content.dropboxapi.com` domain". Spec 010 §4 makes a connector's origin a
compile-time constant that "nothing in a request, a credential, a provider
response, a pagination cursor, or a webhook payload may change". One connector
cannot serve both.

The precedent already existed and had never been *taken*: `hubspot` dropped
`form.submit` because HubSpot serves it from `api.hsforms.com`, and its module
says "A HubSpot forms connector is its own module with its own origin, its own
credential contract, and its own batch." No such module was ever written, so the
programme had a stated remedy and no worked example of it.

**A download that answers with bytes rather than JSON.** The SDK's response
contract is JSON, and `google_drive.file.download` already composes
`{content_base64, content_bytes, content_type}` inside its module, bounded by
the shared 1 MiB ceiling, with the gap recorded in
[[056-a-scope-is-a-property-of-an-operation-and-a-success-envelope-can-carry-a-failure]].
Batch I has two byte surfaces rather than one, and the second is not the same
shape as the first.

## Decision

**A second origin is a second connector.** `dropbox_content` is its own module,
its own `&'static Connector`, its own registry entry, and its own deployment
instance, carrying exactly one operation: `file.download`. A deployment that
needs both halves of Dropbox configures two instances against the same OAuth2
client, and names both.

The alternative spec 025 offered — an SDK that admits a declared, closed set of
origins per connector — is refused, and the reason is what the single origin
*buys*. Today `ProviderRuntime::origin` answers one value, the executor resolves
that host, pins the resolved addresses, and refuses a connected peer it did not
resolve; `Pagination` refuses a continuation outside it; `Origin::contains` is a
single comparison. A set turns every one of those into "which of these?", and
the question would have to be answered per operation, per page, per
continuation, and per redirect. The safety property that survives that change is
weaker than the one the programme has, and it would be bought for a provider
whose own specification already models the split as two hosts.

The visible cost is real and is the point: two instances, two rows in
`connectors.yaml`, and two authorizations if a deployment revokes one. Two
origins are two authorities, and a deployment reaching both should say so twice.

**Box's download is declared nowhere, because there is no origin to compile.**
Box publishes `GET /files/{file_id}/content` and publishes what it answers: "302
— If the file is available for download the response will include a `Location`
header for the file on `dl.boxcloud.com`. The `dl.boxcloud.com` URL is not
persistent", and "200 — Returns the requested file **if the client has the
follow redirects setting enabled**". The SDK's transport follows no redirect.
Even the Dropbox answer does not apply: a second connector needs a *compiled*
origin, and Box publishes that the host its bytes come from is not stable. So
the operation is absent rather than declared-and-broken, and
`box_download_is_a_redirect_to_a_third_origin` records why in a test, the way
`hubspot_form_submission_is_a_different_origin` does.

**A download composes its declared output inside its module, and the bound runs
before the composition.** `dropbox_content.file.download` follows the
`google_drive` precedent and adds two things to it.

* The argument is a header the module composes. Dropbox passes a
  content-download endpoint's argument as JSON in `Dropbox-API-Arg`, so the
  declaration binds that header from an input slot marked `supplied_input` and
  the module writes the one-field document from the caller's typed `path`. No
  Process can bind the slot, and a path carrying a control character is refused
  at the boundary rather than escaped into a value Dropbox would read as a
  different file.
* The provider's own metadata travels beside the bytes. Dropbox publishes "the
  result will appear as JSON in the `Dropbox-API-Result` response header", so the
  composed output carries a `metadata` field read from it — and a header that is
  not that JSON is `null` rather than a failure, because the bytes are the
  answer.

The order inside `decode` is the contract: status, then ceiling, then
composition. A body one byte past `MAX_HTTP_BODY_BYTES` is a `validation`
failure carrying no part of the file, which
`dropbox_content_download_is_bounded` asserts at the exact boundary and one byte
over. Half a file is not a file, and a truncated one is indistinguishable from a
complete one downstream — the same reason the pagination walk emits no partial
aggregate.

## Alternatives

| Option | Why Not |
|--------|---------|
| Give the SDK a closed set of origins per connector, selected per operation | Every safety property built on "the origin" becomes "which origin?": the resolve-then-pin rule, the connected-peer check, the cross-origin continuation refusal, and the catalog's published endpoint identity. That is a large change to the most load-bearing invariant in the SDK, bought for a provider that publishes the split as two hosts itself |
| Point one connector at `api.dropboxapi.com` and let the download rewrite the host | The exact thing spec 010 §4 forbids, spelled as a special case. An origin a module may rewrite is not a fixed origin, and the rule would then be "fixed, except where a module disagrees" |
| Serve Box's download by following the `302` to `dl.boxcloud.com` | The transport follows no redirect by design, and Box publishes that the URL is not persistent — so there is no origin a deployment could have named, reviewed, or resolved. Following it would send a credentialed request to a host chosen by a provider response |
| Return the bytes as a `content_url` a Process fetches later | It moves the fetch outside the connector boundary, where no origin is compiled, no ceiling is enforced, and no credential is applied. The ceiling is the whole reason the download is admitted at all |
| Stream the download into the file-attachment store instead of the response | A real design, and a much larger one: it needs a storage destination in the operation's declaration, a second failure mode between provider and store, and a cleanup path. It is the right answer for files past 1 MiB and is not this batch |
| Let the caller supply the `Dropbox-API-Arg` value directly | It hands a caller the whole argument document — including fields nobody reviewed — through a header. The point of composing it is that the caller chooses one typed field of it |

## Consequences

The programme has a worked example of the rule it had already written down, and
the next provider that splits its hosts inherits a pattern instead of
rediscovering the question. `dropbox` and `dropbox_content` are two names in
`ConnectorRegistry::built_in_module_names`, two conformance fixtures, and two
instances in any deployment that reads and downloads.

Box's file surface is read, listed, searched, and deleted, and its bytes are not
reachable from this engine at all. That is a real gap in the connector, it is
recorded in the module header and in `INVENTORY.md`, and the sentence Box would
have to publish to close it — a stable origin for the content — is named.

The download bound is now asserted at the boundary rather than assumed from the
transport's own ceiling. The transport refuses an oversized *response* already;
the module's check exists because `decode` is also reachable from tests, from a
walk's page gate, and from any future caller that hands it bytes, and a bound
that only one caller enforces is a bound one edit removes.
