//! The hand-written connector modules (spec 010 §3).
//!
//! One module per provider. A module is a static [`crate::sdk::Connector`]
//! declaration plus the deploy-time configuration type its instances compile
//! from; nothing here opens a request path a caller can aim.

pub mod airtable;
pub mod aws;
pub mod aws_s3;
pub mod aws_ses;
pub mod aws_sqs;
pub mod calendly;
pub mod github;
pub mod google;
pub mod google_calendar;
pub mod google_drive;
pub mod google_gmail;
pub mod google_sheets;
pub mod hubspot;
pub mod inbound;
pub mod intercom;
pub mod jira;
pub mod linear;
pub mod microsoft_excel;
pub mod microsoft_graph;
pub mod microsoft_onedrive;
pub mod microsoft_outlook;
pub mod microsoft_teams;
pub mod notion;
pub mod openai;
// Batch J: payments and billing (spec 026).
pub mod mercado_pago;
pub mod paddle;
// The first connector in the programme whose credential is an OAuth2
// client-credentials token the executor mints per attempt.
pub mod paypal;
pub mod postmark;
pub mod sendgrid;
pub mod sentry;
pub mod shopify;
pub mod slack;
pub mod telegram;
pub mod twilio;
pub mod typeform;
pub mod xero;
// Batch G: CRM and helpdesk (spec 023).
pub mod freshdesk;
pub mod pipedrive;
pub mod salesforce;
pub mod woocommerce;
pub mod zendesk;
pub mod zoho_crm;
// Batch H: project tracking and collaboration (spec 024).
pub mod asana;
pub mod basecamp;
pub mod clickup;
pub mod monday;
pub mod todoist;
pub mod trello;
// Batch I: storage and messaging (spec 025).
//
// `box_platform` is the Rust module name of the connector a deployment selects
// as `box`: `box` is a reserved word in this language, and Box's own name for
// the product is "Box Platform".
//
// `dropbox_content` is the second half of one provider rather than a second
// provider: Dropbox serves metadata from `api.dropboxapi.com` and content from
// `content.dropboxapi.com`, and a connector has one compiled origin
// ([[074-a-second-origin-is-a-second-connector-and-a-download-is-composed-under-its-bound]]).
pub mod box_platform;
pub mod discord;
pub mod dropbox;
pub mod dropbox_content;
pub mod mailchimp;
pub mod mattermost;
pub mod zoom;
// Batch K: development and monitoring (spec 027).
//
// `gitlab` and `grafana` are the deployment's own instances rather than a
// vendor's tenants, so each names a whole origin
// ([[082-an-instance-a-deployment-operates-is-a-whole-origin-it-names]]).
//
// `pagerduty` is the only connector in the workspace whose credential is an
// `Authorization` authentication *parameter* rather than a bare token
// ([[081-a-credential-is-an-authentication-parameter-and-a-body-credential-is-a-version-that-was-superseded]]).
pub mod bitbucket;
pub mod cloudflare;
pub mod gitlab;
pub mod grafana;
pub mod pagerduty;
pub mod uptimerobot;

// Batch L, the scheduling and people half (spec 028).
//
// `harvest` sends two credential-adjacent values and only one of them is a
// secret: a Personal Access Token in `Authorization: Bearer`, and a non-secret
// account identifier in a `Harvest-Account-Id` header its declaration compiles
// per deployment.
pub mod harvest;
// `bamboohr` publishes exactly one credential wire form and it is the one
// [[064-a-credentials-scheme-and-its-username-are-the-providers]] added the
// plan for: the API key is the HTTP Basic *username* and the password is a
// constant nobody chooses.
pub mod bamboohr;
// `clockify` scopes almost every endpoint to a workspace it puts in the *path*,
// so its declaration compiles the workspace in exactly as Basecamp's account is
// ([[066-a-credential-can-be-two-query-parameters-and-an-account-is-a-compiled-path-prefix]]).
pub mod clockify;
// `eventbrite` publishes an opaque continuation token in the response body and
// spends it as a query value, which is the one plan in the SDK's closed set that
// can never turn a provider value into a request destination.
pub mod eventbrite;

// Batch L, the forms half (spec 028).
//
// `jotform` serves one account from one of three published API regions, and two
// of the three spell a prefix in front of `api` rather than a label under a
// constant suffix, so its origin comes from a closed compiled table rather than
// from a templated host
// ([[065-an-origin-is-a-label-a-table-or-a-whole-value-a-deployment-names]]).
pub mod jotform;
//
// `surveymonkey` declares the global datacentre's origin only: the EU and
// Canadian hosts are a second and third connector, and the `access_url` its own
// token exchange returns is a provider-chosen origin this connector never reads
// ([[074-a-second-origin-is-a-second-connector-and-a-download-is-composed-under-its-bound]]).
pub mod surveymonkey;
//
// `cal_com` pins its API version per *operation*: `cal-api-version` is required
// on every v2 endpoint and its value differs between the booking collection,
// the booking read and write, and the event types.
pub mod cal_com;
//
// `acuity` sends two deploy-time values and only one of them is a secret: the
// account's numeric User ID is its HTTP Basic *username*, which the declaration
// compiles per deployment, and the API key is the password
// ([[064-a-credentials-scheme-and-its-username-are-the-providers]]).
pub mod acuity;
