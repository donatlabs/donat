//! Bounded pagination plans.
//!
//! The closed set of six plans below is what a connector may declare about a
//! provider's continuation protocol. Each shares one [`PaginationBudget`] with
//! the logical attempt, and the walk stops at the first ceiling it reaches
//! rather than truncating: a partial aggregate is indistinguishable downstream
//! from a complete one, so it is never returned.
//!
//! A plan here is not documentation. [`Pagination::collect_pages`] is what the
//! serving executor in `donat-server` runs for every operation whose module
//! declared one, so an operation with a plan reaches its provider as a walk and
//! an operation without one reaches it as a single request
//! ([[034-a-declaration-the-runtime-ignores-is-a-defect]]).
//!
//! No plan can move a request off the connector's compiled origin. The two
//! plans that follow a provider-chosen destination — a `Link` continuation and
//! a body-carried next URI — resolve it against the origin and then check it,
//! and a continuation that lands anywhere else is rejected rather than
//! followed. A `TokenInBody` value, by contrast, is always a query value: a
//! body that spells an absolute URL becomes a percent-encoded query parameter
//! on the same origin, never a destination.
//!
//! The walk classifies nothing. Whether a page is a success is asked of the
//! caller, once per page, so that the answer is the *operation's* — its
//! declared success statuses, its `ErrorMap`, and for a provider that reports
//! failure inside a `2xx`, its module's own body gate — rather than a fallback
//! this file invented for a connector that declared no rule.

use std::future::Future;
use std::time::Duration;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Url;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure};
use crate::sdk::operation::{
    DEFAULT_OPERATION_DEADLINE, OperationError, Origin, RequestPlan, validate_json_pointer,
    validate_query_key,
};
use crate::sdk::transport::{MAX_HTTP_BODY_BYTES, RawHttpResponse};

/// The one budget a logical attempt spends across every page it fetches.
///
/// Every plan shares it, so a provider cannot turn "one activity" into an
/// unbounded amount of work by offering an endless continuation. Exceeding any
/// ceiling fails the attempt: pagination never emits partial output, because a
/// truncated aggregate is indistinguishable from a complete one downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaginationBudget {
    max_calls: u32,
    max_pages: u32,
    max_items: usize,
    max_aggregate_bytes: usize,
    deadline: tokio::time::Instant,
}

impl PaginationBudget {
    /// The ceilings one logical attempt spends when its connector declares a
    /// continuation plan but no budget of its own.
    ///
    /// Most providers publish a protocol and no limit, so the SDK carries the
    /// limit. The aggregate byte ceiling is the transport's own body ceiling:
    /// a walk assembles one document that the declaration then decodes, and a
    /// walk that produced more bytes than this connector would accept in a
    /// single response is refused rather than truncated.
    pub const DEFAULT_MAX_CALLS: u32 = 16;
    pub const DEFAULT_MAX_PAGES: u32 = 16;
    pub const DEFAULT_MAX_ITEMS: usize = 5_000;
    pub const DEFAULT_MAX_AGGREGATE_BYTES: usize = MAX_HTTP_BODY_BYTES;

    /// The default ceilings, with a placeholder deadline.
    ///
    /// A budget's deadline belongs to one attempt, and an attempt does not
    /// exist when a connector is compiled — so every caller that spends this
    /// budget binds it with [`Self::with_deadline`] first, and the placeholder
    /// is the operation's own default deadline rather than a value that could
    /// silently outlive an activity.
    #[must_use]
    pub fn default_ceilings() -> Self {
        Self::new(
            Self::DEFAULT_MAX_CALLS,
            Self::DEFAULT_MAX_PAGES,
            Self::DEFAULT_MAX_ITEMS,
            Self::DEFAULT_MAX_AGGREGATE_BYTES,
            DEFAULT_OPERATION_DEADLINE,
        )
    }

    pub fn new(
        max_calls: u32,
        max_pages: u32,
        max_items: usize,
        max_aggregate_bytes: usize,
        time_to_live: Duration,
    ) -> Self {
        Self {
            max_calls,
            max_pages,
            max_items,
            max_aggregate_bytes,
            deadline: tokio::time::Instant::now() + time_to_live,
        }
    }

    /// Share the enclosing activity's deadline rather than starting a new one.
    #[must_use]
    pub fn with_deadline(mut self, deadline: tokio::time::Instant) -> Self {
        self.deadline = deadline;
        self
    }

    pub const fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }

    pub const fn max_calls(&self) -> u32 {
        self.max_calls
    }

    pub const fn max_pages(&self) -> u32 {
        self.max_pages
    }

    pub const fn max_items(&self) -> usize {
        self.max_items
    }

    pub const fn max_aggregate_bytes(&self) -> usize {
        self.max_aggregate_bytes
    }

    /// Whether one more call may be made.  Checked before the call, so the
    /// ceiling counts calls attempted, not calls that happened to succeed.
    pub fn admit_call(&self, calls_so_far: u32) -> Result<(), ConnectorFailure> {
        if calls_so_far >= self.max_calls {
            return Err(budget_failure(
                "connector pagination exceeded its call budget",
            ));
        }
        Ok(())
    }

    pub fn admit_page(&self, pages_so_far: u32) -> Result<(), ConnectorFailure> {
        if pages_so_far >= self.max_pages {
            return Err(budget_failure(
                "connector pagination exceeded its page budget",
            ));
        }
        Ok(())
    }

    /// Whether the running totals after a page are still inside the budget.
    pub fn admit_totals(
        &self,
        aggregate_bytes: usize,
        items: usize,
    ) -> Result<(), ConnectorFailure> {
        if aggregate_bytes > self.max_aggregate_bytes {
            return Err(budget_failure(
                "connector pagination exceeded its aggregate byte budget",
            ));
        }
        if items > self.max_items {
            return Err(budget_failure(
                "connector pagination exceeded its item budget",
            ));
        }
        Ok(())
    }

    pub fn admit_deadline(&self) -> Result<(), ConnectorFailure> {
        if tokio::time::Instant::now() >= self.deadline {
            return Err(ConnectorFailure::timeout());
        }
        Ok(())
    }
}

/// A provider that keeps offering work past a declared ceiling has answered
/// outside the contract the connector declared for it, so the aggregate is
/// refused rather than truncated.
fn budget_failure(safe_message: &'static str) -> ConnectorFailure {
    ConnectorFailure::new(
        ConnectorErrorClass::Validation,
        "connector_pagination_budget",
        safe_message,
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PaginationKind {
    /// An opaque token the provider echoes back.
    Cursor {
        cursor_in: String,
        cursor_out: String,
        page_size_key: String,
        page_size: u32,
    },
    OffsetLimit {
        offset_key: String,
        limit_key: String,
        page_size: u32,
    },
    PageNumber {
        page_key: String,
        per_page_key: String,
        page_size: u32,
        /// The number of the provider's *first* page. Typeform documents
        /// "page (integer, default: 1)"; Twilio's `page` is zero-indexed.
        first_page: u32,
    },
    /// RFC 8288 `Link`, whose next URL must be on the compiled origin.
    LinkHeader {
        rel: String,
    },
    TokenInBody {
        pointer: String,
        query_key: String,
    },
    /// A continuation *URI* in the response body — SendGrid's `_metadata.next`,
    /// Twilio's `next_page_uri` — resolved and then checked against the
    /// compiled origin exactly as [`PaginationKind::LinkHeader`] is.
    NextUriInBody {
        pointer: String,
    },
}

/// One declared pagination plan.
///
/// As with [`crate::sdk::auth::AuthPlan`], the representation is private: a
/// provider module selects one of these six, and adding a seventh is an edit to
/// this file with its own test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pagination {
    items_pointer: String,
    kind: PaginationKind,
}

impl Pagination {
    /// `items` is the JSON pointer to each page's item list; every plan
    /// declares it, because a page with no readable item list is a contract
    /// violation rather than an empty page.
    pub fn cursor(
        items: &str,
        cursor_in: &str,
        cursor_out: &str,
        page_size_key: &str,
        page_size: u32,
    ) -> Result<Self, OperationError> {
        validate_query_key(cursor_in)?;
        validate_json_pointer(cursor_out)?;
        validate_query_key(page_size_key)?;
        Self::build(
            items,
            PaginationKind::Cursor {
                cursor_in: cursor_in.to_owned(),
                cursor_out: cursor_out.to_owned(),
                page_size_key: page_size_key.to_owned(),
                page_size: positive(page_size)?,
            },
        )
    }

    pub fn offset_limit(
        items: &str,
        offset_key: &str,
        limit_key: &str,
        page_size: u32,
    ) -> Result<Self, OperationError> {
        validate_query_key(offset_key)?;
        validate_query_key(limit_key)?;
        Self::build(
            items,
            PaginationKind::OffsetLimit {
                offset_key: offset_key.to_owned(),
                limit_key: limit_key.to_owned(),
                page_size: positive(page_size)?,
            },
        )
    }

    /// A page-number walk over a provider whose first page is page 1.
    pub fn page_number(
        items: &str,
        page_key: &str,
        per_page_key: &str,
        page_size: u32,
    ) -> Result<Self, OperationError> {
        Self::page_number_from(items, page_key, per_page_key, page_size, 1)
    }

    /// The same walk over a provider that numbers its pages from `first_page`.
    ///
    /// The first page number is a declaration rather than a constant because
    /// providers disagree about it: Typeform documents "page (integer, default:
    /// 1)" while Twilio's `page` is zero-indexed, and a walk that assumed one
    /// of them would silently skip the first page of the other — a wrong
    /// answer, not a failure.
    pub fn page_number_from(
        items: &str,
        page_key: &str,
        per_page_key: &str,
        page_size: u32,
        first_page: u32,
    ) -> Result<Self, OperationError> {
        validate_query_key(page_key)?;
        validate_query_key(per_page_key)?;
        Self::build(
            items,
            PaginationKind::PageNumber {
                page_key: page_key.to_owned(),
                per_page_key: per_page_key.to_owned(),
                page_size: positive(page_size)?,
                first_page,
            },
        )
    }

    pub fn link_header(items: &str, rel: &str) -> Result<Self, OperationError> {
        if rel.is_empty()
            || !rel
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
        {
            return Err(OperationError::new("a link relation must be static"));
        }
        Self::build(
            items,
            PaginationKind::LinkHeader {
                rel: rel.to_owned(),
            },
        )
    }

    pub fn token_in_body(
        items: &str,
        pointer: &str,
        query_key: &str,
    ) -> Result<Self, OperationError> {
        validate_json_pointer(pointer)?;
        validate_query_key(query_key)?;
        Self::build(
            items,
            PaginationKind::TokenInBody {
                pointer: pointer.to_owned(),
                query_key: query_key.to_owned(),
            },
        )
    }

    /// A continuation URI the provider publishes inside the response body.
    ///
    /// This is [`Pagination::token_in_body`]'s opposite: that plan treats the
    /// value as data and can only ever spend it as a query value, while this
    /// one treats it as a destination — which is why it is resolved against the
    /// compiled origin and refused when it lands anywhere else, exactly as a
    /// `Link` continuation is. SendGrid publishes `_metadata.next` and Twilio
    /// publishes `next_page_uri`; both reject their own URI when it is sent
    /// back as a query value, so neither is expressible any other way.
    pub fn next_uri_in_body(items: &str, pointer: &str) -> Result<Self, OperationError> {
        validate_json_pointer(pointer)?;
        Self::build(
            items,
            PaginationKind::NextUriInBody {
                pointer: pointer.to_owned(),
            },
        )
    }

    /// Where each page's item list lives, and therefore where a completed walk
    /// writes its aggregate.
    ///
    /// A caller needs this to answer one question at startup: does the
    /// operation's declared output actually read the place the aggregate lands?
    /// A plan that collected `/data` for an operation publishing `/items` would
    /// publish page one's list and drop the rest of the walk in silence.
    #[must_use]
    pub fn items_pointer(&self) -> &str {
        &self.items_pointer
    }

    fn build(items: &str, kind: PaginationKind) -> Result<Self, OperationError> {
        // RFC 6901: the empty pointer references the whole document. A provider
        // whose collection *is* the response — GitHub answers every list
        // endpoint with a bare JSON array — has its items there and nowhere
        // else, and `JsonValue::pointer("")` returns exactly that. Every other
        // pointer still has to be static and absolute.
        if !items.is_empty() {
            validate_json_pointer(items)?;
        }
        Ok(Self {
            items_pointer: items.to_owned(),
            kind,
        })
    }

    /// Walk the pages and return one bounded aggregate.
    ///
    /// `fetch` sends one request; the caller owns transport, so the walk cannot
    /// reach a destination the caller did not resolve and pin. It receives each
    /// page's request *by value*, and the continuation is derived from the copy
    /// the walk kept — so a caller whose credential must be recomputed for the
    /// request it is actually sending applies it there. A signature is the case
    /// that forces this: AWS SigV4 covers the canonical query string, so a walk
    /// that signed only the first request would send every continuation with a
    /// signature for a different query. A caller whose credential is a static
    /// header may instead apply it once to `first`; every continuation keeps
    /// the headers that request carried.
    ///
    /// `admit` is asked about every page that arrives, before its body is read,
    /// and it is the *operation's* answer rather than this file's: its declared
    /// success statuses, its own `ErrorMap`, and — for a provider that reports
    /// failure inside a `2xx` — its module's body gate. A page it refuses ends
    /// the walk with exactly the failure a single request would have produced,
    /// so a `429` on page three is retryable for the same reason a `429` on
    /// page one is ([[034-a-declaration-the-runtime-ignores-is-a-defect]]).
    ///
    /// Nothing partial escapes: every return but the last is an error, and the
    /// items already collected go with it.
    pub async fn collect<F, Fut, G>(
        &self,
        first: RequestPlan,
        origin: &Origin,
        budget: &PaginationBudget,
        admit: G,
        fetch: F,
    ) -> Result<Vec<JsonValue>, ConnectorFailure>
    where
        F: FnMut(RequestPlan) -> Fut,
        Fut: Future<Output = Result<RawHttpResponse, ConnectorFailure>>,
        G: Fn(u16, &HeaderMap, &[u8]) -> Result<(), ConnectorFailure>,
    {
        self.collect_pages(first, origin, budget, admit, fetch)
            .await
            .map(Walk::into_items)
    }

    /// The same walk, keeping the page the aggregate is assembled into.
    ///
    /// A caller that only wants the items uses [`Self::collect`]. The serving
    /// executor wants the [`Walk`], because the operation's declared outputs are
    /// read from a whole document rather than from a list — see
    /// [`Self::aggregate`].
    pub async fn collect_pages<F, Fut, G>(
        &self,
        first: RequestPlan,
        origin: &Origin,
        budget: &PaginationBudget,
        admit: G,
        mut fetch: F,
    ) -> Result<Walk, ConnectorFailure>
    where
        F: FnMut(RequestPlan) -> Fut,
        Fut: Future<Output = Result<RawHttpResponse, ConnectorFailure>>,
        G: Fn(u16, &HeaderMap, &[u8]) -> Result<(), ConnectorFailure>,
    {
        let mut request = first;
        self.prime(&mut request);
        let mut items: Vec<JsonValue> = Vec::new();
        let mut aggregate_bytes = 0usize;
        let mut calls = 0u32;
        let mut pages = 0u32;
        loop {
            budget.admit_call(calls)?;
            budget.admit_page(pages)?;
            budget.admit_deadline()?;

            let mut next = request.clone();
            let response = fetch(request).await?;
            calls += 1;
            pages += 1;
            admit(
                response.status.as_u16(),
                response.headers(),
                response.body(),
            )?;
            aggregate_bytes = aggregate_bytes.saturating_add(response.body().len());
            let body: JsonValue = serde_json::from_slice(response.body()).map_err(|_| {
                ConnectorFailure::validation("connector provider returned malformed JSON")
            })?;
            let page = body
                .pointer(&self.items_pointer)
                .and_then(JsonValue::as_array)
                .ok_or_else(|| {
                    ConnectorFailure::validation(
                        "connector provider page did not carry the declared item list",
                    )
                })?;
            let page_len = page.len();
            items.extend(page.iter().cloned());
            budget.admit_totals(aggregate_bytes, items.len())?;

            // The deadline is checked before deciding to fetch again, not after
            // the last page: a walk whose final page arrived has done its work,
            // and failing it for a deadline it no longer needs would throw away
            // a complete aggregate.
            let facts = PageFacts {
                body: &body,
                response: &response,
                page_len,
                total_items: items.len(),
                pages,
            };
            if !self.advance(&mut next, origin, &facts)? {
                return Ok(Walk {
                    items,
                    document: body,
                    last: response,
                });
            }
            request = next;
        }
    }

    /// The one document a completed walk decodes as.
    ///
    /// It is the walk's final page with the complete aggregate written where
    /// the plan declared the item list. Everything else is that page's own,
    /// which is what lets one aggregate be read through exactly the declared
    /// output pointers a single page is read through — including the
    /// continuation field, which is absent there precisely because the walk
    /// reached the end.
    #[must_use]
    pub fn aggregate(&self, walk: Walk) -> JsonValue {
        let Walk {
            items,
            mut document,
            ..
        } = walk;
        // RFC 6901: the empty pointer is the whole document, which is where a
        // provider that answers with a bare array keeps its collection.
        if self.items_pointer.is_empty() {
            return JsonValue::Array(items);
        }
        if let Some(slot) = document.pointer_mut(&self.items_pointer) {
            *slot = JsonValue::Array(items);
        }
        document
    }

    /// The parameters a plan puts on its first request.
    fn prime(&self, request: &mut RequestPlan) {
        match &self.kind {
            PaginationKind::Cursor {
                page_size_key,
                page_size,
                ..
            } => set_query_pair(request.url_mut(), page_size_key, &page_size.to_string()),
            PaginationKind::OffsetLimit {
                offset_key,
                limit_key,
                page_size,
            } => {
                set_query_pair(request.url_mut(), offset_key, "0");
                set_query_pair(request.url_mut(), limit_key, &page_size.to_string());
            }
            PaginationKind::PageNumber {
                page_key,
                per_page_key,
                page_size,
                first_page,
            } => {
                set_query_pair(request.url_mut(), page_key, &first_page.to_string());
                set_query_pair(request.url_mut(), per_page_key, &page_size.to_string());
            }
            PaginationKind::LinkHeader { .. }
            | PaginationKind::TokenInBody { .. }
            | PaginationKind::NextUriInBody { .. } => {}
        }
    }

    /// Turn the request into the next page's request, or report that the walk
    /// is over.  The request keeps every header the one it was derived from
    /// carried, so a credential applied as a static header travels with each
    /// continuation; a credential that signs the request is applied again per
    /// page by the caller, because this is where the query it signs changes.
    fn advance(
        &self,
        request: &mut RequestPlan,
        origin: &Origin,
        page: &PageFacts<'_>,
    ) -> Result<bool, ConnectorFailure> {
        match &self.kind {
            PaginationKind::Cursor {
                cursor_in,
                cursor_out,
                ..
            } => match text_at(page.body, cursor_out) {
                Some(cursor) => {
                    set_query_pair(request.url_mut(), cursor_in, &cursor);
                    Ok(true)
                }
                None => Ok(false),
            },
            PaginationKind::OffsetLimit {
                offset_key,
                page_size,
                ..
            } => {
                if page.page_len < *page_size as usize {
                    return Ok(false);
                }
                set_query_pair(request.url_mut(), offset_key, &page.total_items.to_string());
                Ok(true)
            }
            PaginationKind::PageNumber {
                page_key,
                page_size,
                first_page,
                ..
            } => {
                if page.page_len < *page_size as usize {
                    return Ok(false);
                }
                // The page number is derived from the walk — the declared first
                // page plus the pages already fetched — never from a provider
                // value, so a provider cannot restart or rewind the walk.
                set_query_pair(
                    request.url_mut(),
                    page_key,
                    &first_page.saturating_add(page.pages).to_string(),
                );
                Ok(true)
            }
            PaginationKind::LinkHeader { rel } => {
                let Some(target) = page
                    .response
                    .headers()
                    .get(reqwest::header::LINK)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|header| link_target(header, rel))
                else {
                    return Ok(false);
                };
                request.set_url(resolve_continuation(&target, origin)?);
                Ok(true)
            }
            PaginationKind::TokenInBody { pointer, query_key } => {
                match text_at(page.body, pointer) {
                    // The token is a query value and only ever a query value,
                    // so a body that spells a URL cannot become a destination.
                    Some(token) => {
                        set_query_pair(request.url_mut(), query_key, &token);
                        Ok(true)
                    }
                    None => Ok(false),
                }
            }
            PaginationKind::NextUriInBody { pointer } => match text_at(page.body, pointer) {
                // Unlike `TokenInBody`, this value *is* a destination, so it
                // goes through the same resolution and the same origin check a
                // `Link` continuation does.
                Some(target) => {
                    request.set_url(resolve_continuation(&target, origin)?);
                    Ok(true)
                }
                None => Ok(false),
            },
        }
    }
}

/// Resolve one provider-offered continuation against the compiled origin.
///
/// A relative continuation resolves against the origin; an absolute one must
/// already be on it. This is the only place in the SDK where a value a provider
/// chose becomes a request destination, which is why the origin check is here
/// rather than at each call site.
fn resolve_continuation(target: &str, origin: &Origin) -> Result<Url, ConnectorFailure> {
    let next = Url::options()
        .base_url(Some(origin.as_url()))
        .parse(target)
        .map_err(|_| {
            ConnectorFailure::validation("connector provider offered an unreadable continuation")
        })?;
    if !origin.contains(&next) {
        return Err(ConnectorFailure::new(
            ConnectorErrorClass::Invariant,
            "connector_pagination_cross_origin",
            "connector provider offered a continuation outside the compiled origin",
        ));
    }
    Ok(next)
}

/// One completed walk: every item the plan collected, and the last page it
/// read.
///
/// The last page is kept rather than discarded because it is what makes the
/// aggregate decodable: [`Pagination::aggregate`] writes the items back into
/// it, and the caller decodes the result against the same status and headers
/// the provider answered its final page with.
#[derive(Debug)]
pub struct Walk {
    items: Vec<JsonValue>,
    document: JsonValue,
    last: RawHttpResponse,
}

impl Walk {
    #[must_use]
    pub fn items(&self) -> &[JsonValue] {
        &self.items
    }

    #[must_use]
    pub fn into_items(self) -> Vec<JsonValue> {
        self.items
    }

    /// The status the provider answered the final page with. It is a declared
    /// success: every page was admitted by the operation's own contract.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.last.status.as_u16()
    }

    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        self.last.headers()
    }
}

/// The page gate of a walk whose caller has no error map to ask — the SDK's own
/// tests, and a connector-module test exercising a plan in isolation.
///
/// A serving executor never uses this: spec 010 §9 makes an operation's own
/// `ErrorMap` the answer for every non-success status, and passing this instead
/// would reintroduce the fallback [`Pagination::collect`] exists to avoid.
pub fn undeclared_status_gate(
    status: u16,
    _headers: &HeaderMap,
    _body: &[u8],
) -> Result<(), ConnectorFailure> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(ConnectorFailure::new(
        ConnectorErrorClass::Permanent,
        "connector_unsupported_http_status",
        "connector provider returned an undeclared HTTP status",
    )
    .with_provider_status(status))
}

/// What one fetched page tells the plan about whether there is another.
struct PageFacts<'a> {
    body: &'a JsonValue,
    response: &'a RawHttpResponse,
    page_len: usize,
    total_items: usize,
    pages: u32,
}

fn positive(page_size: u32) -> Result<u32, OperationError> {
    if page_size == 0 {
        return Err(OperationError::new("a declared page size must be positive"));
    }
    Ok(page_size)
}

/// A non-empty string at a pointer.  An absent, null, or empty value ends the
/// walk, which is how every provider in this set spells "no more pages".
fn text_at(body: &JsonValue, pointer: &str) -> Option<String> {
    match body.pointer(pointer) {
        Some(JsonValue::String(value)) if !value.is_empty() => Some(value.clone()),
        _ => None,
    }
}

/// Replace or append one query parameter, leaving every other parameter byte
/// for byte as it was.  Re-encoding the whole query would rewrite a value an
/// auth plan already applied.
fn set_query_pair(url: &mut Url, key: &str, value: &str) {
    let applied = format!("{key}={}", utf8_percent_encode(value, NON_ALPHANUMERIC));
    let existing = url.query().unwrap_or_default();
    let mut replaced = false;
    let mut parts = existing
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let name = part.split_once('=').map_or(part, |(name, _)| name);
            if name == key {
                replaced = true;
                applied.clone()
            } else {
                part.to_owned()
            }
        })
        .collect::<Vec<_>>();
    if !replaced {
        parts.push(applied);
    }
    url.set_query(Some(&parts.join("&")));
}

/// The target of the first `Link` entry carrying the declared relation.
fn link_target(header: &str, rel: &str) -> Option<String> {
    for entry in header.split(',') {
        let entry = entry.trim();
        let Some(start) = entry.find('<') else {
            continue;
        };
        let Some(length) = entry[start + 1..].find('>') else {
            continue;
        };
        let target = &entry[start + 1..start + 1 + length];
        let carries_relation = entry[start + 1 + length + 1..].split(';').any(|parameter| {
            let Some((name, value)) = parameter.split_once('=') else {
                return false;
            };
            name.trim().eq_ignore_ascii_case("rel")
                && value
                    .trim()
                    .trim_matches('"')
                    .split_whitespace()
                    .any(|token| token.eq_ignore_ascii_case(rel))
        });
        if carries_relation {
            return Some(target.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use reqwest::StatusCode;
    use serde_json::{Value as JsonValue, json};

    use super::*;
    use crate::sdk::auth::{AuthPlan, Credential, Secret, field};
    use crate::sdk::errors::ErrorMap;
    use crate::sdk::operation::Operation;
    use crate::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};

    fn listing() -> Operation {
        Operation::get("item.list", "/v1/items")
            .version("1.0.0")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("static declaration is valid")
    }

    fn generous_budget() -> PaginationBudget {
        PaginationBudget::new(16, 16, 64, 64 * 1024, Duration::from_secs(5))
    }

    /// One page of a two-page listing.
    fn page(items: [i64; 2], next: JsonValue) -> JsonValue {
        json!({ "data": items, "next": next })
    }

    async fn walk(
        stub: &ProviderStub,
        plan: &Pagination,
        budget: &PaginationBudget,
    ) -> Result<Walk, ConnectorFailure> {
        let request = listing()
            .plan_request(&stub.origin(), &json!({}))
            .expect("request renders");
        plan.collect_pages(
            request,
            &stub.origin(),
            budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
    }

    async fn run(
        stub: &ProviderStub,
        plan: &Pagination,
        budget: &PaginationBudget,
    ) -> Result<Vec<JsonValue>, ConnectorFailure> {
        walk(stub, plan, budget).await.map(Walk::into_items)
    }

    /// `sdk_pagination_is_bounded`: every plan stops at calls, pages, items,
    /// aggregate bytes, and deadline; a cross-origin `next` is rejected.
    #[tokio::test]
    async fn sdk_pagination_is_bounded() {
        // Each plan walks its two pages and stops where the provider says so.
        let cursor = Pagination::cursor("/data", "cursor", "/next", "limit", 2)
            .expect("a static cursor plan is valid");
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items")
                .query("limit=2")
                .respond_json(200, page([1, 2], json!("c2"))),
            Expectation::new("GET", "/v1/items")
                .query("limit=2&cursor=c2")
                .respond_json(200, page([3, 4], JsonValue::Null)),
        ])
        .await;
        assert_eq!(
            run(&stub, &cursor, &generous_budget())
                .await
                .expect("the cursor plan walks both pages"),
            vec![json!(1), json!(2), json!(3), json!(4)]
        );
        stub.assert_satisfied();

        let offset_limit = Pagination::offset_limit("/data", "offset", "limit", 2)
            .expect("a static offset plan is valid");
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items")
                .query("offset=0&limit=2")
                .respond_json(200, json!({ "data": [1, 2] })),
            Expectation::new("GET", "/v1/items")
                .query("offset=2&limit=2")
                .respond_json(200, json!({ "data": [3] })),
        ])
        .await;
        assert_eq!(
            run(&stub, &offset_limit, &generous_budget())
                .await
                .expect("a short page ends an offset walk"),
            vec![json!(1), json!(2), json!(3)]
        );
        stub.assert_satisfied();

        let page_number = Pagination::page_number("/data", "page", "per_page", 2)
            .expect("a static page plan is valid");
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items")
                .query("page=1&per_page=2")
                .respond_json(200, json!({ "data": [1, 2] })),
            Expectation::new("GET", "/v1/items")
                .query("page=2&per_page=2")
                .respond_json(200, json!({ "data": [] })),
        ])
        .await;
        assert_eq!(
            run(&stub, &page_number, &generous_budget())
                .await
                .expect("an empty page ends a page-number walk"),
            vec![json!(1), json!(2)]
        );
        stub.assert_satisfied();

        let link_header = Pagination::link_header("/data", "next").expect("a link plan is valid");
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items")
                .respond_header("link", "<{base_url}/v1/items?page=2>; rel=\"next\"")
                .respond_json(200, json!({ "data": [1, 2] })),
            Expectation::new("GET", "/v1/items")
                .query("page=2")
                .respond_header("link", "<{base_url}/v1/items?page=1>; rel=\"prev\"")
                .respond_json(200, json!({ "data": [3] })),
        ])
        .await;
        assert_eq!(
            run(&stub, &link_header, &generous_budget())
                .await
                .expect("the link plan follows one same-origin next"),
            vec![json!(1), json!(2), json!(3)]
        );
        stub.assert_satisfied();

        let token_in_body = Pagination::token_in_body("/data", "/next_token", "page_token")
            .expect("a static token plan is valid");
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items")
                .query("")
                .respond_json(200, json!({ "data": [1, 2], "next_token": "t2" })),
            Expectation::new("GET", "/v1/items")
                .query("page_token=t2")
                .respond_json(200, json!({ "data": [3], "next_token": null })),
        ])
        .await;
        assert_eq!(
            run(&stub, &token_in_body, &generous_budget())
                .await
                .expect("the token plan walks both pages"),
            vec![json!(1), json!(2), json!(3)]
        );
        stub.assert_satisfied();

        // An endless provider: every ceiling stops it, and none of them
        // returns the pages already collected.
        async fn endless(plan: &Pagination, budget: PaginationBudget) -> ConnectorFailure {
            let stub = ProviderStub::start((0..12).map(|index| {
                Expectation::new("GET", "/v1/items")
                    .respond_header("link", "<{base_url}/v1/items?page=9>; rel=\"next\"")
                    .respond_json(
                        200,
                        json!({ "data": [index, index], "next": "more", "next_token": "more" }),
                    )
            }))
            .await;
            run(&stub, plan, &budget)
                .await
                .expect_err("an endless provider exhausts the budget")
        }

        let next_uri_in_body = Pagination::next_uri_in_body("/data", "/next")
            .expect("a static next-URI plan is valid");

        for plan in [
            &cursor,
            &offset_limit,
            &page_number,
            &link_header,
            &token_in_body,
            &next_uri_in_body,
        ] {
            let calls = endless(
                plan,
                PaginationBudget::new(3, 9, 64, 64 * 1024, Duration::from_secs(5)),
            )
            .await;
            assert_eq!(calls.class(), ConnectorErrorClass::Validation);
            assert_eq!(calls.code(), "connector_pagination_budget");

            let pages = endless(
                plan,
                PaginationBudget::new(9, 3, 64, 64 * 1024, Duration::from_secs(5)),
            )
            .await;
            assert_eq!(pages.class(), ConnectorErrorClass::Validation);

            let items = endless(
                plan,
                PaginationBudget::new(9, 9, 5, 64 * 1024, Duration::from_secs(5)),
            )
            .await;
            assert_eq!(items.class(), ConnectorErrorClass::Validation);

            let bytes = endless(
                plan,
                PaginationBudget::new(9, 9, 64, 120, Duration::from_secs(5)),
            )
            .await;
            assert_eq!(bytes.class(), ConnectorErrorClass::Validation);
        }

        // The deadline is the budget every plan shares with its activity.
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items")
                .delay(Duration::from_millis(120))
                .respond_json(200, page([1, 2], json!("c2"))),
            Expectation::new("GET", "/v1/items").respond_json(200, page([3, 4], json!("c3"))),
        ])
        .await;
        let deadline = run(
            &stub,
            &cursor,
            &generous_budget()
                .with_deadline(tokio::time::Instant::now() + Duration::from_millis(60)),
        )
        .await
        .expect_err("the shared deadline stops the walk");
        assert_eq!(deadline.class(), ConnectorErrorClass::Timeout);

        // A `next` on another origin is refused rather than followed.
        let elsewhere = ProviderStub::start([
            Expectation::new("GET", "/v1/items").respond_json(200, json!({ "data": [99] }))
        ])
        .await;
        let stub = ProviderStub::start([Expectation::new("GET", "/v1/items")
            .respond_header(
                "link",
                &format!("<{}/v1/items>; rel=\"next\"", elsewhere.base_url()),
            )
            .respond_json(200, json!({ "data": [1, 2] }))])
        .await;
        let cross_origin = run(&stub, &link_header, &generous_budget())
            .await
            .expect_err("a cross-origin continuation is not followed");
        assert_eq!(cross_origin.class(), ConnectorErrorClass::Invariant);
        assert_eq!(cross_origin.code(), "connector_pagination_cross_origin");
        assert!(
            elsewhere.mismatches().len() == 1,
            "the other origin was never contacted"
        );

        // A token that spells an absolute URL is a query value, never a
        // destination: `TokenInBody` cannot leave the origin at all.
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items").respond_json(
                200,
                json!({ "data": [1], "next_token": "https://attacker.invalid/v1/items" }),
            ),
            Expectation::new("GET", "/v1/items")
                .query("page_token=https%3A%2F%2Fattacker%2Einvalid%2Fv1%2Fitems")
                .respond_json(200, json!({ "data": [2], "next_token": null })),
        ])
        .await;
        assert_eq!(
            run(&stub, &token_in_body, &generous_budget())
                .await
                .expect("the token stays a query value"),
            vec![json!(1), json!(2)]
        );
        stub.assert_satisfied();
    }

    /// A signature is not a header a continuation can inherit.
    ///
    /// AWS SigV4 signs the canonical query string, and a continuation *is* a
    /// different query — so a walk that signed only its first request sends
    /// page two with a signature for page one and earns
    /// `SignatureDoesNotMatch`, which an error map classifies `authentication`
    /// and no Process retries. The walk therefore hands each page's request to
    /// `fetch` by value, derived from the copy it kept, so the caller can
    /// authenticate the request it is actually sending. This proves both
    /// halves: the two pages carry different signatures, and page two's is the
    /// one page two's own URL earns.
    #[tokio::test]
    async fn each_page_of_a_signed_walk_is_signed_over_the_query_that_page_sends() {
        // 2013-05-24T00:00:00Z, the instant AWS's own SigV4 examples sign at.
        let signing_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_369_353_600);

        let credential = Credential::from_fields([
            (field::AWS_ACCESS_KEY_ID, Secret::new("AKIDEXAMPLE")),
            (field::AWS_SECRET_ACCESS_KEY, Secret::new(SECRET_SENTINEL)),
            (field::AWS_REGION, Secret::new("eu-west-1")),
        ]);
        let plan = AuthPlan::aws_sigv4("ses").expect("a static service code is valid");
        let pagination = Pagination::token_in_body("/data", "/next", "NextToken")
            .expect("a static token plan is valid");

        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items")
                .respond_json(200, json!({ "data": [1], "next": "t2" })),
            Expectation::new("GET", "/v1/items")
                .query("NextToken=t2")
                .respond_json(200, json!({ "data": [2] })),
        ])
        .await;

        let first = listing()
            .plan_request(&stub.origin(), &json!({}))
            .expect("request renders");
        let items = pagination
            .collect(
                first,
                &stub.origin(),
                &generous_budget(),
                undeclared_status_gate,
                // Exactly what the serving executor does: authenticate the
                // request being sent, not the request the walk started from.
                |mut request| {
                    let credential = &credential;
                    let plan = &plan;
                    let stub = &stub;
                    async move {
                        plan.apply_at(credential, &mut request, None, signing_time)?;
                        stub.send(request).await
                    }
                },
            )
            .await
            .expect("the signed walk completes");
        assert_eq!(items, vec![json!(1), json!(2)]);
        stub.assert_satisfied();

        let recorded = stub.recorded();
        let signature = |index: usize| {
            recorded[index]
                .header("authorization")
                .expect("every page is signed")
                .to_owned()
        };
        assert!(signature(0).starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"));
        assert_ne!(
            signature(0),
            signature(1),
            "a continuation that inherited page one's signature would be rejected"
        );

        // And page two's signature is the one page two's own URL earns: the
        // same plan, over the same request, at the same instant.
        let mut page_two = listing()
            .plan_request(&stub.origin(), &json!({}))
            .expect("request renders");
        let mut url = stub.origin().as_url().clone();
        url.set_path(&recorded[1].path);
        url.set_query(Some(&recorded[1].query));
        page_two.set_url(url);
        plan.apply_at(&credential, &mut page_two, None, signing_time)
            .expect("the plan signs");
        assert_eq!(
            page_two
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some(signature(1).as_str()),
            "page two's signature verifies against page two's own request"
        );
    }

    /// The deadline stops the *next* fetch.  A walk whose last page already
    /// arrived returns its complete aggregate rather than failing for a
    /// deadline it no longer needs.
    #[tokio::test]
    async fn a_completed_walk_is_not_failed_by_a_deadline_that_elapsed_during_its_last_page() {
        let plan = Pagination::cursor("/data", "cursor", "/next", "limit", 2)
            .expect("a static cursor plan is valid");
        let stub = ProviderStub::start([Expectation::new("GET", "/v1/items")
            .delay(Duration::from_millis(120))
            .respond_json(200, page([1, 2], JsonValue::Null))])
        .await;

        let items = run(
            &stub,
            &plan,
            &generous_budget()
                .with_deadline(tokio::time::Instant::now() + Duration::from_millis(60)),
        )
        .await
        .expect("the aggregate is complete");
        assert_eq!(items, vec![json!(1), json!(2)]);
        stub.assert_satisfied();
    }

    #[tokio::test]
    async fn a_page_that_does_not_hold_the_declared_item_list_is_a_validation_failure() {
        let plan = Pagination::cursor("/data", "cursor", "/next", "limit", 2)
            .expect("a static cursor plan is valid");
        let stub =
            ProviderStub::start([Expectation::new("GET", "/v1/items")
                .respond_json(200, json!({ "data": "not a list" }))])
            .await;
        let failure = run(&stub, &plan, &generous_budget())
            .await
            .expect_err("the declared item list must be a list");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    }

    #[tokio::test]
    async fn a_failed_page_never_yields_the_pages_already_collected() {
        let plan = Pagination::cursor("/data", "cursor", "/next", "limit", 2)
            .expect("a static cursor plan is valid");
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items").respond_json(200, page([1, 2], json!("c2"))),
            Expectation::new("GET", "/v1/items").respond_bytes(500, "provider text must not leak"),
        ])
        .await;
        let failure = run(&stub, &plan, &generous_budget())
            .await
            .expect_err("a failed page fails the aggregate");
        assert!(!failure.safe_message().contains("provider text"));
    }

    /// A credential applied to the first request keeps travelling with every
    /// continuation the plan derives from it.
    #[tokio::test]
    async fn a_continuation_keeps_the_credential_the_first_request_carried() {
        let plan = Pagination::cursor("/data", "cursor", "/next", "limit", 2)
            .expect("a static cursor plan is valid");
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items")
                .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
                .respond_json(200, page([1, 2], json!("c2"))),
            Expectation::new("GET", "/v1/items")
                .query("limit=2&cursor=c2")
                .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
                .respond_json(200, page([3, 4], JsonValue::Null)),
        ])
        .await;

        let mut request = listing()
            .plan_request(&stub.origin(), &json!({}))
            .expect("request renders");
        AuthPlan::bearer()
            .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
            .expect("the credential applies");
        let items = plan
            .collect(
                request,
                &stub.origin(),
                &generous_budget(),
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect("the walk succeeds");
        assert_eq!(items.len(), 4);
        stub.assert_satisfied();
    }

    /// The walk asks the caller whether a page is a failure and classifies
    /// nothing itself.
    ///
    /// This is the gap [[034-a-declaration-the-runtime-ignores-is-a-defect]]
    /// names: the loop used to answer a non-2xx page with one built-in
    /// `permanent` failure, which is not what the operation that declared a
    /// `429` as retryable said about it.
    #[tokio::test]
    async fn a_failing_page_is_classified_by_the_gate_the_caller_supplied() {
        let plan = Pagination::cursor("/data", "cursor", "/next", "limit", 2)
            .expect("a static cursor plan is valid");
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items").respond_json(200, page([1, 2], json!("c2"))),
            Expectation::new("GET", "/v1/items")
                .respond_json(429, json!({ "error": { "type": "RATE_LIMITED" } })),
        ])
        .await;

        let map = ErrorMap::builder(ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            .build()
            .expect("a static error map is valid");
        let request = listing()
            .plan_request(&stub.origin(), &json!({}))
            .expect("request renders");
        let failure = plan
            .collect(
                request,
                &stub.origin(),
                &generous_budget(),
                |status, headers, body| {
                    if listing().is_success(status) {
                        Ok(())
                    } else {
                        Err(map.classify(status, headers, body))
                    }
                },
                |request| stub.send(request),
            )
            .await
            .expect_err("a failing page fails the walk");
        assert_eq!(failure.class(), ConnectorErrorClass::Http429);
        assert_eq!(failure.provider_status(), Some(429));
        stub.assert_satisfied();
    }

    /// A completed walk decodes as one document: the last page with the whole
    /// aggregate where the plan declared the item list.
    #[tokio::test]
    async fn a_completed_walk_aggregates_into_the_last_page_it_read() {
        let plan = Pagination::cursor("/data", "cursor", "/next", "limit", 2)
            .expect("a static cursor plan is valid");
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items")
                .respond_json(200, json!({ "data": [1, 2], "next": "c2", "total": 3 })),
            Expectation::new("GET", "/v1/items")
                .query("limit=2&cursor=c2")
                .respond_json(200, json!({ "data": [3], "total": 3 })),
        ])
        .await;
        let walked = walk(&stub, &plan, &generous_budget())
            .await
            .expect("the walk succeeds");
        assert_eq!(walked.status(), 200);
        assert_eq!(
            plan.aggregate(walked),
            json!({ "data": [1, 2, 3], "total": 3 }),
            "the continuation is absent because the walk reached the end"
        );
        stub.assert_satisfied();

        // A provider whose collection *is* the document aggregates into a bare
        // array, which is the only shape its declaration can read.
        let root = Pagination::link_header("", "next").expect("a static link plan is valid");
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items")
                .respond_header("link", "<{base_url}/v1/items?page=2>; rel=\"next\"")
                .respond_json(200, json!([1, 2])),
            Expectation::new("GET", "/v1/items")
                .query("page=2")
                .respond_json(200, json!([3])),
        ])
        .await;
        let walked = walk(&stub, &root, &generous_budget())
            .await
            .expect("the walk succeeds");
        assert_eq!(root.aggregate(walked), json!([1, 2, 3]));
        stub.assert_satisfied();
    }

    /// The first page number is part of the declaration.
    ///
    /// Typeform documents "page (integer, default: 1)" and Twilio documents its
    /// `page` as zero-indexed. A plan hard-coded to start at 1 walks a
    /// zero-indexed provider from its *second* page and silently drops the
    /// first, which is a wrong answer rather than a failure.
    #[tokio::test]
    async fn the_first_page_number_of_a_page_number_walk_is_declared() {
        // Zero-indexed: the walk starts at 0 and the second request asks for 1.
        let zero_based = Pagination::page_number_from("/data", "Page", "PageSize", 2, 0)
            .expect("a static zero-indexed page plan is valid");
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items")
                .query("Page=0&PageSize=2")
                .respond_json(200, json!({ "data": [1, 2] })),
            Expectation::new("GET", "/v1/items")
                .query("Page=1&PageSize=2")
                .respond_json(200, json!({ "data": [3] })),
        ])
        .await;
        assert_eq!(
            run(&stub, &zero_based, &generous_budget())
                .await
                .expect("a zero-indexed walk starts at its documented first page"),
            vec![json!(1), json!(2), json!(3)]
        );
        stub.assert_satisfied();

        // One-based stays exactly what it was: `page_number` is the same plan
        // with the first page Typeform documents.
        assert_eq!(
            Pagination::page_number("/items", "page", "page_size", 200)
                .expect("a static page plan is valid"),
            Pagination::page_number_from("/items", "page", "page_size", 200, 1)
                .expect("a static page plan is valid"),
        );
        let one_based = Pagination::page_number("/data", "page", "per_page", 2)
            .expect("a static page plan is valid");
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items")
                .query("page=1&per_page=2")
                .respond_json(200, json!({ "data": [1, 2] })),
            Expectation::new("GET", "/v1/items")
                .query("page=2&per_page=2")
                .respond_json(200, json!({ "data": [3] })),
        ])
        .await;
        assert_eq!(
            run(&stub, &one_based, &generous_budget())
                .await
                .expect("a one-based walk is unchanged"),
            vec![json!(1), json!(2), json!(3)]
        );
        stub.assert_satisfied();

        assert!(Pagination::page_number_from("/data", "{page}", "per_page", 2, 0).is_err());
        assert!(Pagination::page_number_from("/data", "page", "per_page", 0, 0).is_err());
    }

    /// `sdk_pagination_is_bounded`, for the plan SendGrid and Twilio publish:
    /// the continuation is a URI in the response body, it is resolved against
    /// the compiled origin exactly as a `Link` header is, and a continuation
    /// pointing anywhere else is rejected rather than followed.
    #[tokio::test]
    async fn a_body_carried_continuation_uri_is_resolved_and_bounded_to_the_compiled_origin() {
        let plan = Pagination::next_uri_in_body("/data", "/next")
            .expect("a static next-URI plan is valid");

        // Twilio publishes a relative `next_page_uri`; it resolves against the
        // compiled origin, keeping every query parameter the provider chose.
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items")
                .query("")
                .respond_json(200, json!({ "data": [1, 2], "next": "/v1/items?Page=1" })),
            Expectation::new("GET", "/v1/items")
                .query("Page=1")
                .respond_json(200, json!({ "data": [3], "next": JsonValue::Null })),
        ])
        .await;
        assert_eq!(
            run(&stub, &plan, &generous_budget())
                .await
                .expect("a relative continuation resolves against the origin"),
            vec![json!(1), json!(2), json!(3)]
        );
        stub.assert_satisfied();

        // SendGrid publishes an absolute `_metadata.next`; it is followed only
        // because it is already on the compiled origin.
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/items").respond_json(
                200,
                json!({ "data": [1], "next": "{base_url}/v1/items?page_token=t2" }),
            ),
            Expectation::new("GET", "/v1/items")
                .query("page_token=t2")
                .respond_json(200, json!({ "data": [2] })),
        ])
        .await;
        assert_eq!(
            run(&stub, &plan, &generous_budget())
                .await
                .expect("an absolute same-origin continuation is followed"),
            vec![json!(1), json!(2)]
        );
        stub.assert_satisfied();

        // A continuation on another origin is refused, and that origin is never
        // contacted.
        let elsewhere = ProviderStub::start([
            Expectation::new("GET", "/v1/items").respond_json(200, json!({ "data": [99] }))
        ])
        .await;
        let stub = ProviderStub::start([Expectation::new("GET", "/v1/items").respond_json(
            200,
            json!({ "data": [1], "next": format!("{}/v1/items", elsewhere.base_url()) }),
        )])
        .await;
        let cross_origin = run(&stub, &plan, &generous_budget())
            .await
            .expect_err("a cross-origin continuation is not followed");
        assert_eq!(cross_origin.class(), ConnectorErrorClass::Invariant);
        assert_eq!(cross_origin.code(), "connector_pagination_cross_origin");
        assert_eq!(
            elsewhere.mismatches().len(),
            1,
            "the other origin was never contacted"
        );

        // Neither is a continuation that is not a URI at all, nor one that
        // carries userinfo or a scheme the origin does not use.
        for next in [
            "http://\u{7f}",
            "https://user:pass@attacker.invalid/v1/items",
            "ftp://provider.example.test/v1/items",
        ] {
            let stub = ProviderStub::start([Expectation::new("GET", "/v1/items")
                .respond_json(200, json!({ "data": [1], "next": next }))])
            .await;
            let failure = run(&stub, &plan, &generous_budget())
                .await
                .expect_err("an unusable continuation ends the attempt");
            assert!(
                matches!(
                    failure.class(),
                    ConnectorErrorClass::Invariant | ConnectorErrorClass::Validation
                ),
                "{next}"
            );
        }
    }

    #[tokio::test]
    async fn a_pagination_plan_declaration_is_static() {
        assert!(Pagination::cursor("data", "cursor", "/next", "limit", 2).is_err());
        assert!(Pagination::cursor("/data", "{cursor}", "/next", "limit", 2).is_err());
        assert!(Pagination::cursor("/data", "cursor", "next", "limit", 2).is_err());
        assert!(Pagination::cursor("/data", "cursor", "/next", "limit", 0).is_err());
        assert!(Pagination::offset_limit("/data", "offset", "{limit}", 2).is_err());
        assert!(Pagination::page_number("/data", "{page}", "per_page", 2).is_err());
        assert!(Pagination::link_header("/data", "").is_err());
        assert!(Pagination::token_in_body("/data", "next", "page_token").is_err());
        assert!(Pagination::next_uri_in_body("/data", "next").is_err());
        assert!(Pagination::next_uri_in_body("data", "/next").is_err());
        assert!(Pagination::next_uri_in_body("/data", "/{next}").is_err());
    }
}
