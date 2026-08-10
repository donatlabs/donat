//! The one list of every connector this workspace declares, shared by the
//! tests that need to walk all of them at once.
//!
//! It lives here rather than in whichever test first needed it because a
//! second reader has since appeared: `projection.rs` asserts a property of
//! every operation, and `provider_schema.rs` dumps the same set for the audit
//! that compares it against each provider's published schema. Two copies of
//! this list would drift, and a connector missing from one copy is a
//! connector silently exempted from the check it was added for.
//!
//! Modules whose declaration is per-deployment are built here with the same
//! placeholder configuration every test uses; none of it is a credential.

#![allow(dead_code)]

use donat_connectors::providers::{
    acuity, airtable, asana, aws_s3, aws_ses, aws_sqs, bamboohr, basecamp, bitbucket, box_platform,
    cal_com, calendly, clickup, clockify, cloudflare, discord, dropbox, dropbox_content,
    eventbrite, freshdesk, github, gitlab, google_calendar, google_drive, google_gmail,
    google_sheets, grafana, harvest, hubspot, intercom, jira, jotform, linear, mailchimp,
    mattermost, mercado_pago, microsoft_excel, microsoft_onedrive, microsoft_outlook,
    microsoft_teams, monday, notion, openai, paddle, pagerduty, paypal, pipedrive, postmark,
    salesforce, sendgrid, sentry, shopify, slack, surveymonkey, telegram, todoist, trello, twilio,
    typeform, uptimerobot, woocommerce, xero, zendesk, zoho_crm, zoom,
};
use donat_connectors::sdk::{Connector, Operation};

pub const ACCOUNT_SID: &str = "AC00000000000000000000000000000042";
pub const JIRA_EMAIL: &str = "integrations@example.test";

/// Every operation a deployment could enable, per module, as the runtime
/// compiles them.
pub fn executable_operations() -> Vec<(&'static str, Vec<Operation>)> {
    fn executable(connector: &Connector) -> Vec<Operation> {
        connector
            .operations()
            .iter()
            .filter(|operation| operation.is_executable())
            .cloned()
            .collect()
    }

    let twilio = twilio::connector(ACCOUNT_SID).expect("a valid account SID declares");
    let jira = jira::connector(JIRA_EMAIL).expect("a valid account address declares");
    let zendesk = zendesk::connector(JIRA_EMAIL).expect("a valid account address declares");
    let woocommerce =
        woocommerce::connector("ck_projection").expect("a valid consumer key declares");
    let zoho_crm =
        zoho_crm::connector(zoho_crm::Region::parse("eu").expect("a published data centre"))
            .expect("a published region declares");
    let basecamp = basecamp::connector("999999999", "Donat (projection@example.invalid)")
        .expect("a valid account id and identity declare");
    let bitbucket =
        bitbucket::connector("projection@example.test").expect("a valid account address declares");
    let pagerduty =
        pagerduty::connector("projection@example.test").expect("a valid From address declares");
    let harvest = harvest::connector("1234567", "Donat (projection@example.invalid)")
        .expect("a valid account id and identity declare");
    let clockify =
        clockify::connector("64a687e29ae1f428e7ebe303").expect("a valid workspace id declares");
    let eventbrite =
        eventbrite::connector("123456789012").expect("a valid organization id declares");
    let acuity = acuity::connector("11145481").expect("a numeric user id declares");
    let jotform = jotform::connector(jotform::Region::parse("eu").expect("a published region"))
        .expect("a published region declares");
    let s3 = aws_s3::S3Instance::compile(
        &aws_s3::S3Configuration::new(
            "eu-west-1",
            "donat-projection-bucket",
            aws_s3::BucketVersioning::Unversioned,
        )
        .expect("a valid S3 configuration"),
    )
    .expect("a configured S3 instance compiles");
    let sqs = aws_sqs::SqsInstance::compile(
        &aws_sqs::SqsConfiguration::new(
            "eu-west-1",
            "123456789012",
            "donat-projection.fifo",
            aws_sqs::QueueType::Fifo,
        )
        .expect("a valid SQS configuration"),
    )
    .expect("a configured SQS instance compiles");
    let ses = aws_ses::SesInstance::compile(
        &aws_ses::SesConfiguration::new("eu-west-1", "notifications@example.test")
            .expect("a valid SES configuration"),
    )
    .expect("a configured SES instance compiles");

    vec![
        ("airtable", executable(airtable::connector())),
        ("sendgrid", executable(sendgrid::connector())),
        ("postmark", executable(postmark::connector())),
        ("twilio", executable(&twilio)),
        ("openai", executable(openai::connector())),
        ("typeform", executable(typeform::connector())),
        // Batch B (spec 013).
        ("github", executable(github::connector())),
        ("shopify", executable(shopify::connector())),
        ("telegram", executable(telegram::connector())),
        ("calendly", executable(calendly::connector())),
        ("sentry", executable(sentry::connector())),
        // Batch C (spec 014).
        ("google_sheets", executable(google_sheets::connector())),
        ("google_drive", executable(google_drive::connector())),
        ("google_gmail", executable(google_gmail::connector())),
        ("google_calendar", executable(google_calendar::connector())),
        // Batch D (spec 015).
        (
            "microsoft_outlook",
            executable(microsoft_outlook::connector()),
        ),
        ("microsoft_teams", executable(microsoft_teams::connector())),
        ("microsoft_excel", executable(microsoft_excel::connector())),
        (
            "microsoft_onedrive",
            executable(microsoft_onedrive::connector()),
        ),
        // Batch E (spec 016).
        ("slack", executable(slack::connector())),
        ("linear", executable(linear::connector())),
        ("notion", executable(notion::connector())),
        ("intercom", executable(intercom::connector())),
        ("hubspot", executable(hubspot::connector())),
        ("jira", executable(&jira)),
        // Batch G (spec 023).
        ("pipedrive", executable(pipedrive::connector())),
        ("freshdesk", executable(freshdesk::connector())),
        ("salesforce", executable(salesforce::connector())),
        ("zendesk", executable(&zendesk)),
        ("woocommerce", executable(&woocommerce)),
        ("zoho_crm", executable(&zoho_crm)),
        // Batch H (spec 024).
        ("asana", executable(asana::connector())),
        ("trello", executable(trello::connector())),
        ("clickup", executable(clickup::connector())),
        ("monday", executable(monday::connector())),
        ("todoist", executable(todoist::connector())),
        ("basecamp", executable(&basecamp)),
        // Batch I (spec 025). `dropbox_content` is the second half of one
        // provider rather than a second provider: a connector has one compiled
        // origin, and Dropbox serves its bytes from another one.
        ("dropbox", executable(dropbox::connector())),
        ("dropbox_content", executable(dropbox_content::connector())),
        ("box", executable(box_platform::connector())),
        ("discord", executable(discord::connector())),
        ("mattermost", executable(mattermost::connector())),
        ("mailchimp", executable(mailchimp::connector())),
        ("zoom", executable(zoom::connector())),
        // Batch J (spec 026).
        ("paddle", executable(paddle::connector())),
        ("mercado_pago", executable(mercado_pago::connector())),
        // PayPal was missing from this list until the schema audit compared it
        // against the compiled module table and found ten executable operations
        // nothing walked: no projection assertion, no audit. It is the drift
        // this module's header warns about, caught once.
        ("paypal", executable(paypal::connector())),
        ("xero", executable(xero::connector())),
        // Batch K (spec 027).
        ("gitlab", executable(gitlab::connector())),
        ("grafana", executable(grafana::connector())),
        ("uptimerobot", executable(uptimerobot::connector())),
        ("cloudflare", executable(cloudflare::connector())),
        ("bitbucket", executable(&bitbucket)),
        ("pagerduty", executable(&pagerduty)),
        // Batch L, the forms half (spec 028).
        ("jotform", executable(&jotform)),
        ("surveymonkey", executable(surveymonkey::connector())),
        ("cal_com", executable(cal_com::connector())),
        ("acuity", executable(&acuity)),
        // Batch L, the scheduling and people half (spec 028).
        ("harvest", executable(&harvest)),
        ("bamboohr", executable(bamboohr::connector())),
        ("clockify", executable(&clockify)),
        ("eventbrite", executable(&eventbrite)),
        (
            "aws_s3",
            s3.operations()
                .iter()
                .filter(|operation| operation.is_executable())
                .cloned()
                .collect(),
        ),
        (
            "aws_sqs",
            sqs.operations()
                .iter()
                .filter(|operation| operation.is_executable())
                .cloned()
                .collect(),
        ),
        (
            "aws_ses",
            ses.operations()
                .iter()
                .filter(|operation| operation.is_executable())
                .cloned()
                .collect(),
        ),
    ]
}
