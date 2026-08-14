//! Managing the identity provider's accounts, without a deployment writing
//! any of it down.
//!
//! "Who can get in" is the platform's question rather than an application's,
//! and the answer is never rows in this database — it is accounts in the
//! provider, reachable over its admin API. Putting that API behind GraphQL is
//! what lets a panel render it exactly the way it renders a table
//! (`knowledgebase/platform/decisions/001-*`), and doing it in metadata takes
//! forty lines of YAML that every deployment would copy and some would copy
//! wrong.
//!
//! So the declaration ships in the binary (`idp_admin.yaml`) and this module
//! fills in the three things that are a deployment's to say: where the
//! provider is, the key to reach it with, and which role may use it. That is
//! the same bargain the connector catalogue makes — an adapter for one named
//! provider is code, and a provider that is not that one is still ordinary
//! metadata.
//!
//! **It grants nothing.** The fields exist only when a deployment configures a
//! key, they are visible only to the one role it names, and that role is
//! established the way every role is: by a verified token or an authentication
//! hook. There is no admin role here and no bypass — see
//! `knowledgebase/api-surfaces/decisions/013-*`.

use donat_metadata::{ActionEntry, ActionHeader, ActionPermission, CustomTypes};
use serde::Deserialize;

/// The declaration itself, reviewable as the YAML it is.
const DECLARATION: &str = include_str!("idp_admin.yaml");

#[derive(Deserialize)]
struct Declaration {
    #[serde(default)]
    actions: Vec<ActionEntry>,
    #[serde(default)]
    custom_types: CustomTypes,
}

/// What a deployment has to say for these fields to exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdpAdmin {
    /// The provider's admin API — for Rauthy, `<origin>/auth/v1`.
    pub api: String,
    /// The whole `Authorization` header value, e.g. `API-Key name$secret`.
    pub key: String,
    /// The one role allowed to manage accounts. Naming it grants nothing on
    /// its own: the role still has to be in a caller's verified token.
    pub role: String,
}

/// Build the fields for this configuration.
///
/// Fails only if the built-in declaration itself is unreadable, which would be
/// a bug in this binary rather than in a deployment — so it is worth refusing
/// at boot rather than discovering per request.
pub fn module(config: &IdpAdmin) -> Result<(Vec<ActionEntry>, CustomTypes), String> {
    let parsed: Declaration = serde_yaml::from_str(DECLARATION)
        .map_err(|error| format!("the built-in identity declaration does not parse: {error}"))?;

    let actions = parsed
        .actions
        .into_iter()
        .map(|mut action| {
            action.definition.handler = Some(config.api.clone());
            // A literal rather than `value_from_env`: the key arrives inside
            // `DONAT_OIDC`, and re-exporting it under a second name would be
            // one more place it can leak from.
            action.definition.headers = vec![ActionHeader {
                name: "Authorization".to_string(),
                value: Some(config.key.clone()),
                value_from_env: None,
            }];
            action.permissions = vec![ActionPermission {
                role: config.role.clone(),
            }];
            action
        })
        .collect();

    Ok((actions, parsed.custom_types))
}

/// Add the fields to loaded metadata.
///
/// A deployment that declares its own action of the same name keeps it: the
/// built-in module is a default, and a metadata file is a deliberate
/// statement. Its custom types are added the same way.
pub fn extend(metadata: &mut donat_metadata::Metadata, config: &IdpAdmin) -> Result<(), String> {
    let (actions, types) = module(config)?;

    for action in actions {
        if metadata
            .actions
            .iter()
            .any(|existing| existing.name == action.name)
        {
            tracing::info!(
                target: "donat::auth",
                action = %action.name,
                "metadata declares this identity field itself; leaving it alone"
            );
            continue;
        }
        metadata.actions.push(action);
    }

    let existing_objects: Vec<String> = metadata
        .custom_types
        .objects
        .iter()
        .map(|object| object.name.clone())
        .collect();
    for object in types.objects {
        if !existing_objects.contains(&object.name) {
            metadata.custom_types.objects.push(object);
        }
    }
    let existing_inputs: Vec<String> = metadata
        .custom_types
        .input_objects
        .iter()
        .map(|object| object.name.clone())
        .collect();
    for object in types.input_objects {
        if !existing_inputs.contains(&object.name) {
            metadata.custom_types.input_objects.push(object);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> IdpAdmin {
        IdpAdmin {
            api: "http://idp:8080/auth/v1".to_string(),
            key: "API-Key donat$secret".to_string(),
            role: "support".to_string(),
        }
    }

    /// Nothing in the built-in declaration is declared twice.
    ///
    /// Two people adding the same field to this file is not hypothetical — it
    /// happened, and the engine refused to boot with a duplicate root while
    /// the test that should have caught it only listed action names, so a
    /// duplicated *type* went through. Names and types both, by construction.
    #[test]
    fn nothing_is_declared_twice() {
        let (actions, types) = module(&config()).expect("the declaration parses");

        let mut seen = std::collections::BTreeSet::new();
        for action in &actions {
            assert!(seen.insert(action.name.clone()), "{} twice", action.name);
        }

        let mut named = std::collections::BTreeSet::new();
        for object in &types.objects {
            assert!(named.insert(object.name.clone()), "{} twice", object.name);
        }
        for input in &types.input_objects {
            assert!(named.insert(input.name.clone()), "{} twice", input.name);
        }
    }

    #[test]
    fn the_built_in_declaration_is_readable() {
        let (actions, types) = module(&config()).expect("the declaration parses");
        let names: Vec<&str> = actions.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                // The people, and then the three things a provider decides
                // about them: what a token may say, who belongs to what, and
                // what an application may ask for.
                "idp_users",
                "idp_user",
                "idp_user_update",
                "idp_user_create",
                "idp_user_delete",
                "idp_roles",
                "idp_role_create",
                "idp_role_update",
                "idp_role_delete",
                "idp_groups",
                "idp_group_create",
                "idp_group_update",
                "idp_group_delete",
                "idp_scopes",
                "idp_scope_create",
                "idp_scope_update",
                "idp_scope_delete",
                "idp_clients",
                "idp_client",
                "idp_client_update",
                "idp_client_create",
                "idp_client_delete",
                "idp_client_secret",
                // What a deployment can say about a person beyond a name, who
                // is refused at the door, and who is signed in right now.
                "idp_user_attributes",
                "idp_user_attribute_create",
                "idp_user_attribute_update",
                "idp_user_attribute_delete",
                "idp_blocked_ips",
                "idp_blocked_ip_create",
                "idp_blocked_ip_delete",
                "idp_sessions",
                "idp_session_delete",
            ]
        );
        for object in [
            "IdpUser",
            "IdpUserDetail",
            "IdpRole",
            "IdpGroup",
            "IdpScope",
            "IdpClient",
            "IdpUserAttribute",
            "IdpBlockedIp",
            "IdpSession",
        ] {
            assert!(
                types.objects.iter().any(|o| o.name == object),
                "{object} is missing"
            );
        }
        for input in [
            "IdpUserInput",
            "IdpUserCreateInput",
            "IdpNameInput",
            "IdpClientInput",
            "IdpUserAttributeInput",
            "IdpBlockedIpInput",
        ] {
            assert!(
                types.input_objects.iter().any(|o| o.name == input),
                "{input} is missing"
            );
        }
    }

    #[test]
    fn every_field_reaches_the_configured_provider_with_the_configured_key() {
        let (actions, _) = module(&config()).expect("the declaration parses");
        for action in &actions {
            assert_eq!(
                action.definition.handler.as_deref(),
                Some("http://idp:8080/auth/v1"),
                "{}",
                action.name
            );
            let authorization = action
                .definition
                .headers
                .iter()
                .find(|header| header.name == "Authorization")
                .expect("the credential is attached");
            assert_eq!(authorization.value.as_deref(), Some("API-Key donat$secret"));
        }
    }

    #[test]
    fn every_field_is_visible_to_exactly_the_configured_role() {
        let (actions, _) = module(&config()).expect("the declaration parses");
        for action in &actions {
            let roles: Vec<&str> = action.permissions.iter().map(|p| p.role.as_str()).collect();
            // Not empty, which in this metadata format means "every role".
            assert_eq!(roles, vec!["support"], "{}", action.name);
        }
    }

    #[test]
    fn a_deployments_own_declaration_wins() {
        let mut metadata: donat_metadata::Metadata =
            serde_yaml::from_str("version: 3\nsources: []\n").expect("an empty metadata document");
        let mut mine: ActionEntry =
            serde_yaml::from_str("name: idp_users\ndefinition:\n  handler: http://mine\n")
                .expect("a hand-written action");
        mine.permissions = vec![ActionPermission {
            role: "operator".to_string(),
        }];
        metadata.actions.push(mine);

        extend(&mut metadata, &config()).expect("the module applies");

        let ours: Vec<&ActionEntry> = metadata
            .actions
            .iter()
            .filter(|a| a.name == "idp_users")
            .collect();
        assert_eq!(ours.len(), 1, "the built-in field was not added twice");
        assert_eq!(ours[0].definition.handler.as_deref(), Some("http://mine"));
    }
}
