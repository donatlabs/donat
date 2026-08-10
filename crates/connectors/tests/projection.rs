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
    airtable, aws, aws_sqs, bamboohr, basecamp, bitbucket, clockify, eventbrite, freshdesk, gitlab,
    grafana, harvest, jotform, mailchimp, mattermost, pagerduty, salesforce, shopify, twilio,
    woocommerce, zendesk, zoho_crm,
};
use donat_connectors::sdk::EffectClass;

mod declarations_support;

use declarations_support::executable_operations;

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
