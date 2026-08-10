---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
  - "[[022-media-derivatives]]"
---

# A code payload is typed by its declaration, and a decoder is opened in a fixed order

## Context

Spec 022 adds the two media derivatives every client project rebuilds: a code on
a document, and a thumbnail of something a user uploaded. They look like one
feature and are two different problems.

A QR code on an invoice is a contract. Whoever scans it believes the business
put it there, and a scanner will follow whatever URL it finds. So the dangerous
input for `local.code` is not size — it is *meaning*: a payload assembled from
customer data that turns out to be a link somewhere else. Spec 022 §1 says the
payload is "typed, not free text" and that a `url` payload is checked against
"the declared allowed origins for that template", and that capacity is answered
before rendering rather than by "silently upgrading the version".

`local.image` has the opposite problem. Its input is a file whose author chose
every byte, *including the bytes that say how large it is*. A few kilobytes can
declare 40000×40000, which is 6.4 GB of RGBA — and the natural way to write a
decode ("open it, then check what came out") pays for the attack before it
detects it. Spec 022 §2 therefore fixes an order and asks for a test per step.

ADR 044 left both with the same seam question spec 019 answered for templates:
the executor is a `fn(&LocalInvocation) -> Result<LocalProduct, _>` whose only
argument is the input, and neither an origin allowlist nor a media-type
allowlist may travel in a value the requester controls.

## Decision

**A code payload has three declared types and no fourth.** `url`, `ticket`, and
`payment` — there is no `text`, because a free-text payload is a URL whenever an
attacker writes one. A `url` payload is parsed to its origin and that origin
must be in the set `media.yaml` declared for *that template*. The origins are
canonicalized once, at load, into the exact spelling a payload's origin is
compared against; both sides run the same strict parser, which refuses a
credential in the authority, an upper-case or non-ASCII host, and any scheme but
`http`/`https`. The allowlist is meaningful only because it arrives in the
`LocalContext` beside the input, the way spec 019's templates do (ADR 050): it is
the one part of the path the party supplying the payload does not choose.

**Capacity is answered before a symbol exists.** The declaration fixes the QR
version, `qrcode::bits` is driven directly against that version, and an
over-capacity payload is a `validation` refusal — never a larger symbol. The
capacity is computed from the crate's own tables rather than a second table kept
here, so the number in the refusal is the number the encoder enforces. Because
`ConnectorFailure` carries a `&'static str` message by design, the two numbers
ride in the typed correlation ids (`capacity_bytes`, `payload_bytes`), which is
the channel that failure type already has for operator-visible detail.

**The image decode order is the feature.** Six steps, in this order, each with
its own test: the target's media-type allowlist; the format confirmed from the
byte header and required to agree with the stored media type; the dimensions
read from the header and checked against `max_pixels` **before any pixel buffer
is allocated**; the decoder's own `Limits` set from the same declaration as a
deliberately redundant second line; a frame count read from the container's own
index — a GIF's block chain, a PNG's chunk list, a WebP's RIFF chunks — so
"is it animated" is answered without decoding; and a re-encode that carries no
metadata across, with the EXIF orientation applied to the pixels before it is
discarded. `image` is compiled with `default-features = false` and four formats,
so the feature list *is* the allowlist at the link level, and SVG is refused
three times over: a declaration naming it does not resolve, the runtime
allowlist rejects it, and no SVG decoder exists to reach.

## Alternatives

| Option | Why Not |
|--------|---------|
| Allow a `text` payload type "for the cases that are not a URL" | Every payload is text. The type exists to say what the value *means* so the check can be against a declaration; a `text` type is the check switched off, and it is the one an author under time pressure picks. |
| Validate a `url` payload by inspecting it for something suspicious | That is the injection spelled as a defence: the attacker supplies the value and therefore the inspection's answer. An origin set the deployment wrote is the only input they do not control (the same argument ADR 050 made for `Html`). |
| Use a URL crate and compare hosts | A general parser normalizes generously — that is its job — and each generosity is a way for two different destinations to compare equal. The shapes that must be refused here are few and known, so they are refused explicitly. |
| Raise the QR version to fit an over-capacity payload | The declared version is what the printed layout was built around. Growing from version 6 to 12 produces a symbol that renders, scans, and does not fit the page it was designed for — a failure discovered by a customer rather than by a process. |
| Put the capacity table in this module | Two tables that must agree, one of which is the encoder's. The refusal would eventually name a capacity the encoder does not enforce. |
| Check the pixel count after decoding, from the decoded image | The allocation is the attack. By the time there is a decoded image to measure, the 6.4 GB has been asked for. |
| Rely on `image`'s `Limits` alone | They are non-strict by the crate's own documentation — "some decoders may ignore it" — and they are one library's option, which a minor release may default differently. The header check is ours and is strict; the limits are the second line. |
| Drop the decoder limits, since the header check is strictly stronger | A redundant bound is exactly what catches the case the first bound cannot: a decoder that reports one size and allocates another. Redundancy is the reason it is worth having, not a reason to remove it. |
| Trust the stored attachment's media type and decode by it | Then the uploader picks which decoder runs, by naming a media type whose parser they prefer. The magic is what the file is; the media type is what somebody said. |
| Count frames by decoding them and stopping after the first | The decode is what the bound exists to prevent, and a first frame in a hostile file can be the expensive one. The container's index answers the question for the cost of a walk. |
| Accept SVG and rasterize it | SVG is a document format with external references, scripting, and its own decompression bombs. There is no configuration of a raster decode path that makes it safe; the only safe answer is that no such decoder is linked in. |
| Keep EXIF and strip only the GPS tags | The tag list is upstream's to extend, and a maker-note blob is opaque. Re-encoding from pixels carries nothing across by construction, and needs no list to stay correct. |
| Drop EXIF without applying the orientation | Every phone photo taken in portrait would silently arrive on its side, and the fix would be to put the metadata back. Orientation is read from the decoder, applied to the pixels, and then there is nothing left to drop. |
| Carry the source image in the activity input as base64 | The input is what the journal retains, the fingerprint is taken over, and `max_input_bytes` measures. A stored file arrives as execution context instead, on the seam spec 020 opened (ADR 052) — one resolution for both capabilities. |
| Let a code template be selected or built by the process at runtime | Choosing among declared templates sounds harmless until "which origins are allowed" becomes a value the run computes. A literal name from the deployment's declarations keeps `validate` able to typecheck it. |
| Return every rendered code inline as a string | An activity result is a journal entry per attempt. Inline is admitted only for SVG, only under a declared `max_inline_bytes`, and a code over it is a refusal rather than a silent switch to storage — so the delivery a template uses is the delivery it declared. |

## Consequences

A deployment gets a code renderer and an image pipeline for one `media.yaml`,
and both are `Pure`: the process owns the retries, and a re-render is free.
Every rendered code and every produced thumbnail is a file in the attachment
store behind a signed URL, claimed by the role the process writes as (ADR 033),
because both go through the artifact handoff ADR 044 already built.

The cost is a real authoring burden on the code side. A URL payload cannot be
declared without naming the origins it may point at, which is a second thing to
edit when a payment host changes — and it is the point: the deploy is what makes
the allowlist mean anything. The image side costs a declaration per shape
(`avatar`, `receipt_scan`) rather than one generic "make a thumbnail", and a
deployment that wants animated sources has to say so; the default refuses them,
because a deployment that never considered animation should get the answer that
decodes least.

The decode order also fixes what the engine will never do here: it will not
guess a format, will not decode a media type nobody declared, will not decide
that a large picture is fine after seeing it, and will not pass any byte of
source metadata through to an output.
