//! Every executable operation of every hand-written connector projects a
//! contract a Process can actually bind.
//!
//! The projection is derived from the declaration, so the failure mode it can
//! still have is a declaration that is *incomplete*: a slot the module fills
//! itself but never marked `supplied_input`, or an input the module consumes
//! without a template leaf that reads it. Either one publishes a contract the
//! module would refuse at the first attempt, which is exactly the disagreement
//! between two descriptions of one provider that the projection exists to
//! prevent.
//!
//! These cases hold that shut for the whole batch at once, so a tenth connector
//! written against the same SDK inherits them.

use std::time::Duration;

use donat_connectors::providers::{
    acuity, airtable, asana, aws, aws_s3, aws_ses, aws_sqs, bamboohr, basecamp, bitbucket,
    box_platform, cal_com, calendly, clickup, clockify, cloudflare, discord, dropbox,
    dropbox_content, eventbrite, freshdesk, github, gitlab, google_calendar, google_drive,
    google_gmail, google_sheets, grafana, harvest, hubspot, intercom, jira, jotform, linear,
    mailchimp, mattermost, mercado_pago, microsoft_excel, microsoft_onedrive, microsoft_outlook,
    microsoft_teams, monday, notion, openai, paddle, pagerduty, pipedrive, postmark, salesforce,
    sendgrid, sentry, shopify, slack, surveymonkey, telegram, todoist, trello, twilio, typeform,
    uptimerobot, woocommerce, xero, zendesk, zoho_crm, zoom,
};
use donat_connectors::sdk::{Connector, EffectClass, Operation};

const ACCOUNT_SID: &str = "AC00000000000000000000000000000042";
const JIRA_EMAIL: &str = "integrations@example.test";

/// The deploy-time keys each module fills itself, on top of the shared AWS
/// reserved names.
fn deploy_time_names() -> Vec<&'static str> {
    let mut names = aws::RESERVED_INPUT_NAMES.to_vec();
    names.push(airtable::BASE_ID);
    names.push(twilio::ACCOUNT_SID);
    names.push("queue_url_from_configuration");
    names.push("from_email_address_from_configuration");
    // Batch B: the shop label is Shopify's *host*, so no operation may publish
    // it as a Process input.
    names.push(shopify::SHOP);
    // Batch G: four per-tenant hosts and the configured values that complete
    // their declarations. None of them may be published as a Process input.
    //
    // Zendesk's `email` is deliberately absent from this list even though it is
    // deploy-time material: it is a *credential field* name rather than an
    // input slot, and `email` is an ordinary operation input on other providers
    // in this workspace (a Shopify order carries one). A reserved name here is
    // a name no connector may publish anywhere, which is the wrong shape for a
    // value that only ever reaches the credential.
    names.push(zendesk::SUBDOMAIN);
    names.push(freshdesk::DOMAIN);
    names.push(salesforce::MY_DOMAIN);
    names.push(woocommerce::STORE_ORIGIN);
    names.push(woocommerce::CONSUMER_KEY);
    names.push(zoho_crm::REGION);
    // Batch H: Basecamp's account id is the first path segment of every URL it
    // renders and its `User-Agent` identifies the deployment to the provider, so
    // neither may be published as a Process input.
    names.push(basecamp::ACCOUNT_ID);
    names.push(basecamp::USER_AGENT);
    // Batch I: Mattermost's whole origin and Mailchimp's data-centre label are
    // each this connector's *host*, so no operation may publish either as a
    // Process input.
    names.push(mattermost::SERVER_ORIGIN);
    names.push(mailchimp::SERVER);
    // Batch K: GitLab's and Grafana's instance origins are each this
    // connector's *host*; Bitbucket's account address is its HTTP Basic
    // username; PagerDuty's `From` is the account user every write is
    // attributed to. None may be published as a Process input.
    names.push(gitlab::INSTANCE_ORIGIN);
    names.push(grafana::INSTANCE_ORIGIN);
    names.push(bitbucket::ACCOUNT_EMAIL);
    names.push(pagerduty::FROM_EMAIL);
    // Batch L, the forms half: Jotform's region *is* this connector's host,
    // chosen from a compiled table, so no operation may publish it as a Process
    // input.
    names.push(jotform::REGION);
    // Acuity's `user_id` is deliberately absent from this list for the reason
    // Zendesk's `email` is: it is a *credential field* name rather than an
    // input slot, and `user_id` is an ordinary operation input on another
    // provider in this workspace (Telegram's chat member read takes one). A
    // reserved name here is a name no connector may publish anywhere, which is
    // the wrong shape for a value that only ever reaches the credential.
    // `crates/connectors/tests/acuity.rs` proves it per operation instead.
    // Batch L, the scheduling and people half: Harvest's account id is a
    // `Harvest-Account-Id` header on every request and its `User-Agent`
    // identifies the deployment to the provider, so neither may be published as
    // a Process input.
    names.push(harvest::ACCOUNT_ID);
    names.push(harvest::USER_AGENT);
    // BambooHR's company subdomain *is* this connector's host, so no operation
    // may publish it as a Process input either.
    names.push(bamboohr::COMPANY_DOMAIN);
    // Clockify's workspace is the first scoped segment of every path it
    // renders, so no operation may publish it as a Process input.
    names.push(clockify::WORKSPACE_ID);
    // Eventbrite's organization is a path segment of its event collection and
    // its event create, so no operation may publish it as a Process input.
    names.push(eventbrite::ORGANIZATION_ID);
    names
}

/// Every operation a deployment could enable, per module, as the runtime
/// compiles them.
fn executable_operations() -> Vec<(&'static str, Vec<Operation>)> {
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

/// The contract a Process binds never names a value the connector fills itself.
///
/// Every one of these names is refused as input by the module that reads it —
/// an Airtable base, a Twilio Account SID, an AWS bucket or queue, a durable
/// activity's own deduplication id — so publishing one would publish a field
/// whose only possible value is a failure.
#[test]
fn no_projected_input_names_a_value_the_connector_supplies() {
    let reserved = deploy_time_names();
    for (module, operations) in executable_operations() {
        assert!(
            !operations.is_empty(),
            "`{module}` publishes at least one executable operation"
        );
        for operation in operations {
            let projection = operation.project();
            for input in projection.inputs() {
                assert!(
                    !reserved.contains(&input.name()),
                    "`{module}.{}` publishes deploy-time value `{}` as a Process input",
                    projection.id(),
                    input.name()
                );
            }
        }
    }
}

/// An executable operation publishes an output contract, because an activity
/// whose output schema is empty gives a Process nothing to read
/// (`knowledgebase/declarative-saas/decisions/029-*`).
///
/// The one admitted exception is an operation whose every documented success
/// carries no body at all: there, an empty output is the provider's answer
/// rather than a declaration nobody finished.
#[test]
fn every_executable_operation_projects_an_output_contract() {
    for (module, operations) in executable_operations() {
        for operation in operations {
            let projection = operation.project();
            let documented_empty = projection
                .success_statuses()
                .iter()
                .all(|status| operation.is_no_content_success(*status));
            assert!(
                !projection.outputs().is_empty() || documented_empty,
                "`{module}.{}` publishes no output a Process could read",
                projection.id()
            );
            assert!(
                projection.deadline() > Duration::ZERO,
                "`{module}.{}` declares a positive deadline",
                projection.id()
            );
            assert!(
                projection.is_executable(),
                "`{module}.{}` projects the class it was admitted on",
                projection.id()
            );
        }
    }
}

/// The one operation whose class carries a key binds it in the request, and the
/// projection carries the evidence a durable activity needs to hold its send
/// inside the provider's own retention window.
#[test]
fn the_explicit_key_operation_projects_its_binding_and_retention() {
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
    let send = sqs
        .operation("message.send")
        .expect("a FIFO queue publishes the deduplicated send");
    let projection = send.project();

    assert_eq!(
        projection.effect_class(),
        Some(EffectClass::ProviderIdempotentExplicitKey)
    );
    let evidence = projection
        .explicit_key()
        .expect("the class carries the evidence it was admitted on");
    assert!(evidence.retention().clock_safety_margin() < evidence.retention().minimum());
    assert!(
        projection
            .inputs()
            .iter()
            .all(|input| input.name() != "deduplication_id"),
        "the deduplication id is the activity's own key, never a caller's"
    );
}
