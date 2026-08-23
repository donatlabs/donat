//! Pethub: the Petshop store domain, served multitenant.
//!
//! One claim, and this suite is what makes it executable:
//!
//! > Multitenancy is an engine capability, not a domain concern. The Petshop
//! > business YAML is composed here **unchanged**. Everything that makes it
//! > multitenant lives in `examples/pethub/metadata` and in DDL.
//!
//! Nothing below reaches into `examples/petshop/metadata`, and nothing needs
//! to. The store keeps its own permissions — most of them `filter: {}`, which
//! is exactly the shape a hand-rolled tenancy gets wrong — and the compiler
//! bounds them.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use donat_conformance::{Running, Suite, apply_sql_migration_dir};
use serde_json::{Value as Json, json};

const SECRET: &str = "pethub-conformance-secret-key-32b+";
const ALPHA: &str = "store-alpha";
const BETA: &str = "store-beta";

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn token_for(role: &str, tenant: Option<&str>, subject: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut claims = json!({
        "sub": subject,
        "iat": now,
        "exp": now + 3600,
        "roles": [role],
    });
    if let Some(tenant) = tenant {
        claims["tenant"] = json!(tenant);
    }
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .expect("signing jwt")
}

fn bearer(token: &str) -> Vec<(String, String)> {
    vec![("Authorization".to_string(), format!("Bearer {token}"))]
}

/// Two stores, one plan that allows two products each, one product already in
/// each, and a member of each store holding every action inside it.
const SEED: &str = "
INSERT INTO public.plan (code, label, max_products) VALUES ('free', 'Free', 2);
INSERT INTO public.store (id, name, status, plan_code) VALUES
    ('store-alpha', 'Alpha Pets', 'active', 'free'),
    ('store-beta',  'Beta Pets',  'active', 'free');
INSERT INTO public.tenant_usage (tenant_id, product_count) VALUES
    ('store-alpha', 1), ('store-beta', 1);
INSERT INTO public.iam_grant (tenant_id, user_id, action) VALUES
    ('store-alpha', 'person-a', '*:*'),
    ('store-beta',  'person-b', '*:*');

-- The same slug in both stores. Under Petshop's own global unique constraint
-- the second of these would have been refused — and the refusal would have
-- told one merchant what another had named a product.
INSERT INTO public.category (slug, name, tenant_id) VALUES
    ('food', 'Food', 'store-alpha'),
    ('food', 'Food', 'store-beta');
INSERT INTO public.product (category_id, slug, title, status, tenant_id)
SELECT c.id, 'kibble', 'Alpha Kibble', 'published', 'store-alpha'
  FROM public.category c WHERE c.tenant_id = 'store-alpha';
INSERT INTO public.product (category_id, slug, title, status, tenant_id)
SELECT c.id, 'kibble', 'Beta Kibble', 'published', 'store-beta'
  FROM public.category c WHERE c.tenant_id = 'store-beta';

-- The same person, a customer of both stores.
INSERT INTO public.customer (customer_id, name, email, tenant_id) VALUES
    ('shopper-1', 'Shopper', 'shopper@example.com', 'store-alpha'),
    ('shopper-1', 'Shopper', 'shopper@example.com', 'store-beta');
INSERT INTO public.orders (customer_id, order_status, total_minor, tenant_id) VALUES
    ('shopper-1', 'paid', 1000, 'store-alpha'),
    ('shopper-1', 'paid', 2000, 'store-beta');
";

fn pethub_suite(name: &str) -> Running {
    let root = examples();
    let metadata = donat_metadata::load_metadata_dir(&root.join("pethub/metadata"))
        .expect("pethub metadata composes petshop and loads");
    let running = Suite::new(name)
        .initial_metadata(metadata)
        .with_migrations()
        .env(
            "DONAT_GRAPHQL_JWT_SECRET",
            &json!({
                "type": "HS256",
                "key": SECRET,
                "claims_map": {
                    "x-donat-allowed-roles": { "path": "$.roles" },
                    "x-donat-default-role": { "path": "$.roles[0]" },
                    "x-donat-tenant-id": { "path": "$.tenant", "default": "" },
                    "x-donat-user-id": { "path": "$.sub", "default": "" }
                }
            })
            .to_string(),
        )
        .env("PETSHOP_PAYMENT_BASE_URL", "http://127.0.0.1:9")
        .env("PETSHOP_PAYMENT_API_TOKEN", "pethub-test")
        .env("DONAT_MOCK_CARRIER_BASE_URL", "http://127.0.0.1:9")
        .env("DONAT_MOCK_CARRIER_TOKEN", "pethub-test")
        .env("PETSHOP_TAX_BASE_URL", "http://127.0.0.1:9")
        .env("PETSHOP_TAX_API_TOKEN", "pethub-test")
        .env("PETSHOP_NOTIFICATION_BASE_URL", "http://127.0.0.1:9")
        .env("PETSHOP_NOTIFICATION_API_TOKEN", "pethub-test")
        .env("PETSHOP_PAYOUT_BASE_URL", "http://127.0.0.1:9")
        .env("PETSHOP_PAYOUT_API_TOKEN", "pethub-test")
        .env("PETSHOP_FILE_SIGNING_SECRET", "pethub-test-file-signing")
        .env("PETSHOP_S3_KEY", "pethub-test-key")
        .env("PETSHOP_S3_SECRET", "pethub-test-secret")
        .start();

    // The store's DDL, then the platform's. The order is the whole story: a
    // platform adds a column to a domain it does not own, and adds it once.
    apply_sql_migration_dir(running.db_url(), &root.join("petshop/migrations")).unwrap();
    apply_sql_migration_dir(running.db_url(), &root.join("pethub/migrations")).unwrap();
    let mut client = postgres::Client::connect(running.db_url(), postgres::NoTls)
        .expect("connect to the pethub suite database");
    client.batch_execute(SEED).expect("seeding two stores");
    running
}

fn query(s: &Running, token: &str, body: Json) -> (u16, Json) {
    s.post("/v1/graphql", &body, &bearer(token))
}

fn titles(resp: &Json, root: &str) -> Vec<String> {
    let mut names: Vec<String> = resp["data"][root]
        .as_array()
        .unwrap_or_else(|| panic!("expected data.{root} in {resp}"))
        .iter()
        .map(|row| row["title"].as_str().expect("title").to_string())
        .collect();
    names.sort();
    names
}

/// The store's own roots, unchanged, serving one store each.
#[test]
fn the_store_domain_serves_one_store_per_caller() {
    let s = pethub_suite("pethub_isolation");
    let alpha = token_for("staff", Some(ALPHA), "person-a");
    let beta = token_for("staff", Some(BETA), "person-b");

    for (token, expected) in [(&alpha, "Alpha Kibble"), (&beta, "Beta Kibble")] {
        let (code, resp) = query(
            &s,
            token,
            json!({ "query": "query { product { title slug } }" }),
        );
        assert_eq!(code, 200, "{resp}");
        assert_eq!(
            titles(&resp, "product"),
            vec![expected.to_string()],
            "{resp}"
        );
    }

    // A view is a table to the engine: tracked, therefore scoped. `staff` reads
    // `order_operations` with `filter: {}` in Petshop's own metadata — the
    // shape that means "every row" until the compiler bounds it.
    for (token, expected) in [(&alpha, 1000_i64), (&beta, 2000)] {
        let (code, resp) = query(
            &s,
            token,
            json!({ "query": "query { order_operations { total_minor } }" }),
        );
        assert_eq!(code, 200, "{resp}");
        let rows = resp["data"]["order_operations"].as_array().expect("rows");
        assert_eq!(rows.len(), 1, "a view served another store's rows: {resp}");
        assert_eq!(rows[0]["total_minor"], expected, "{resp}");
    }
}

/// The store's own insert permission is untouched; the tenant column is a
/// preset the caller cannot express, and the plan's ceiling is a layer on top
/// of the same permission.
#[test]
fn a_write_stays_in_its_store_and_stops_at_the_plan() {
    let s = pethub_suite("pethub_writes");
    let alpha = token_for("staff", Some(ALPHA), "person-a");
    let beta = token_for("staff", Some(BETA), "person-b");

    let category_of = |token: &str| {
        let (code, resp) = query(&s, token, json!({ "query": "query { category { id } }" }));
        assert_eq!(code, 200, "{resp}");
        resp["data"]["category"][0]["id"]
            .as_i64()
            .expect("a category")
    };
    let alpha_category = category_of(&alpha);

    // The plan allows two and one exists, so the second fits.
    let (code, resp) = query(
        &s,
        &alpha,
        json!({ "query": format!(
            "mutation {{ insert_product(objects: [{{ category_id: {alpha_category}, \
             slug: \"treats\", title: \"Alpha Treats\", status: \"published\" }}]) \
             {{ affected_rows }} }}"
        )}),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(resp["data"]["insert_product"]["affected_rows"], 1, "{resp}");

    // The third does not, and says so rather than writing.
    let (code, resp) = query(
        &s,
        &alpha,
        json!({ "query": format!(
            "mutation {{ insert_product(objects: [{{ category_id: {alpha_category}, \
             slug: \"toys\", title: \"Alpha Toys\", status: \"published\" }}]) \
             {{ affected_rows }} }}"
        )}),
    );
    assert_eq!(code, 200, "{resp}");
    assert!(
        resp["errors"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no more products"),
        "the plan ceiling did not hold: {resp}"
    );

    // The other store is untouched, and still has its own ceiling.
    let (code, resp) = query(&s, &beta, json!({ "query": "query { product { title } }" }));
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        titles(&resp, "product"),
        vec!["Beta Kibble".to_string()],
        "{resp}"
    );
}

/// Grants are live in the example too: the same compiled role, the same store,
/// and a person nobody granted anything sees nothing.
#[test]
fn a_member_without_grants_sees_nothing_in_a_store_they_are_in() {
    let s = pethub_suite("pethub_grants");
    let ungranted = token_for("staff", Some(ALPHA), "person-nobody");

    let (code, resp) = query(
        &s,
        &ungranted,
        json!({ "query": "query { product { title } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert!(
        titles(&resp, "product").is_empty(),
        "a role with no grant read a store's catalogue: {resp}"
    );
}

/// A request that cannot say which store it is in is refused, and no header
/// supplies one.
#[test]
fn a_request_without_a_store_is_refused() {
    let s = pethub_suite("pethub_no_tenant");
    let tenantless = token_for("staff", None, "person-a");

    let (code, resp) = query(
        &s,
        &tenantless,
        json!({ "query": "query { product { title } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["errors"][0]["extensions"]["code"], "access-denied",
        "{resp}"
    );
}

/// The acceptance criterion, as an assertion rather than an intention: the
/// store's metadata still loads on its own, knows nothing about tenants, and
/// is the same document Pethub composes.
#[test]
fn the_store_metadata_is_unchanged_and_still_stands_alone() {
    let root = examples();
    let petshop = donat_metadata::load_metadata_dir(&root.join("petshop/metadata"))
        .expect("the store still loads by itself");
    assert!(
        petshop.tenancy.is_none() && petshop.iam.is_none() && petshop.quotas.is_none(),
        "the store's own metadata learned about tenants"
    );

    let pethub = donat_metadata::load_metadata_dir(&root.join("pethub/metadata"))
        .expect("pethub composes it");
    assert!(pethub.tenancy.is_some(), "pethub declares tenancy");

    // Every table the store tracks is still tracked, with the permissions it
    // wrote — Pethub only adds.
    let store_tables: Vec<String> = petshop.sources[0]
        .tables
        .iter()
        .map(|entry| entry.table.to_string())
        .collect();
    let composed: Vec<String> = pethub.sources[0]
        .tables
        .iter()
        .map(|entry| entry.table.to_string())
        .collect();
    for table in &store_tables {
        assert!(
            composed.contains(table),
            "`{table}` was lost in composition"
        );
    }
    assert_eq!(
        composed.len(),
        store_tables.len() + 4,
        "pethub adds exactly its four platform tables"
    );
}

/// One person, two stores, one open cart each.
///
/// The store's `cart_one_open_per_customer` index is on `cart(customer_id)`
/// alone, and Pethub made `customer.customer_id` unique only *within* a store
/// — so the same shopper legitimately exists in both. Buying in one store then
/// blocks buying in the other, and the refusal tells the second store that
/// somebody somewhere already holds that id.
#[test]
fn a_shopper_can_hold_an_open_cart_in_each_store_they_shop_in() {
    let s = pethub_suite("pethub_cart_collision");
    let open_cart = json!({
        "query": "mutation { insert_cart(objects: [{}]) { affected_rows } }"
    });

    let alpha = token_for("customer", Some(ALPHA), "shopper-1");
    let (code, resp) = query(&s, &alpha, open_cart.clone());
    assert_eq!(code, 200, "{resp}");
    assert_eq!(resp["data"]["insert_cart"]["affected_rows"], 1, "{resp}");

    // The same person, the other store. Nothing about this write touches the
    // first store's row.
    let beta = token_for("customer", Some(BETA), "shopper-1");
    let (code, resp) = query(&s, &beta, open_cart);
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["data"]["insert_cart"]["affected_rows"], 1,
        "a shopper's cart in one store blocked their cart in another: {resp}"
    );
}
