# Reference porting register

This register is the authoritative provenance record for the declarative SaaS
runtime. A specification may summarize entries, but an implementation may not
copy, vendor, generate from, or adapt an upstream file until this register has
the required immutable record.

## Admission rule

A source-level port is allowed only when all of the following are recorded in
the same pull request before the imported artifact:

1. upstream repository URL and full immutable commit;
2. exact source file or generated-artifact path and SHA-256;
3. license classification and the required copyright or NOTICE text;
4. destination path in this repository and the responsible module;
5. the Donat test that was added first and failed before the implementation;
6. review of every copied diff, including any generated output.

Apache-2.0, MIT, and BSD-compatible sources are eligible only after that
record. Non-permissive sources are behavior-only references: no source file,
fixture, generated artifact, or large verbatim text may be copied. A future
port creates or updates root THIRD_PARTY_NOTICES.md before it lands. This
register does not waive upstream obligations.

## Active references

| ID | Upstream and revision | Exact source surface | License | Status and permitted use | First Donat test destination |
| --- | --- | --- | --- | --- | --- |
| DONAT-NATIVE | current repository revision at implementation time | crates/metadata, catalog, schema, ir, sqlgen, server, conformance | Apache-2.0 | native source; extend established patterns rather than copy external code | crate-local unit/insta plus crates/conformance |
| CEL-0252 | google/cel-spec cb51b4176013ad19bd00df94be273c322916a620 | doc/, conformance/proto2/, conformance/proto3/, conformance/test/ | Apache-2.0 | behavior and selected expected-value cases only; no protobuf or generated Go is planned | crates/rules tests and command/process integration |
| TEMPORAL-RUST | temporalio/sdk-rust d2769368df9077a311537431ff4594c9c14db4e7 | ARCHITECTURE.md, crates/sdk-core/, crates/workflow/ | MIT | durable-history, replay, lease, and retry behavior only; no Temporal client, server, protocol, or source import | crates/server process integration |
| STRIPE-OAS | stripe/openapi 6dfda253ec9229dd4d20e0cac3ec9b1ff31fac69 | openapi/spec3.json, openapi/fixtures3.json | MIT | Checkout contract reference; an individual schema/generated artifact is eligible but no file is copied yet | crates/server connectors/stripe contract tests |
| STRIPE-MOCK | stripe/stripe-mock 3f370d112ba55a8a12c09b162547ba32f26b9693 | executable release; main_test.go, server/, spec/ for behavior reading | MIT | black-box test server only; Go source is not compiled or ported | connector stripe-mock integration test |
| AIRBYTE-STRIPE | airbytehq/airbyte 32ec364b51e96f748e6aea28bbbac2dd9aac8bd9 | source-stripe manifest.yaml, acceptance-test-config.yml, integration_tests/, unit_tests/ | ELv2 | behavior and test-category reference only; no source or fixture copy | Donat-owned Stripe acceptance fixtures |
| TEMPORAL-DOCS | [Temporal safe deployments](https://docs.temporal.io/develop/safe-deployments), accessed 2026-07-28 | deployment compatibility and worker versioning concepts | documentation | behavior-only reference for revision ABI fencing; no code, fixture, or text copy | process rolling-deploy integration tests |
| AWS-SFN-DOCS | [AWS Step Functions Task](https://docs.aws.amazon.com/step-functions/latest/dg/state-task.html) and [error handling](https://docs.aws.amazon.com/step-functions/latest/dg/concepts-error-handling.html), accessed 2026-07-28 | task timeout and typed error-routing concepts | documentation | behavior-only reference for distinct activity deadlines and on_error routes; no code, fixture, or text copy | process timeout/error-route tests |
| CAMUNDA-DMN-DOCS | [Camunda decision-table hit policies](https://docs.camunda.io/docs/8.8/components/modeler/dmn/decision-table-hit-policy/), accessed 2026-07-28 | first/unique decision-table semantics | documentation | behavior-only reference; no code, fixture, or text copy | rules decision-table tests |
| INNGEST-DOCS | [Inngest concurrency](https://www.inngest.com/docs/guides/concurrency) and [throttling](https://www.inngest.com/docs/guides/throttling), accessed 2026-07-28 | deployment-wide concurrency and throttling concepts | documentation | behavior-only reference for shared operation capacity; no code, fixture, or text copy | two-worker capacity integration tests |
| RYU-JS-1.0.2 | [boa-dev/ryu-js](https://github.com/boa-dev/ryu-js) b8e8098f350af02a1ad7d21488ed41ab71ec5438 | `src/pretty/mod.rs`, `tests/d2s_test.rs`, crates.io package `ryu-js` 1.0.2 | Apache-2.0 OR BSL-1.0; Donat selects Apache-2.0 | compiled dependency for RFC 8785 / ECMAScript finite-binary64 formatting only; no upstream source is copied | `crates/connector-catalog/tests/remediation_red.rs` |
| MEDUSA-PETSHOP-BEHAVIOR | [medusajs/medusa](https://github.com/medusajs/medusa) 5b732d40ee78e4c9973fdb1e0ac247b319611f51 | store catalogue, cart, checkout, payment, fulfilment, cancellation, and refund behavior | MIT; behavior-only reference | no upstream bytes are copied; independently author Donat-owned executable store behavior | `examples/petshop/metadata/**/*_test.yaml` |
| SALEOR-PETSHOP-BEHAVIOR | [saleor/saleor](https://github.com/saleor/saleor) 8e6164e3d12327496660f91f836d5c3222d8d2b6 | product catalogue, checkout, order, payment, fulfilment, and refund behavior | BSD-3-Clause; behavior-only reference | no upstream bytes are copied; independently author Donat-owned executable store behavior | `examples/petshop/metadata/**/*_test.yaml` |
| SPREE-PETSHOP-BEHAVIOR | [spree/spree](https://github.com/spree/spree) b839535c9e634d61196b5ab341cd2b1ec062526c | storefront, cart, order, payment, fulfilment, cancellation, and refund behavior | BSD-3-Clause; behavior-only reference | no upstream bytes are copied; independently author Donat-owned executable store behavior | `examples/petshop/metadata/**/*_test.yaml` |
| LIBERATION-FONTS-2.1.5 | [liberationfonts/liberation-fonts](https://github.com/liberationfonts/liberation-fonts) release `2.1.5`, as packaged by Debian `fonts-liberation` 1:2.1.5-3 | `LiberationSans-{Regular,Bold,Italic,BoldItalic}.ttf`, `LiberationMono-{Regular,Bold,Italic,BoldItalic}.ttf` | SIL OFL 1.1 | embedded asset; the eight upstream `.ttf` files are copied verbatim into `crates/connectors/assets/fonts` and compiled into the binary; no glyph or table is modified; notice and per-file SHA-256 in root `THIRD_PARTY_NOTICES.md` | `crates/connectors/tests/local_document.rs::pdf_uses_only_embedded_fonts` |
| TYPST-0.15.1 | crates.io packages `typst`, `typst-layout`, `typst-pdf` 0.15.1 | compiled dependency; `typst::World` is implemented by Donat in `crates/connectors/src/local/document/world.rs` | Apache-2.0 | compiled dependency only; no upstream source is copied, and the sandbox (file resolution, package denial, fonts, clock) is Donat-owned | `crates/connectors/tests/local_document.rs::pdf_world_denies_filesystem_and_packages` |
| MRML-6.0.1 | crates.io package `mrml` 6.0.1 | compiled dependency; MJML parse and render only, with `default-features = false` so no HTTP or filesystem include loader is built | MIT | compiled dependency only; no upstream source is copied; interpolation and escaping are Donat-owned | `crates/connectors/tests/local_document.rs::email_render_escapes_by_default` |
| RUST-XLSXWRITER-0.97.1 | crates.io package `rust_xlsxwriter` 0.97.1 | compiled dependency; workbook writing only | MIT OR Apache-2.0; Donat selects Apache-2.0 | compiled dependency only; no upstream source is copied; formula-injection defence and the typed-cell mapping are Donat-owned | `crates/connectors/tests/local_document.rs::spreadsheet_rejects_formula_injection` |
| ICALENDAR-0.17.13 | crates.io package `icalendar` 0.17.13 | compiled dependency; iCalendar building and parsing | MIT OR Apache-2.0; Donat selects Apache-2.0 | compiled dependency only; no upstream source is copied; UID and DTSTAMP policy is Donat-owned | `crates/connectors/tests/local_document.rs::calendar_uid_comes_from_input` |
| QRCODE-0.14.1 | crates.io package `qrcode` 0.14.1 | compiled dependency; matrix encoding through `qrcode::bits` and the module grid only, with `default-features = false` so neither the `image` nor the `svg` renderer is built | MIT OR Apache-2.0; Donat selects Apache-2.0 | compiled dependency only; no upstream source is copied; the payload typing, the pre-render capacity check, and both output writers are Donat-owned | `crates/connectors/tests/local_code.rs::qr_capacity_is_checked_before_render` |
| BARCODERS-2.0.0 | crates.io package `barcoders` 2.0.0 | compiled dependency; Code128, Code39, and EAN-13 module encoding only, with `default-features = false` so the `ascii`, `json`, and `svg` generators are not built | MIT | compiled dependency only; no upstream source is copied; the raster and SVG layout are Donat-owned | `crates/connectors/tests/local_code.rs::code_render_is_deterministic` |
| IMAGE-0.25.10 | crates.io package `image` 0.25.10 | compiled dependency; PNG, JPEG, GIF, and WebP decoding plus PNG and JPEG encoding, with `default-features = false` so the feature list is the format allowlist | MIT OR Apache-2.0; Donat selects Apache-2.0 | compiled dependency only; no upstream source is copied; the fixed decode order — allowlist, header format, pre-allocation dimension check, decoder limits, frame probe, metadata-dropping re-encode — is Donat-owned | `crates/connectors/tests/local_image.rs::image_dimensions_are_checked_before_allocation` |
| CALAMINE-0.36.0 | crates.io package `calamine` 0.36.0 | compiled dependency; `.xlsx` reading only — sheet ranges, cached formula values, the workbook's 1904 flag, and `ExcelDateTime::to_ymd_hms_milli` — with `default-features = false` so neither the `dates`/`chrono` conversions nor the `picture` extractor is built | MIT | compiled dependency only; no upstream source is copied; the archive guard that runs before it, the declared-schema binding, and every coercion rule are Donat-owned | `crates/connectors/tests/local_ingest.rs::ingest_date_systems_are_exact` |
| CSV-1.4.0 | crates.io package `csv` 1.4.0 | compiled dependency; record reading only | Unlicense OR MIT; Donat selects MIT | compiled dependency only; no upstream source is copied; the header binding, the row bounds, and the coercion rules are Donat-owned | `crates/connectors/tests/local_ingest.rs::ingest_is_deterministic` |
| ZIP-8.6.0 | crates.io package `zip` 8.6.0 | compiled dependency; central-directory reading and deflate extraction, with `default-features = false` so `deflate` is the only compression method built | MIT | compiled dependency only; no upstream source is copied; the ordered bounds — entry count and declared expansion before extraction, real uncompressed bytes during it — are Donat-owned | `crates/connectors/tests/local_ingest.rs::ingest_bounds_precede_decompression` |
| RRULE-0.14.0 | crates.io package `rrule` 0.14.0 | compiled dependency; RFC 5545 `RRULE` parsing, validation, and occurrence iteration only, with `default-features = false` so neither the `cli-tool` binary nor the deprecated `exrule` handling is built | MIT OR Apache-2.0; Donat selects Apache-2.0 | compiled dependency only; no upstream source is copied. The expansion is driven over naive wall-clock time and the zone is resolved by Donat, so the library's own DST behaviour never runs; the pre-expansion boundedness arithmetic, the declared DST policy, and the refusal of anything that would read the machine's timezone are Donat-owned | `crates/connectors/src/local/recurrence.rs::recurrence_rejects_unbounded_rules` |

## Verified behavior references

### RYU-JS-1.0.2 — RFC 8785 number-format dependency

- Upstream: [boa-dev/ryu-js](https://github.com/boa-dev/ryu-js) @
  `b8e8098f350af02a1ad7d21488ed41ab71ec5438`
- Crates.io package checksum:
  `dd29631678d6fb0903b69223673e122c32e9ae559d0960a38d574695ebc0ea15`
- Formatter: `src/pretty/mod.rs`
  (SHA-256: `3d02810bb9bf62756591a46a1a0aef1729ff8d94443b268a147dcd65a5ca6b2b`)
- Upstream vectors: `tests/d2s_test.rs`
  (SHA-256: `570a6bcfb6fe3a3c3593f6dbf6bd2e6240668141574f8f3d436d6dc595dc41b1`)
- License: Apache-2.0 OR BSL-1.0; Donat selects Apache-2.0.
  `LICENSE-APACHE` SHA-256:
  `62c7a1e35f56406896d7aa7ca52d0cc0d272ac022b5d2796e7d6905db8a3636a`.
- Selected behavior: `Buffer::format_finite` supplies the ECMAScript
  `Number::toString` spelling required by RFC 8785 after Donat independently
  proves that a preserved raw decimal token has the same mathematical value
  as the parsed finite binary64.
- Destination: direct pure-Rust dependency of `donat-connector-catalog`; it
  adds no mandatory dependency, build script, JavaScript, Node, WASM,
  sidecar, or runtime-loaded module. No upstream source is copied.
- Initial RED evidence: `raw_numbers_use_exact_ecmascript_canonicalization`
  rejected the old formatter's `1.0 -> 1` contract and
  `every_finite_rfc_8785_appendix_b_vector_is_exact` independently fixes all
  finite Appendix B bit-pattern/output pairs.

### STRIPE-CHECKOUT-PHASE-1 — behavior-only contract audit

- Upstream: [stripe/openapi](https://github.com/stripe/openapi) @
  `6dfda253ec9229dd4d20e0cac3ec9b1ff31fac69`
- Source: `openapi/spec3.json`
  (SHA-256: `e24a26de4188fd64dec4c043d5d3726277fdcb07556a493ea481c305b0a223d8`)
- License: MIT. No OpenAPI schema, generated artifact, fixture, or source text
  is copied into Donat.
- Selected behavior: `POST /v1/checkout/sessions` accepts
  `application/x-www-form-urlencoded`; this Phase-1 module independently
  encodes only `mode`, `success_url`, `cancel_url`, `client_reference_id`, and
  `line_items[*].price`/`line_items[*].quantity`, then extracts the Checkout
  Session's `id`, `url`, `status`, and `expires_at` from a JSON response.
- Destination: independent Rust implementation in
  `crates/server/src/connectors/stripe.rs` and Donat-owned tests in
  `crates/server/tests/connectors_stripe.rs` plus crate-local unit tests.
- Initial RED evidence: the Donat-owned `connectors_stripe` integration target
  failed before the module existed with `no StripeConnector in
  connectors::stripe`. The later crate-local test
  `stripe_checkout_posts_form_and_returns_typed_session` covers the resulting
  form contract.

### STRIPE-MOCK-CHECKOUT-PHASE-1 — behavior-only mock-server audit

- Upstream: [stripe/stripe-mock](https://github.com/stripe/stripe-mock) @
  `3f370d112ba55a8a12c09b162547ba32f26b9693`
- Sources: `server/server.go`
  (SHA-256: `74e227ddf08787f7b070213dbc2e95c5dece69788ba664b0058b71693caa82fc`)
  and `server/server_test.go`
  (SHA-256: `96a68725ac45e277e90f7094fe236689ae078cca5c4dc5948b66c38abeafe154`)
- License: MIT. No Go source, fixture, executable, generated output, or test
  text is copied into Donat.
- Selected behavior: local tests independently verify form content type and a
  stable `Idempotency-Key` request header against a Donat-owned Axum stub; no
  Stripe account or live endpoint is used.
- Destination: independent Rust implementation in
  `crates/server/src/connectors/stripe.rs` and Donat-owned tests in
  `crates/server/tests/connectors_stripe.rs` plus crate-local unit tests.
- Initial RED evidence: the Donat-owned `connectors_stripe` integration target
  failed before the module existed with `no StripeConnector in
  connectors::stripe`. The later crate-local test
  `stripe_checkout_posts_form_and_returns_typed_session` covers the resulting
  form contract.

No source-level port is made by these records, so `THIRD_PARTY_NOTICES.md` is
not created or changed for this slice.

## Per-port record template

Add one subsection for every actual imported file:

~~~markdown
### PORT-YYYY-NNN — short description

- Upstream: owner/repo @ full-commit
- Source: exact/path.ext (SHA-256: ...)
- License: SPDX identifier; notice copied to THIRD_PARTY_NOTICES.md line ...
- Destination: crates/.../file.rs
- Adaptation: what was changed in the independent Rust rewrite
- Red test: crate/test path and exact test identifier
- Green evidence: command and reviewed output
- Reviewer: name/date
~~~

A code review rejects a port whose source path, license, notice, red test, or
destination is absent. A source reference with status behavior only cannot be
promoted to copied code by changing its description; it needs a new eligible
record.
