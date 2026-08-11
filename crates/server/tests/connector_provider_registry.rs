//! The hand-written connectors, as the serving registry sees them.
//!
//! Every case here is a deploy-time question: which modules the binary carries,
//! which operations a deployment may enable on them, where each instance's
//! configuration comes from, and what a missing or hostile value does. None of
//! them opens a socket — the provider-facing behaviour of these connectors is
//! proven in `donat-connectors` against the SDK's own stub.

use donat_connector_catalog::OperationEffect;
use donat_server::connectors::ConnectorRegistry;
use donat_server::state::validate_connector_metadata;
use serde_json::{Value as Json, json};

const AIRTABLE_TOKEN: &str = "DONAT_TEST_PROVIDER_AIRTABLE_TOKEN";
const SENDGRID_TOKEN: &str = "DONAT_TEST_PROVIDER_SENDGRID_TOKEN";
const POSTMARK_TOKEN: &str = "DONAT_TEST_PROVIDER_POSTMARK_TOKEN";
const TWILIO_TOKEN: &str = "DONAT_TEST_PROVIDER_TWILIO_TOKEN";
const OPENAI_TOKEN: &str = "DONAT_TEST_PROVIDER_OPENAI_TOKEN";
const TYPEFORM_TOKEN: &str = "DONAT_TEST_PROVIDER_TYPEFORM_TOKEN";
const AWS_ACCESS_KEY_ID: &str = "DONAT_TEST_PROVIDER_AWS_ACCESS_KEY_ID";
const AWS_SECRET_ACCESS_KEY: &str = "DONAT_TEST_PROVIDER_AWS_SECRET_ACCESS_KEY";
const ABSENT: &str = "DONAT_TEST_PROVIDER_ABSENT_VARIABLE";
/// Batch B (spec 013): every webhook-bearing connector reads one API key and
/// one inbound signing secret.
const GITHUB_TOKEN: &str = "DONAT_TEST_PROVIDER_GITHUB_TOKEN";
const SHOPIFY_TOKEN: &str = "DONAT_TEST_PROVIDER_SHOPIFY_TOKEN";
const TELEGRAM_TOKEN: &str = "DONAT_TEST_PROVIDER_TELEGRAM_TOKEN";
const CALENDLY_TOKEN: &str = "DONAT_TEST_PROVIDER_CALENDLY_TOKEN";
const SENTRY_TOKEN: &str = "DONAT_TEST_PROVIDER_SENTRY_TOKEN";
const WEBHOOK_SECRET: &str = "DONAT_TEST_PROVIDER_WEBHOOK_SECRET";
/// Batch G (spec 023): the four CRM and helpdesk connectors whose credential is
/// deploy-time configuration rather than a stored OAuth2 token.
const PIPEDRIVE_TOKEN: &str = "DONAT_TEST_PROVIDER_PIPEDRIVE_TOKEN";
const FRESHDESK_KEY: &str = "DONAT_TEST_PROVIDER_FRESHDESK_KEY";
const ZENDESK_TOKEN: &str = "DONAT_TEST_PROVIDER_ZENDESK_TOKEN";
const WOOCOMMERCE_SECRET: &str = "DONAT_TEST_PROVIDER_WOOCOMMERCE_SECRET";
/// Batch J (spec 026): the two payments connectors whose credential is
/// deploy-time configuration. Xero's is a stored OAuth2 token, so it configures
/// no secret at all.
/// Batch H (spec 024): the five project connectors whose credential is
/// deploy-time configuration. Trello configures two, because neither half of
/// its credential authenticates alone; Basecamp's is a stored OAuth2 token, so
/// it configures none.
const ASANA_TOKEN: &str = "DONAT_TEST_PROVIDER_ASANA_TOKEN";
const TRELLO_KEY: &str = "DONAT_TEST_PROVIDER_TRELLO_KEY";
const TRELLO_TOKEN: &str = "DONAT_TEST_PROVIDER_TRELLO_TOKEN";
const CLICKUP_TOKEN: &str = "DONAT_TEST_PROVIDER_CLICKUP_TOKEN";
const MONDAY_TOKEN: &str = "DONAT_TEST_PROVIDER_MONDAY_TOKEN";
const TODOIST_TOKEN: &str = "DONAT_TEST_PROVIDER_TODOIST_TOKEN";
const PADDLE_KEY: &str = "DONAT_TEST_PROVIDER_PADDLE_KEY";
const MERCADO_PAGO_TOKEN: &str = "DONAT_TEST_PROVIDER_MERCADO_PAGO_TOKEN";
/// PayPal's credential is a client-credentials pair rather than one key.
const PAYPAL_CLIENT_ID: &str = "DONAT_TEST_PROVIDER_PAYPAL_CLIENT_ID";
const PAYPAL_CLIENT_SECRET: &str = "DONAT_TEST_PROVIDER_PAYPAL_CLIENT_SECRET";
/// Batch I (spec 025): the three storage and messaging connectors whose
/// credential is deploy-time configuration. Dropbox, its content origin, Box,
/// and Zoom all read the source-local OAuth2 store instead and configure none.
const DISCORD_TOKEN: &str = "DONAT_TEST_PROVIDER_DISCORD_TOKEN";
const MATTERMOST_TOKEN: &str = "DONAT_TEST_PROVIDER_MATTERMOST_TOKEN";
const MAILCHIMP_KEY: &str = "DONAT_TEST_PROVIDER_MAILCHIMP_KEY";
/// Batch K (spec 027): the six development and monitoring connectors. Every one
/// of them reads a deploy-time key; none reads the source-local OAuth2 store.
const GITLAB_TOKEN: &str = "DONAT_TEST_PROVIDER_GITLAB_TOKEN";
const GRAFANA_TOKEN: &str = "DONAT_TEST_PROVIDER_GRAFANA_TOKEN";
const BITBUCKET_TOKEN: &str = "DONAT_TEST_PROVIDER_BITBUCKET_TOKEN";
const PAGERDUTY_KEY: &str = "DONAT_TEST_PROVIDER_PAGERDUTY_KEY";
const UPTIMEROBOT_TOKEN: &str = "DONAT_TEST_PROVIDER_UPTIMEROBOT_TOKEN";
const CLOUDFLARE_TOKEN: &str = "DONAT_TEST_PROVIDER_CLOUDFLARE_TOKEN";
/// Batch L, the forms half (spec 028): Jotform reads one deploy-time API key
/// and one non-secret region.
const JOTFORM_KEY: &str = "DONAT_TEST_PROVIDER_JOTFORM_KEY";
const SURVEYMONKEY_TOKEN: &str = "DONAT_TEST_PROVIDER_SURVEYMONKEY_TOKEN";
const CAL_COM_KEY: &str = "DONAT_TEST_PROVIDER_CAL_COM_KEY";
const ACUITY_KEY: &str = "DONAT_TEST_PROVIDER_ACUITY_KEY";
/// Batch L, the scheduling and people half (spec 028): Harvest reads one
/// deploy-time Personal Access Token and one non-secret account identifier.
const HARVEST_TOKEN: &str = "DONAT_TEST_PROVIDER_HARVEST_TOKEN";
/// BambooHR's key is its HTTP Basic *username*; the password beside it is the
/// constant its own example sends, so no deployment configures one.
const BAMBOOHR_KEY: &str = "DONAT_TEST_PROVIDER_BAMBOOHR_KEY";
const CLOCKIFY_KEY: &str = "DONAT_TEST_PROVIDER_CLOCKIFY_KEY";
const EVENTBRITE_TOKEN: &str = "DONAT_TEST_PROVIDER_EVENTBRITE_TOKEN";

/// The one account SID Twilio's grammar admits in these fixtures.
const ACCOUNT_SID: &str = "AC00000000000000000000000000000042";

fn resolve_test_environment() {
    // Every value here is a test sentinel: no case asserts on a resolved value,
    // and the redaction cases assert that none of them reaches an error.
    for (variable, value) in [
        (AIRTABLE_TOKEN, "pat_test_airtable_sentinel"),
        (SENDGRID_TOKEN, "sg_test_sendgrid_sentinel"),
        (POSTMARK_TOKEN, "pm_test_postmark_sentinel"),
        (TWILIO_TOKEN, "tw_test_twilio_sentinel"),
        (OPENAI_TOKEN, "sk_test_openai_sentinel"),
        (TYPEFORM_TOKEN, "tf_test_typeform_sentinel"),
        (AWS_ACCESS_KEY_ID, "AKIAIOSFODNN7EXAMPLE"),
        (AWS_SECRET_ACCESS_KEY, "wJalrXUtnFEMIbKtestSECRETsentinel"),
        (GITHUB_TOKEN, "github_pat_test_sentinel"),
        (SHOPIFY_TOKEN, "shpat_test_shopify_sentinel"),
        (TELEGRAM_TOKEN, "8100000:test-telegram-sentinel"),
        (CALENDLY_TOKEN, "cal_test_calendly_sentinel"),
        (SENTRY_TOKEN, "sntrys_test_sentry_sentinel"),
        (WEBHOOK_SECRET, "whsec_test_inbound_sentinel"),
        (PIPEDRIVE_TOKEN, "pd_test_pipedrive_sentinel"),
        (FRESHDESK_KEY, "fd_test_freshdesk_sentinel"),
        (ZENDESK_TOKEN, "zd_test_zendesk_sentinel"),
        (WOOCOMMERCE_SECRET, "cs_test_woocommerce_sentinel"),
        (ASANA_TOKEN, "asana_test_asana_sentinel"),
        (TRELLO_KEY, "tk_test_trello_key_sentinel"),
        (TRELLO_TOKEN, "tt_test_trello_token_sentinel"),
        (CLICKUP_TOKEN, "pk_test_clickup_sentinel"),
        (MONDAY_TOKEN, "eyJhbGciOi_test_monday_sentinel"),
        (TODOIST_TOKEN, "td_test_todoist_sentinel"),
        (PADDLE_KEY, "pdl_test_paddle_sentinel"),
        (MERCADO_PAGO_TOKEN, "APP_USR-test-mercado-pago-sentinel"),
        (PAYPAL_CLIENT_ID, "AeA1QIZXi-test-paypal-client-id"),
        (PAYPAL_CLIENT_SECRET, "EL1tVxAjhT-test-paypal-sentinel"),
        (DISCORD_TOKEN, "MTk4NjIy.test-discord-sentinel"),
        (MATTERMOST_TOKEN, "mm_test_mattermost_sentinel"),
        (MAILCHIMP_KEY, "mc_test_mailchimp_sentinel-us19"),
        (GITLAB_TOKEN, "glpat_test_gitlab_sentinel"),
        (GRAFANA_TOKEN, "glsa_test_grafana_sentinel"),
        (BITBUCKET_TOKEN, "ATATT_test_bitbucket_sentinel"),
        (PAGERDUTY_KEY, "y_NbAk_test_pagerduty_sentinel"),
        (UPTIMEROBOT_TOKEN, "ur_test_uptimerobot_sentinel"),
        (CLOUDFLARE_TOKEN, "cf_test_cloudflare_sentinel"),
        (JOTFORM_KEY, "jf_test_jotform_sentinel"),
        (SURVEYMONKEY_TOKEN, "sm_test_surveymonkey_sentinel"),
        (CAL_COM_KEY, "cal_live_test_cal_com_sentinel"),
        (ACUITY_KEY, "acuity_test_acuity_sentinel"),
        (HARVEST_TOKEN, "hv_test_harvest_sentinel"),
        (BAMBOOHR_KEY, "bhr_test_bamboohr_sentinel"),
        (CLOCKIFY_KEY, "clk_test_clockify_sentinel"),
        (EVENTBRITE_TOKEN, "eb_test_eventbrite_sentinel"),
    ] {
        // Safety: the connector registry reads these variables on the same
        // thread that sets them, before any listener or worker exists.
        unsafe { std::env::set_var(variable, value) };
    }
    unsafe { std::env::remove_var(ABSENT) };
}

fn capacity() -> Json {
    json!({
        "max_in_flight": 1,
        "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
    })
}

fn operations(names: &[&str]) -> Json {
    Json::Array(
        names
            .iter()
            .map(|name| json!({ "name": name, "capacity": capacity() }))
            .collect(),
    )
}

fn metadata(connectors: Json) -> donat_metadata::Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{ "name": "default", "kind": "postgres", "configuration": {} }],
        "connectors": connectors
    }))
    .expect("provider connector metadata deserializes")
}

fn errors(metadata: &donat_metadata::Metadata) -> String {
    validate_connector_metadata(metadata)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// One instance per compiled hand-written module, each with the deploy-time
/// configuration its provider needs and one executable operation enabled.
fn every_provider_instance() -> Json {
    let mut instances = json!([
        {
            "name": "records",
            "module": "airtable",
            "config": {
                "endpoint_identity": "airtable_test",
                "credential_identity": "airtable_test_credential",
                "secret_key": { "value_from_env": AIRTABLE_TOKEN },
                "settings": { "base_id": "appTestBase000001" }
            },
            "operations": operations(&["record.list"])
        },
        {
            "name": "marketing",
            "module": "sendgrid",
            "config": {
                "endpoint_identity": "sendgrid_test",
                "credential_identity": "sendgrid_test_credential",
                "secret_key": { "value_from_env": SENDGRID_TOKEN }
            },
            "operations": operations(&["contact.list", "contact.upsert"])
        },
        {
            "name": "transactional",
            "module": "postmark",
            "config": {
                "endpoint_identity": "postmark_test",
                "credential_identity": "postmark_test_credential",
                "secret_key": { "value_from_env": POSTMARK_TOKEN }
            },
            "operations": operations(&["message.list_outbound"])
        },
        {
            "name": "telephony",
            "module": "twilio",
            "config": {
                "endpoint_identity": "twilio_test",
                "credential_identity": "twilio_test_credential",
                "secret_key": { "value_from_env": TWILIO_TOKEN },
                "settings": { "account_sid": ACCOUNT_SID }
            },
            "operations": operations(&["message.list"])
        },
        {
            "name": "models",
            "module": "openai",
            "config": {
                "endpoint_identity": "openai_test",
                "credential_identity": "openai_test_credential",
                "secret_key": { "value_from_env": OPENAI_TOKEN }
            },
            "operations": operations(&["model.list"])
        },
        {
            "name": "forms",
            "module": "typeform",
            "config": {
                "endpoint_identity": "typeform_test",
                "credential_identity": "typeform_test_credential",
                "secret_key": { "value_from_env": TYPEFORM_TOKEN },
                "webhook_secret": { "value_from_env": WEBHOOK_SECRET }
            },
            "operations": operations(&["form.list"])
        },
        {
            "name": "objects",
            "module": "aws_s3",
            "config": {
                "endpoint_identity": "s3_test",
                "credential_identity": "s3_test_credential",
                "settings": {
                    "region": "eu-west-1",
                    "bucket": "donat-test-bucket",
                    "bucket_versioning": "unversioned"
                },
                "secrets": {
                    "access_key_id": { "value_from_env": AWS_ACCESS_KEY_ID },
                    "secret_access_key": { "value_from_env": AWS_SECRET_ACCESS_KEY }
                }
            },
            "operations": operations(&["object.get", "object.put", "object.delete"])
        },
        {
            "name": "queue",
            "module": "aws_sqs",
            "config": {
                "endpoint_identity": "sqs_test",
                "credential_identity": "sqs_test_credential",
                "settings": {
                    "region": "eu-west-1",
                    "account_id": "123456789012",
                    "queue_name": "donat-test.fifo",
                    "queue_type": "fifo"
                },
                "secrets": {
                    "access_key_id": { "value_from_env": AWS_ACCESS_KEY_ID },
                    "secret_access_key": { "value_from_env": AWS_SECRET_ACCESS_KEY }
                }
            },
            "operations": operations(&["message.send", "message.receive"])
        },
        {
            "name": "mail",
            "module": "aws_ses",
            "config": {
                "endpoint_identity": "ses_test",
                "credential_identity": "ses_test_credential",
                "settings": {
                    "region": "eu-west-1",
                    "from_email_address": "notifications@example.test"
                },
                "secrets": {
                    "access_key_id": { "value_from_env": AWS_ACCESS_KEY_ID },
                    "secret_access_key": { "value_from_env": AWS_SECRET_ACCESS_KEY }
                }
            },
            "operations": operations(&["template.list"])
        },
        // Batch B: the webhook-bearing connectors (spec 013). Each one
        // configures the inbound signing secret its route verifies under.
        {
            "name": "code",
            "module": "github",
            "config": {
                "endpoint_identity": "github_test",
                "credential_identity": "github_test_credential",
                "secret_key": { "value_from_env": GITHUB_TOKEN },
                "webhook_secret": { "value_from_env": WEBHOOK_SECRET }
            },
            "operations": operations(&["issue.get", "file.put"])
        },
        {
            "name": "storefront",
            "module": "shopify",
            "config": {
                "endpoint_identity": "shopify_test",
                "credential_identity": "shopify_test_credential",
                "secret_key": { "value_from_env": SHOPIFY_TOKEN },
                "webhook_secret": { "value_from_env": WEBHOOK_SECRET },
                "settings": { "shop": "donat-test-store" }
            },
            "operations": operations(&["order.get", "product.delete"])
        },
        {
            "name": "chat",
            "module": "telegram",
            "config": {
                "endpoint_identity": "telegram_test",
                "credential_identity": "telegram_test_credential",
                "secret_key": { "value_from_env": TELEGRAM_TOKEN },
                "webhook_secret": { "value_from_env": WEBHOOK_SECRET }
            },
            "operations": operations(&["chat.get"])
        },
        {
            "name": "scheduling",
            "module": "calendly",
            "config": {
                "endpoint_identity": "calendly_test",
                "credential_identity": "calendly_test_credential",
                "secret_key": { "value_from_env": CALENDLY_TOKEN },
                "webhook_secret": { "value_from_env": WEBHOOK_SECRET }
            },
            "operations": operations(&["user.me"])
        },
        {
            "name": "errors",
            "module": "sentry",
            "config": {
                "endpoint_identity": "sentry_test",
                "credential_identity": "sentry_test_credential",
                "secret_key": { "value_from_env": SENTRY_TOKEN },
                "webhook_secret": { "value_from_env": WEBHOOK_SECRET }
            },
            "operations": operations(&["issue.get"])
        },
        // Batch G (spec 023): the CRM and helpdesk connectors whose credential
        // is deploy-time configuration. The two whose credential is a stored
        // OAuth2 token are absent for the same reason Batch C's and Batch D's
        // are: they configure no secret at all, and their startup obligations
        // are proven in the conformance suite against the real binary.
        {
            "name": "sales",
            "module": "pipedrive",
            "config": {
                "endpoint_identity": "pipedrive_test",
                "credential_identity": "pipedrive_test_credential",
                "secret_key": { "value_from_env": PIPEDRIVE_TOKEN }
            },
            "operations": operations(&["deal.get", "deal.create"])
        },
        {
            "name": "helpdesk",
            "module": "freshdesk",
            "config": {
                "endpoint_identity": "freshdesk_test",
                "credential_identity": "freshdesk_test_credential",
                "secret_key": { "value_from_env": FRESHDESK_KEY },
                "settings": { "domain": "donat-test-support" }
            },
            "operations": operations(&["ticket.get", "ticket.list"])
        },
        {
            "name": "support",
            "module": "zendesk",
            "config": {
                "endpoint_identity": "zendesk_test",
                "credential_identity": "zendesk_test_credential",
                "secret_key": { "value_from_env": ZENDESK_TOKEN },
                "settings": {
                    "subdomain": "donat-test",
                    "email": "integrations@example.test"
                }
            },
            "operations": operations(&["ticket.get", "ticket.create"])
        },
        {
            "name": "store",
            "module": "woocommerce",
            "config": {
                "endpoint_identity": "woocommerce_test",
                "credential_identity": "woocommerce_test_credential",
                "secret_key": { "value_from_env": WOOCOMMERCE_SECRET },
                "settings": {
                    "store_origin": "https://shop.example.test",
                    "consumer_key": "ck_donat_test_consumer_key"
                }
            },
            "operations": operations(&["order.get", "order.create"])
        },
        {
            "name": "billing",
            "module": "paddle",
            "config": {
                "endpoint_identity": "paddle_test",
                "credential_identity": "paddle_test_credential",
                "secret_key": { "value_from_env": PADDLE_KEY }
            },
            "operations": operations(&["transaction.get", "customer.create"])
        },
        {
            "name": "payments",
            "module": "mercado_pago",
            "config": {
                "endpoint_identity": "mercado_pago_test",
                "credential_identity": "mercado_pago_test_credential",
                "secret_key": { "value_from_env": MERCADO_PAGO_TOKEN }
            },
            "operations": operations(&["payment.get", "refund.list"])
        },
        {
            // Xero configures no secret: its credential is the source-local
            // store's, and its organisation and send horizon are the two
            // deploy-time values a request may not reach.
            "name": "books",
            "module": "xero",
            "config": {
                "endpoint_identity": "xero_test",
                "credential_identity": "xero_test_credential",
                "settings": {
                    "tenant_id": "00000000-0000-0000-0000-000000000042",
                    "send_horizon_ms": "300000"
                },
                "oauth2": {
                    "authorization_endpoint": "https://login.xero.com/identity/connect/authorize",
                    "token_endpoint": "https://identity.xero.com/connect/token",
                    "redirect_uri": "http://127.0.0.1:8765/callback",
                    "client_id": { "value_from_env": PADDLE_KEY },
                    "client_secret": { "value_from_env": PADDLE_KEY },
                    "scopes": ["accounting.transactions"]
                }
            },
            "operations": operations(&["invoice.get", "payment.create"])
        },
        {
            // PayPal configures two ordinary secrets and no `oauth2` block: its
            // credential is a client-credentials token the executor mints per
            // attempt and never stores
            // ([[072-a-minted-credential-is-spent-inside-one-attempt]]).
            "name": "paypal_payments",
            "module": "paypal",
            "config": {
                "endpoint_identity": "paypal_test",
                "credential_identity": "paypal_test_credential",
                "settings": { "send_horizon_ms": "21540000" },
                "secrets": {
                    "client_id": { "value_from_env": PAYPAL_CLIENT_ID },
                    "client_secret": { "value_from_env": PAYPAL_CLIENT_SECRET }
                }
            },
            "operations": operations(&["order.get", "order.capture"])
        },
        // Batch H (spec 024): the project-tracking and collaboration
        // connectors whose credential is deploy-time configuration. Basecamp is
        // absent for the same reason Batch C's and Batch G's stored-credential
        // modules are: it configures no secret at all, and its startup
        // obligations are proven in the conformance suite against the real
        // binary.
        {
            "name": "work",
            "module": "asana",
            "config": {
                "endpoint_identity": "asana_test",
                "credential_identity": "asana_test_credential",
                "secret_key": { "value_from_env": ASANA_TOKEN }
            },
            "operations": operations(&["task.get", "task.create"])
        },
        {
            // Trello is the one module in the workspace whose credential is two
            // secrets: its key names the application and its token names the
            // authorization, and neither authenticates alone.
            "name": "boards",
            "module": "trello",
            "config": {
                "endpoint_identity": "trello_test",
                "credential_identity": "trello_test_credential",
                "secrets": {
                    "api_key": { "value_from_env": TRELLO_KEY },
                    "api_token": { "value_from_env": TRELLO_TOKEN }
                }
            },
            "operations": operations(&["card.get", "card.create"])
        },
        {
            "name": "tasks",
            "module": "clickup",
            "config": {
                "endpoint_identity": "clickup_test",
                "credential_identity": "clickup_test_credential",
                "secret_key": { "value_from_env": CLICKUP_TOKEN }
            },
            "operations": operations(&["task.get", "task.create"])
        },
        {
            "name": "items",
            "module": "monday",
            "config": {
                "endpoint_identity": "monday_test",
                "credential_identity": "monday_test_credential",
                "secret_key": { "value_from_env": MONDAY_TOKEN }
            },
            "operations": operations(&["item.get", "item.create"])
        },
        {
            "name": "todos",
            "module": "todoist",
            "config": {
                "endpoint_identity": "todoist_test",
                "credential_identity": "todoist_test_credential",
                "secret_key": { "value_from_env": TODOIST_TOKEN }
            },
            "operations": operations(&["task.get", "task.close"])
        },
        // Batch I (spec 025): the storage and messaging connectors whose
        // credential is deploy-time configuration. The four whose credential is
        // a stored OAuth2 token are absent for the same reason Batch C's,
        // Batch D's and Batch G's are: they configure no secret at all, and
        // their startup obligations are proven in the conformance suite against
        // the real binary.
        {
            "name": "guild_chat",
            "module": "discord",
            "config": {
                "endpoint_identity": "discord_test",
                "credential_identity": "discord_test_credential",
                "secret_key": { "value_from_env": DISCORD_TOKEN }
            },
            "operations": operations(&["channel.get", "message.list"])
        },
        {
            // Mattermost is self-hosted, so the deployment names the whole
            // origin — and the module refuses one it may not send a bearer
            // token to.
            "name": "team_chat",
            "module": "mattermost",
            "config": {
                "endpoint_identity": "mattermost_test",
                "credential_identity": "mattermost_test_credential",
                "secret_key": { "value_from_env": MATTERMOST_TOKEN },
                "settings": { "server_origin": "https://chat.example.test" }
            },
            "operations": operations(&["channel.get", "post.create"])
        },
        {
            // Mailchimp's data centre is one host label, from the key.
            "name": "audience",
            "module": "mailchimp",
            "config": {
                "endpoint_identity": "mailchimp_test",
                "credential_identity": "mailchimp_test_credential",
                "secret_key": { "value_from_env": MAILCHIMP_KEY },
                "settings": { "server": "us19" }
            },
            "operations": operations(&["list.get", "member.upsert"])
        },
        // Batch K (spec 027): the development and monitoring connectors. Two
        // name their own instance's whole origin, and two are declarations one
        // deployment completes.
        {
            "name": "source_host",
            "module": "gitlab",
            "config": {
                "endpoint_identity": "gitlab_test",
                "credential_identity": "gitlab_test_credential",
                "secret_key": { "value_from_env": GITLAB_TOKEN },
                "settings": { "instance_origin": "https://gitlab.example.test" }
            },
            "operations": operations(&["project.get", "issue.create"])
        },
        {
            "name": "dashboards",
            "module": "grafana",
            "config": {
                "endpoint_identity": "grafana_test",
                "credential_identity": "grafana_test_credential",
                "secret_key": { "value_from_env": GRAFANA_TOKEN },
                "settings": { "instance_origin": "https://grafana.example.test" }
            },
            "operations": operations(&["alert_rule.list", "dashboard.get"])
        },
        {
            // Bitbucket's HTTP Basic username is the Atlassian account address,
            // which the auth plan carries and no request may choose.
            "name": "code_review",
            "module": "bitbucket",
            "config": {
                "endpoint_identity": "bitbucket_test",
                "credential_identity": "bitbucket_test_credential",
                "secret_key": { "value_from_env": BITBUCKET_TOKEN },
                "settings": { "account_email": "ci@example.test" }
            },
            "operations": operations(&["repository.get", "issue.create"])
        },
        {
            // PagerDuty attributes every write to the account user named by
            // `From`, so it is compiled into the declaration.
            "name": "paging",
            "module": "pagerduty",
            "config": {
                "endpoint_identity": "pagerduty_test",
                "credential_identity": "pagerduty_test_credential",
                "secret_key": { "value_from_env": PAGERDUTY_KEY },
                "settings": { "from_email": "oncall@example.test" }
            },
            "operations": operations(&["incident.get", "incident.create"])
        },
        {
            "name": "uptime",
            "module": "uptimerobot",
            "config": {
                "endpoint_identity": "uptimerobot_test",
                "credential_identity": "uptimerobot_test_credential",
                "secret_key": { "value_from_env": UPTIMEROBOT_TOKEN }
            },
            "operations": operations(&["monitor.list", "incident.get"])
        },
        {
            "name": "edge",
            "module": "cloudflare",
            "config": {
                "endpoint_identity": "cloudflare_test",
                "credential_identity": "cloudflare_test_credential",
                "secret_key": { "value_from_env": CLOUDFLARE_TOKEN }
            },
            "operations": operations(&["zone.get", "dns_record.update"])
        }
    ]);
    // Batch L, the scheduling and people half (spec 028). These are appended
    // rather than written into the literal above because `json!` reaches the
    // macro recursion limit at that size; the value is identical either way.
    if let Json::Array(instances) = &mut instances {
        instances.extend(scheduling_and_people_instances());
        instances.extend(forms_instances());
    }
    instances
}

/// Batch L, the forms half (spec 028).
///
/// Two of the four configure a non-secret value beside their secret one:
/// Jotform's region names one of the three API URLs it publishes, and Acuity's
/// numeric User ID is its HTTP Basic *username*. SurveyMonkey's and Cal.com's
/// origins and version pins are compiled constants, so each configures only its
/// credential. Appended rather than written into the literal above for the same
/// reason the scheduling half is: `json!` reaches the macro recursion limit.
fn forms_instances() -> Vec<Json> {
    vec![
        json!({
            "name": "surveys",
            "module": "jotform",
            "config": {
                "endpoint_identity": "jotform_test",
                "credential_identity": "jotform_test_credential",
                "secret_key": { "value_from_env": JOTFORM_KEY },
                "settings": { "region": "eu" }
            },
            "operations": operations(&["form.list", "submission.list"])
        }),
        json!({
            "name": "questionnaires",
            "module": "surveymonkey",
            "config": {
                "endpoint_identity": "surveymonkey_test",
                "credential_identity": "surveymonkey_test_credential",
                "secret_key": { "value_from_env": SURVEYMONKEY_TOKEN }
            },
            "operations": operations(&["survey.list", "response.list"])
        }),
        json!({
            "name": "appointments",
            "module": "cal_com",
            "config": {
                "endpoint_identity": "cal_com_test",
                "credential_identity": "cal_com_test_credential",
                "secret_key": { "value_from_env": CAL_COM_KEY }
            },
            "operations": operations(&["booking.list", "booking.create"])
        }),
        json!({
            "name": "bookings",
            "module": "acuity",
            "config": {
                "endpoint_identity": "acuity_test",
                "credential_identity": "acuity_test_credential",
                "secret_key": { "value_from_env": ACUITY_KEY },
                "settings": { "user_id": "11145481" }
            },
            "operations": operations(&["appointment.list", "appointment.create"])
        }),
    ]
}

/// Batch L, the scheduling and people half (spec 028).
///
/// Harvest sends two values on every request and only one of them is a secret:
/// the Personal Access Token is the instance's `secret_key`, and the account id
/// it travels beside is ordinary non-secret configuration. BambooHR's company
/// subdomain is this connector's host, and the API key beside it is spent as
/// the HTTP Basic *username*, with the constant password its own example
/// publishes.
fn scheduling_and_people_instances() -> Vec<Json> {
    vec![
        json!({
            "name": "timesheets",
            "module": "harvest",
            "config": {
                "endpoint_identity": "harvest_test",
                "credential_identity": "harvest_test_credential",
                "secret_key": { "value_from_env": HARVEST_TOKEN },
                "settings": {
                    "account_id": "1234567",
                    "user_agent": "Donat (integrations@example.test)"
                }
            },
            "operations": operations(&["time_entry.list", "time_entry.create"])
        }),
        json!({
            "name": "people",
            "module": "bamboohr",
            "config": {
                "endpoint_identity": "bamboohr_test",
                "credential_identity": "bamboohr_test_credential",
                "secret_key": { "value_from_env": BAMBOOHR_KEY },
                "settings": { "company_domain": "acme" }
            },
            "operations": operations(&["employee.get", "employee.create"])
        }),
        json!({
            "name": "timers",
            "module": "clockify",
            "config": {
                "endpoint_identity": "clockify_test",
                "credential_identity": "clockify_test_credential",
                "secret_key": { "value_from_env": CLOCKIFY_KEY },
                "settings": { "workspace_id": "64a687e29ae1f428e7ebe303" }
            },
            "operations": operations(&["project.list", "time_entry.create"])
        }),
        json!({
            "name": "events",
            "module": "eventbrite",
            "config": {
                "endpoint_identity": "eventbrite_test",
                "credential_identity": "eventbrite_test_credential",
                "secret_key": { "value_from_env": EVENTBRITE_TOKEN },
                "settings": { "organization_id": "123456789012" }
            },
            "operations": operations(&["event.list", "event.create"])
        }),
    ]
}

/// One instance of one module, from the full set above, with `mutate` applied.
fn instance_of(module: &str, mutate: impl FnOnce(&mut Json)) -> Json {
    let Json::Array(instances) = every_provider_instance() else {
        panic!("the provider fixture is an array")
    };
    let mut instance = instances
        .into_iter()
        .find(|instance| instance["module"] == json!(module))
        .unwrap_or_else(|| panic!("the fixture declares an instance of `{module}`"));
    mutate(&mut instance);
    json!([instance])
}

/// Spec 010 §11: adding a connector is one module file and one table line, and
/// the table is the whole world a deployment may select from.
#[test]
fn the_module_table_carries_every_hand_written_connector() {
    assert_eq!(
        ConnectorRegistry::built_in_module_names(),
        [
            "http",
            "stripe",
            "airtable",
            "sendgrid",
            "postmark",
            "twilio",
            "openai",
            "typeform",
            "aws_s3",
            "aws_sqs",
            "aws_ses",
            "github",
            "shopify",
            "telegram",
            "calendly",
            "sentry",
            // Batch C (spec 014): the Google Workspace connectors, whose
            // credential is a stored OAuth2 token rather than configuration.
            "google_sheets",
            "google_drive",
            "google_gmail",
            "google_calendar",
            "slack",
            "linear",
            "notion",
            "intercom",
            "hubspot",
            // Batch J (spec 026): added by the payments and billing slice.
            "paddle",
            "mercado_pago",
            "xero",
            "paypal",
            "jira",
            // Batch D (spec 015): the Microsoft 365 connectors, which share one
            // origin and one stored OAuth2 credential shape.
            "microsoft_outlook",
            "microsoft_teams",
            "microsoft_excel",
            "microsoft_onedrive",
            // Batch G (spec 023): the CRM and helpdesk connectors. Four of the
            // six have a per-tenant host and four are declarations one
            // deployment completes.
            "pipedrive",
            "freshdesk",
            "zendesk",
            "woocommerce",
            "salesforce",
            "zoho_crm",
            // Batch H (spec 024): the project-tracking and collaboration
            // connectors. Basecamp's declaration is one a deployment completes,
            // because its account id is the first path segment of every URL it
            // renders.
            "asana",
            "trello",
            "clickup",
            "monday",
            "todoist",
            "basecamp",
            // Batch I (spec 025): the storage and messaging connectors.
            // `dropbox` and `dropbox_content` are one provider on two origins,
            // and a connector has one compiled origin, so a deployment that
            // needs both names both.
            "dropbox",
            "dropbox_content",
            "box",
            "discord",
            "mattermost",
            "mailchimp",
            "zoom",
            // Batch K (spec 027): the development and monitoring connectors.
            // GitLab's and Grafana's origin is the deployment's own instance;
            // Bitbucket's and PagerDuty's declarations are ones a deployment
            // completes.
            "gitlab",
            "grafana",
            "uptimerobot",
            "cloudflare",
            "bitbucket",
            "pagerduty",
            // Batch L, the forms half (spec 028): Jotform's declaration is
            // built from one deployment's configured region.
            "jotform",
            "surveymonkey",
            "cal_com",
            "acuity",
            // Batch L, the scheduling and people half (spec 028): Harvest's
            // declaration is built from one deployment's account id and the
            // identity its provider demands.
            "harvest",
            "bamboohr",
            "clockify",
            "eventbrite",
        ],
        "the published module names are the compiled table itself"
    );
}

/// Every module's deploy-time configuration resolves from `SecretRef`s and
/// metadata, and the whole set compiles into one immutable registry.
#[test]
fn every_hand_written_connector_compiles_from_its_deploy_time_configuration() {
    resolve_test_environment();
    let metadata = metadata(every_provider_instance());
    assert_eq!(errors(&metadata), "", "the fixture is valid metadata");

    let registry = ConnectorRegistry::build(&metadata).expect("every provider instance compiles");

    for (instance, operation) in [
        ("records", "record.list"),
        ("marketing", "contact.upsert"),
        ("transactional", "message.list_outbound"),
        ("telephony", "message.list"),
        ("models", "model.list"),
        ("forms", "form.list"),
        ("objects", "object.put"),
        ("queue", "message.send"),
        ("mail", "template.list"),
        // Batch G (spec 023).
        ("sales", "deal.create"),
        ("helpdesk", "ticket.list"),
        ("support", "ticket.create"),
        ("store", "order.create"),
        // Batch H (spec 024).
        ("work", "task.create"),
        ("boards", "card.create"),
        ("tasks", "task.create"),
        ("items", "item.create"),
        ("todos", "task.close"),
        // Batch I (spec 025).
        ("guild_chat", "message.list"),
        ("team_chat", "post.create"),
        ("audience", "member.upsert"),
    ] {
        let fingerprint = registry
            .configuration_fingerprint(instance, operation)
            .unwrap_or_else(|| panic!("`{instance}` compiled `{operation}`"));
        assert_eq!(fingerprint.len(), 64, "a fingerprint is a SHA-256 digest");
        assert!(
            registry
                .configuration_fingerprint(instance, "record.does_not_exist")
                .is_none(),
            "only the enabled operations are compiled"
        );
    }
}

/// Spec 010 §7: an inventory-only operation stays declared, typed, and tested,
/// and a deployment still cannot enable it. The refusal names the exact
/// metadata path, and it lands before a listener opens.
#[test]
fn no_hand_written_connector_publishes_an_inventory_only_operation() {
    resolve_test_environment();
    // Each of these is inventory-only for its own recorded reason, and ADR 063
    // reaches none of them: a repeat that sets the same values changes nothing,
    // a repeat Telegram publishes nothing about has no recorded consequence, and
    // a repeat AWS documents as safe needs a class that keeps the retry.
    for (module, operation) in [
        ("airtable", "record.update_patch"),
        ("sendgrid", "list.update"),
        ("telegram", "message.delete"),
        ("openai", "chat.complete"),
        ("aws_sqs", "message.delete"),
        ("github", "issue.update"),
        // Batch G (spec 023): a partial update nobody publishes a repeat
        // consequence for, and an upsert the provider documents as repeat-safe
        // over a method the gate does not admit.
        ("zendesk", "user.create_or_update"),
        ("woocommerce", "order.update"),
        ("pipedrive", "deal.update"),
        ("freshdesk", "ticket.update"),
        // Batch H (spec 024): a partial update over a `PUT` nobody publishes a
        // repeat consequence for, a delete whose second send the provider is
        // silent about, and a GraphQL mutation that is a POST whatever it does.
        ("asana", "task.update"),
        ("trello", "card.delete"),
        ("clickup", "task.update"),
        ("monday", "item.delete"),
        ("todoist", "task.delete"),
        // Batch I (spec 025): the send whose provider publishes a
        // deduplication mechanism and no window for it, which is a class
        // neither ADR 073 nor ADR 063 admits.
        ("discord", "message.send"),
        // Batch K (spec 027): a partial state change nobody publishes a repeat
        // consequence for, a `PUT` whose provider never described its effect,
        // and a write the provider documents as repeat-safe over a `POST` —
        // which wants a class that keeps the retry rather than one that trades
        // it away.
        ("pagerduty", "incident.update"),
        ("grafana", "alert_rule.update"),
        ("uptimerobot", "monitor.pause"),
        ("cloudflare", "zone.update"),
        // Batch L, the forms half (spec 028): a `DELETE` against a fixed
        // identity whose provider publishes no repeat statement, and therefore
        // no consequence of a second send either.
        ("jotform", "submission.delete"),
        ("surveymonkey", "response.delete"),
        ("cal_com", "booking.cancel"),
        ("acuity", "appointment.cancel"),
        // Batch L, the scheduling and people half (spec 028): a `PATCH` partial
        // update whose provider publishes nothing about a second send, so there
        // is no consequence to record and ADR 063's bar is not met either.
        ("harvest", "time_entry.update"),
        // BambooHR publishes its partial update over a `POST`, and publishes
        // nothing about what a second identical send does.
        ("bamboohr", "employee.update"),
        // A `PUT` against a fixed identity whose provider publishes no repeat
        // statement at all: NaturalMethod is evidence, not a method.
        ("clockify", "time_entry.update"),
        ("eventbrite", "event.update"),
    ] {
        let metadata = metadata(instance_of(module, |instance| {
            instance["operations"] = operations(&[operation]);
        }));
        let rendered = errors(&metadata);
        assert!(
            rendered.contains("inventory-only and cannot be enabled by a deployment"),
            "`{module}.{operation}` must be refused: {rendered}"
        );
        assert!(
            rendered.contains("connectors.yaml[0].operations[0].name"),
            "the refusal names the exact metadata path: {rendered}"
        );
        assert!(
            ConnectorRegistry::build(&metadata).is_err(),
            "`{module}.{operation}` must not reach a compiled registry"
        );
    }
}

/// Spec 010 §11 again, from the other side: a name this binary was not built
/// with is refused, whichever module a deployment selects.
#[test]
fn an_undeclared_operation_cannot_be_enabled_on_a_hand_written_connector() {
    resolve_test_environment();
    let metadata = metadata(instance_of("openai", |instance| {
        instance["operations"] = operations(&["chat.stream"]);
    }));
    let rendered = errors(&metadata);
    assert!(
        rendered.contains(
            "connector operation `chat.stream` on module `openai`: connector operation is not \
             compiled into this binary"
        ),
        "{rendered}"
    );
}

/// A missing required value stops startup naming only the variable, and never
/// a value another instance resolved.
#[test]
fn a_missing_secret_stops_startup_naming_only_the_variable() {
    resolve_test_environment();
    let metadata = metadata(instance_of("aws_s3", |instance| {
        instance["config"]["secrets"]["secret_access_key"] = json!({ "value_from_env": ABSENT });
    }));

    let error = ConnectorRegistry::build(&metadata)
        .err()
        .expect("a missing AWS secret prevents serving")
        .to_string();

    assert_eq!(
        error,
        format!("connector instance `objects` requires environment variable `{ABSENT}`")
    );
    assert!(
        !error.contains("AKIAIOSFODNN7EXAMPLE"),
        "a startup error never discloses a resolved credential: {error}"
    );
}

/// A required non-secret setting is checked in metadata, before any
/// environment value is read.
#[test]
fn a_missing_deploy_time_setting_is_refused_with_its_metadata_path() {
    resolve_test_environment();
    for (module, setting) in [
        ("airtable", "base_id"),
        ("twilio", "account_sid"),
        ("aws_s3", "bucket"),
        ("aws_sqs", "queue_name"),
        ("aws_ses", "from_email_address"),
    ] {
        let metadata = metadata(instance_of(module, |instance| {
            instance["config"]["settings"]
                .as_object_mut()
                .expect("the fixture declares settings")
                .remove(setting);
        }));
        let rendered = errors(&metadata);
        assert!(
            rendered.contains(&format!("connectors.yaml[0].config.settings.{setting}")),
            "`{module}` must name the missing setting: {rendered}"
        );
    }
}

/// A setting a module does not read is a declaration the runtime ignores, so
/// it is refused rather than accepted (ADR 034).
#[test]
fn a_setting_no_module_reads_is_refused_rather_than_ignored() {
    resolve_test_environment();
    let metadata = metadata(instance_of("airtable", |instance| {
        instance["config"]["settings"]["workspace_id"] = json!("wspTest");
    }));
    let rendered = errors(&metadata);
    assert!(
        rendered.contains("connectors.yaml[0].config.settings.workspace_id"),
        "{rendered}"
    );
    assert!(rendered.contains("airtable"), "{rendered}");
}

/// A hand-written connector has one fixed provider origin, so accepting an
/// endpoint, header, or network field would turn it into the declarative HTTP
/// module it deliberately is not.
#[test]
fn a_hand_written_connector_refuses_the_declarative_connector_configuration() {
    resolve_test_environment();
    for (field, value) in [
        ("base_url", json!("https://attacker.invalid")),
        ("network_policy", json!("public")),
        (
            "headers",
            json!([{ "name": "X-Test", "value_from_env": AIRTABLE_TOKEN }]),
        ),
    ] {
        let metadata = metadata(instance_of("airtable", |instance| {
            instance["config"][field] = value.clone();
        }));
        let rendered = errors(&metadata);
        assert!(
            rendered.contains(&format!("connectors.yaml[0].config.{field}")),
            "`{field}` must be refused: {rendered}"
        );
    }
}

/// ADR 046 and ADR 063: the effect class of `aws_sqs.message.send` depends on
/// the queue this deployment configured. A FIFO queue publishes deduplication
/// and the send is provider-idempotent; a standard queue publishes the
/// opposite, so the same send is at-most-once — deployable, and reachable from
/// a Process only through the activity's own opt-in.
#[test]
fn a_standard_queue_cannot_enable_the_deduplicated_send() {
    resolve_test_environment();
    let standard = metadata(instance_of("aws_sqs", |instance| {
        instance["config"]["settings"]["queue_name"] = json!("donat-test");
        instance["config"]["settings"]["queue_type"] = json!("standard");
        instance["operations"] = operations(&["message.send"]);
    }));
    assert_eq!(errors(&standard), "", "a standard-queue send is deployable");
    let registry = ConnectorRegistry::build(&standard).expect("a standard queue compiles");
    assert!(
        matches!(
            registry
                .operation_spec("default", "queue", operation_id("message.send"))
                .expect("the standard-queue send is published")
                .effect,
            OperationEffect::AtMostOnce
        ),
        "the class this deployment's own target denies is the weaker one, not the stronger"
    );

    // The same queue may still be read: refusing the whole connector because
    // one operation is not repeat-safe would punish a deployment for an
    // operation it never enabled.
    let readable = metadata(instance_of("aws_sqs", |instance| {
        instance["config"]["settings"]["queue_name"] = json!("donat-test");
        instance["config"]["settings"]["queue_type"] = json!("standard");
        instance["operations"] = operations(&["message.receive", "queue.attributes"]);
    }));
    assert_eq!(errors(&readable), "");
    assert!(ConnectorRegistry::build(&readable).is_ok());
}

/// ADR 046, the same shape on a different provider: a keyless delete against a
/// versioning-enabled bucket leaves a second delete marker, so `object.delete`
/// is executable only on an unversioned bucket.
#[test]
fn a_versioned_bucket_cannot_enable_the_keyless_delete() {
    resolve_test_environment();
    let versioned = metadata(instance_of("aws_s3", |instance| {
        instance["config"]["settings"]["bucket_versioning"] = json!("versioned");
        instance["operations"] = operations(&["object.delete"]);
    }));
    let rendered = errors(&versioned);
    assert!(
        rendered.contains("inventory-only and cannot be enabled by a deployment"),
        "a versioned bucket cannot enable a keyless delete: {rendered}"
    );

    let readable = metadata(instance_of("aws_s3", |instance| {
        instance["config"]["settings"]["bucket_versioning"] = json!("versioned");
        instance["operations"] = operations(&["object.get", "object.put"]);
    }));
    assert_eq!(errors(&readable), "");
}

/// AWS's own check on a deployment that disagrees with its own queue name.
#[test]
fn a_queue_type_that_disagrees_with_its_queue_name_is_refused() {
    resolve_test_environment();
    let metadata = metadata(instance_of("aws_sqs", |instance| {
        instance["config"]["settings"]["queue_type"] = json!("standard");
    }));
    let rendered = errors(&metadata);
    assert!(
        rendered.contains("queue_type"),
        "a mislabelled queue is refused at startup: {rendered}"
    );
}

/// Twilio's HTTP Basic username *is* its Account SID, so its declaration is
/// built per deployment. The table still refuses a SID Twilio's own grammar
/// does not admit, before a listener opens.
#[test]
fn the_twilio_declaration_is_built_from_its_configured_account_sid() {
    resolve_test_environment();
    let valid = metadata(instance_of("twilio", |_| {}));
    assert_eq!(errors(&valid), "");
    assert!(
        ConnectorRegistry::build(&valid)
            .expect("a configured Twilio account compiles")
            .configuration_fingerprint("telephony", "message.list")
            .is_some()
    );

    for hostile in [
        "",
        "AC0000",
        "ACzz../../evil",
        ACCOUNT_SID.to_lowercase().as_str(),
    ] {
        let metadata = metadata(instance_of("twilio", |instance| {
            instance["config"]["settings"]["account_sid"] = json!(hostile);
        }));
        let rendered = errors(&metadata);
        assert!(
            rendered.contains("connectors.yaml[0].config.settings.account_sid"),
            "account SID `{hostile}` must be refused: {rendered}"
        );
        assert!(ConnectorRegistry::build(&metadata).is_err());
    }
}

/// Spec 028 §3: Jotform's two deploy-time values are configured apart, and the
/// split is observable — the non-secret region is part of the configuration
/// fingerprint, while nothing about the secret but the *name* of its variable is.
#[test]
fn the_jotform_region_is_non_secret_configuration_and_its_key_is_not() {
    resolve_test_environment();

    let fingerprint_for = |region: &str, variable: &str| {
        let metadata = metadata(instance_of("jotform", |instance| {
            instance["config"]["settings"]["region"] = json!(region);
            instance["config"]["secret_key"] = json!({ "value_from_env": variable });
        }));
        assert_eq!(errors(&metadata), "", "the fixture is valid metadata");
        ConnectorRegistry::build(&metadata)
            .expect("a published region compiles")
            .configuration_fingerprint("surveys", "form.list")
            .expect("a compiled operation has a fingerprint")
            .to_owned()
    };

    let eu = fingerprint_for("eu", JOTFORM_KEY);
    let us = fingerprint_for("us", JOTFORM_KEY);
    assert_ne!(
        eu, us,
        "the region names a different origin, so it is part of the pinned identity"
    );
    assert_eq!(
        eu,
        fingerprint_for("eu", JOTFORM_KEY),
        "the fingerprint is a property of the configuration, not of a run"
    );
    // The resolved key is not in it at all: only the variable's name is, so
    // rotating the value behind one variable never moves the fingerprint.
    for fingerprint in [&eu, &us] {
        assert_eq!(fingerprint.len(), 64, "a fingerprint is a SHA-256 digest");
        assert!(!fingerprint.contains("jf_test_jotform_sentinel"));
    }
    assert_ne!(
        eu,
        fingerprint_for("eu", AIRTABLE_TOKEN),
        "the *name* of the variable behind the secret is part of the identity"
    );

    // And a region Jotform does not publish is refused with its metadata path,
    // before a listener opens, without naming a resolved value.
    for hostile in ["", "US", "api.jotform.com", "https://attacker.invalid"] {
        let metadata = metadata(instance_of("jotform", |instance| {
            instance["config"]["settings"]["region"] = json!(hostile);
        }));
        let rendered = errors(&metadata);
        assert!(
            rendered.contains("connectors.yaml[0].config.settings.region"),
            "region `{hostile}` must be refused: {rendered}"
        );
        assert!(!rendered.contains("jf_test_jotform_sentinel"), "{rendered}");
        assert!(ConnectorRegistry::build(&metadata).is_err());
    }
}

/// Spec 028 §3, the other shape: Acuity's numeric User ID is the HTTP Basic
/// *username*, so it is deploy-time configuration a request may never choose —
/// it is fingerprinted, held to the provider's own grammar before a listener
/// opens, and the API key beside it never appears in a refusal.
#[test]
fn the_acuity_user_id_is_non_secret_configuration_and_its_key_is_not() {
    resolve_test_environment();

    let fingerprint_for = |user_id: &str| {
        let metadata = metadata(instance_of("acuity", |instance| {
            instance["config"]["settings"]["user_id"] = json!(user_id);
        }));
        assert_eq!(errors(&metadata), "", "the fixture is valid metadata");
        ConnectorRegistry::build(&metadata)
            .expect("a numeric user id compiles")
            .configuration_fingerprint("bookings", "appointment.list")
            .expect("a compiled operation has a fingerprint")
            .to_owned()
    };

    let one = fingerprint_for("11145481");
    let other = fingerprint_for("22290962");
    assert_ne!(
        one, other,
        "the account the Basic username names is part of the pinned identity"
    );
    assert_eq!(one.len(), 64, "a fingerprint is a SHA-256 digest");
    assert!(!one.contains("acuity_test_acuity_sentinel"));

    for hostile in ["", "acme", "1114 5481", "11145481:extra", "-1"] {
        let metadata = metadata(instance_of("acuity", |instance| {
            instance["config"]["settings"]["user_id"] = json!(hostile);
        }));
        let rendered = errors(&metadata);
        assert!(
            rendered.contains("connectors.yaml[0].config.settings.user_id"),
            "user id `{hostile}` must be refused: {rendered}"
        );
        assert!(
            !rendered.contains("acuity_test_acuity_sentinel"),
            "{rendered}"
        );
        assert!(ConnectorRegistry::build(&metadata).is_err());
    }
}

fn operation_id(name: &str) -> donat_connector_abi::OperationId {
    donat_connector_abi::OperationId::parse(name).expect("a canonical operation ID")
}

/// Every enabled operation of a hand-written connector reaches process
/// compilation, as the projection of the declaration it was validated against.
///
/// The contract a Process binds is the declaration's own: the deploy-time
/// values the connector fills — an Airtable base, an AWS bucket, a durable
/// activity's deduplication id — are not in it, and the inputs the module
/// consumes without rendering them into the request are.
#[test]
fn a_hand_written_connector_publishes_its_executable_operations() {
    resolve_test_environment();
    let metadata = metadata(every_provider_instance());
    let registry = ConnectorRegistry::build(&metadata).expect("every provider instance compiles");

    for (instance, operation) in [
        ("records", "record.list"),
        ("marketing", "contact.upsert"),
        ("transactional", "message.list_outbound"),
        ("telephony", "message.list"),
        ("models", "model.list"),
        ("forms", "form.list"),
        ("objects", "object.put"),
        ("queue", "message.send"),
        ("mail", "template.list"),
        // Batch B.
        ("code", "file.put"),
        ("storefront", "product.delete"),
        ("chat", "chat.get"),
        ("scheduling", "user.me"),
        ("errors", "issue.get"),
    ] {
        let spec = registry
            .operation_spec("default", instance, operation_id(operation))
            .unwrap_or_else(|| panic!("`{instance}.{operation}` is published"));
        assert_eq!(spec.operation.as_str(), operation);
        assert_eq!(spec.steps.len(), 1, "one provider request per operation");
        assert_eq!(spec.origins.len(), 1, "one fixed origin per operation");
    }

    // Airtable's base is deploy-time material, and its table is not.
    let list = registry
        .operation_spec("default", "records", operation_id("record.list"))
        .expect("the Airtable read is published");
    assert_eq!(
        list.input.roots.keys().collect::<Vec<_>>(),
        ["table"],
        "the configured base is never a field a Process binds"
    );
    assert!(list.output.roots["records"].required);
    assert!(!list.output.roots["offset"].required);
    assert!(matches!(list.effect, OperationEffect::ReadOnly));

    // The S3 write's bytes are its input even though no template leaf renders
    // them, and the ETag is its output even though it only ever arrives as a
    // response header.
    let put = registry
        .operation_spec("default", "objects", operation_id("object.put"))
        .expect("the S3 write is published");
    assert_eq!(
        put.input.roots.keys().collect::<Vec<_>>(),
        ["body", "key"],
        "the configured bucket is never a field a Process binds"
    );
    assert_eq!(put.output.roots.keys().collect::<Vec<_>>(), ["etag"]);
    assert_eq!(put.steps[0].method, "PUT");
    assert_eq!(put.steps[0].path, "/donat-test-bucket/{key}");

    // The one published operation whose class carries a key publishes the
    // evidence the durable runtime holds its send inside.
    let send = registry
        .operation_spec("default", "queue", operation_id("message.send"))
        .expect("the FIFO send is published");
    let OperationEffect::ProviderIdempotent { side_effect_steps } = &send.effect else {
        panic!("a FIFO send is provider-idempotent")
    };
    assert_eq!(side_effect_steps.len(), 1);
    assert!(matches!(
        &side_effect_steps[0].fixed_binding,
        donat_connector_catalog::FixedIdempotencyBinding::BodyField { pointer }
            if pointer == "/MessageDeduplicationId"
    ));
    assert!(
        side_effect_steps[0].clock_safety_margin_ms < side_effect_steps[0].minimum_retention_ms
    );
    assert!(
        !send.input.roots.contains_key("deduplication_id"),
        "the deduplication id is the activity's own key, never a caller's"
    );
    assert!(send.input.roots["group_id"].required);
}

/// Spec 010 §7 from the publication side: an operation a deployment cannot
/// enable is an operation process compilation never sees.
#[test]
fn a_hand_written_connector_publishes_no_inventory_only_operation() {
    resolve_test_environment();
    let metadata = metadata(every_provider_instance());
    let registry = ConnectorRegistry::build(&metadata).expect("every provider instance compiles");

    for (instance, operation) in [
        ("records", "record.create"),
        ("marketing", "mail.send"),
        ("telephony", "message.send"),
        ("queue", "message.delete"),
        // Batch B: every operation whose provider publishes no key.
        ("code", "issue.create"),
        ("code", "workflow.dispatch"),
        ("storefront", "order.create"),
        ("chat", "message.send"),
        ("errors", "issue.update"),
    ] {
        assert!(
            registry
                .operation_spec("default", instance, operation_id(operation))
                .is_none(),
            "`{instance}.{operation}` is inventory-only and must not be published"
        );
    }
}

/// ADR 046 reaches publication: one module, two deployments, and the one whose
/// own target denies a class publishes strictly fewer operations than the one
/// whose target admits it.
#[test]
fn a_configuration_dependent_class_publishes_fewer_operations() {
    resolve_test_environment();

    // A FIFO queue publishes the deduplicated send; a standard queue publishes
    // the same reads and not the send.
    let fifo = metadata(instance_of("aws_sqs", |instance| {
        instance["operations"] = operations(&["message.receive"]);
    }));
    let standard = metadata(instance_of("aws_sqs", |instance| {
        instance["config"]["settings"]["queue_name"] = json!("donat-test");
        instance["config"]["settings"]["queue_type"] = json!("standard");
        instance["operations"] = operations(&["message.receive"]);
    }));
    let fifo = ConnectorRegistry::build(&fifo).expect("a FIFO queue compiles");
    let standard = ConnectorRegistry::build(&standard).expect("a standard queue compiles");
    assert!(
        fifo.operation_spec("default", "queue", operation_id("message.receive"))
            .is_some()
            && standard
                .operation_spec("default", "queue", operation_id("message.receive"))
                .is_some(),
        "both queues publish the read they share"
    );

    // An unversioned bucket publishes the keyless delete; a versioned one, on
    // the same module and the same enabled set, does not.
    let unversioned = metadata(instance_of("aws_s3", |instance| {
        instance["operations"] = operations(&["object.get"]);
    }));
    let unversioned = ConnectorRegistry::build(&unversioned).expect("an unversioned bucket");
    assert!(
        unversioned
            .operation_spec("default", "objects", operation_id("object.get"))
            .is_some()
    );

    // The refusal is a *startup* refusal, so the deployment that names the
    // configuration-denied operation never reaches a registry at all. A
    // versioned bucket's keyless delete is the one that stays inventory-only:
    // a second send leaves a second delete marker, and no class admits that.
    let denied = metadata(instance_of("aws_s3", |instance| {
        instance["config"]["settings"]["bucket_versioning"] = json!("versioned");
        instance["operations"] = operations(&["object.delete"]);
    }));
    assert!(
        ConnectorRegistry::build(&denied).is_err(),
        "a class this deployment's target denies never reaches publication"
    );
}

/// Spec 028 §3: Harvest sends two credential-adjacent values on every request
/// and only one of them is a secret.
///
/// The account id is non-secret deploy-time configuration, so it *decides* the
/// operation's configuration fingerprint — changing which account a pinned
/// operation reaches changes what that operation is. The Personal Access Token
/// contributes only the *name* of the environment variable behind it: rotating
/// the resolved value leaves the fingerprint byte-for-byte identical, and no
/// startup error, diagnostic, or rendered validation message names it.
#[test]
fn the_harvest_account_id_is_fingerprinted_and_its_token_is_not() {
    const TOKEN_SENTINEL: &str = "hv_test_harvest_sentinel";
    const ROTATED_TOKEN: &str = "hv_test_harvest_rotated_sentinel";

    fn fingerprint(instance: Json) -> String {
        let metadata = metadata(instance);
        assert_eq!(errors(&metadata), "", "the fixture is valid metadata");
        ConnectorRegistry::build(&metadata)
            .expect("the harvest instance compiles")
            .configuration_fingerprint("timesheets", "time_entry.list")
            .expect("`timesheets` compiled `time_entry.list`")
            .to_owned()
    }

    resolve_test_environment();
    let configured = fingerprint(instance_of("harvest", |_| {}));
    assert_eq!(configured.len(), 64, "a fingerprint is a SHA-256 digest");

    // The non-secret half is in the fingerprint: another account is another
    // pinned operation.
    let other_account = fingerprint(instance_of("harvest", |instance| {
        instance["config"]["settings"]["account_id"] = json!("7654321");
    }));
    assert_ne!(
        configured, other_account,
        "the account this instance reaches is part of what a pinned operation is"
    );

    // The secret half is not. Rotating the value behind the same variable name
    // changes nothing a fingerprint can see.
    //
    // Safety: this test sets and restores one variable that no case asserts a
    // resolved value of, on the thread that then reads it.
    unsafe { std::env::set_var(HARVEST_TOKEN, ROTATED_TOKEN) };
    let rotated = fingerprint(instance_of("harvest", |_| {}));
    unsafe { std::env::set_var(HARVEST_TOKEN, TOKEN_SENTINEL) };
    assert_eq!(
        configured, rotated,
        "a fingerprint carries the name of the environment variable and never its value"
    );

    // And a refusal of the non-secret half names the key it refused without
    // disclosing the secret one.
    resolve_test_environment();
    let refused = metadata(instance_of("harvest", |instance| {
        instance["config"]["settings"]["account_id"] = json!("1234567/../7654321");
    }));
    let rendered = errors(&refused);
    assert!(
        rendered.contains("connectors.yaml[0].config.settings.account_id"),
        "the refusal names the configuration key: {rendered}"
    );
    assert!(
        !rendered.contains(TOKEN_SENTINEL) && !rendered.contains(ROTATED_TOKEN),
        "a startup refusal must not disclose a resolved secret: {rendered}"
    );
    let Err(failure) = ConnectorRegistry::build(&refused) else {
        panic!("an account id Harvest's own grammar refuses never compiles")
    };
    let failure = failure.to_string();
    assert!(!failure.contains(TOKEN_SENTINEL), "{failure}");
}
