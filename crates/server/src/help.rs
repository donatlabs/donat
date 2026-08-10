//! `donat help` — the integration surface, read out of the declarations that
//! implement it.
//!
//! Every line this prints is derived from the compiled module table and the
//! local capability registry at the moment it runs. Nothing here is a second
//! description maintained beside the first: a connector added to the table
//! appears in the help with no edit to this file, and an operation whose class
//! or contract changes says so the next time somebody asks. A hand-written
//! catalogue would have been quicker and would have started drifting on the
//! first connector nobody remembered to document.
//!
//! What it deliberately does not print: any credential value, and any header
//! or query *value* an auth plan applies. A plan is named
//! ([`AuthPlan::label`]) rather than spelled out, so a deployment's help output
//! carries nothing that looks like a secret and nothing worth redacting.
//!
//! Per-deployment modules — Twilio, Jira, Zendesk and the rest whose
//! declaration needs configuration before it exists — can be listed but not
//! expanded, because their operation list is a function of a deployment this
//! command does not have. They say so rather than appearing empty.

use std::fmt::Write as _;

use donat_connectors::local::{self, LocalCapability};
use donat_connectors::sdk::{Connector, CredentialApplication, FieldClassification, OriginSpec};

use crate::connectors::{ModuleDeclaration, compiled_modules};

/// What the operator asked to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Topic {
    /// The table of contents.
    Root,
    /// Every connector, one line each.
    Connectors,
    /// One connector in full.
    Connector(String),
    /// Every local capability, one line each.
    Capabilities,
    /// One local capability in full.
    Capability(String),
    /// Everything, in one pass — the whole surface as a single document.
    Everything,
}

impl Topic {
    /// The topic named by the positional words after `donat help`.
    ///
    /// `donat help connectors github` and `donat help github` both reach the
    /// connector: an operator who knows the module name should not have to
    /// know which section it lives in.
    pub fn parse(path: &[String]) -> Result<Self, HelpError> {
        match path {
            [] => Ok(Self::Root),
            [section] if section == "all" => Ok(Self::Everything),
            [section] if section == "connectors" => Ok(Self::Connectors),
            [section] if section == "capabilities" => Ok(Self::Capabilities),
            [name] => Self::guess(name),
            [section, name] if section == "connectors" => Ok(Self::Connector(name.clone())),
            [section, name] if section == "capabilities" => Ok(Self::Capability(name.clone())),
            _ => Err(HelpError::UnknownTopic(path.join(" "))),
        }
    }

    fn guess(name: &str) -> Result<Self, HelpError> {
        if compiled_modules()
            .iter()
            .any(|module| module.declaration().name() == name)
        {
            return Ok(Self::Connector(name.to_owned()));
        }
        if local::capabilities()
            .iter()
            .any(|capability| capability.name() == name)
        {
            return Ok(Self::Capability(name.to_owned()));
        }
        Err(HelpError::UnknownTopic(name.to_owned()))
    }
}

/// The rendering, which changes the punctuation and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Text,
    Markdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpError {
    UnknownTopic(String),
}

impl std::fmt::Display for HelpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTopic(name) => write!(
                formatter,
                "no help topic named `{name}`; try `donat help` for the contents"
            ),
        }
    }
}

impl std::error::Error for HelpError {}

/// Render one topic.
pub fn render(topic: &Topic, format: Format) -> Result<String, HelpError> {
    let mut out = String::new();
    match topic {
        Topic::Root => root(&mut out, format),
        Topic::Connectors => connector_index(&mut out, format),
        Topic::Capabilities => capability_index(&mut out, format),
        Topic::Connector(name) => {
            let module = compiled_modules()
                .iter()
                .find(|module| module.declaration().name() == name)
                .ok_or_else(|| HelpError::UnknownTopic(name.clone()))?;
            connector(&mut out, module.declaration(), format);
        }
        Topic::Capability(name) => {
            let capability = local::capabilities()
                .iter()
                .find(|capability| capability.name() == name)
                .ok_or_else(|| HelpError::UnknownTopic(name.clone()))?;
            capability_detail(&mut out, capability, format);
        }
        // Every page in one pass. This is what `--format markdown` is for:
        // one command produces the whole reference for a build, and because it
        // walks the same tables as every other topic it cannot describe a
        // connector this binary does not have.
        Topic::Everything => {
            root(&mut out, format);
            connector_index(&mut out, format);
            for module in compiled_modules() {
                connector(&mut out, module.declaration(), format);
            }
            capability_index(&mut out, format);
            for capability in local::capabilities() {
                capability_detail(&mut out, capability, format);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

fn heading(out: &mut String, format: Format, level: usize, text: &str) {
    match format {
        Format::Markdown => {
            let _ = writeln!(out, "\n{} {text}\n", "#".repeat(level));
        }
        Format::Text => {
            let _ = writeln!(out, "\n{text}");
            if level <= 2 {
                let _ = writeln!(out, "{}", "-".repeat(text.len()));
            }
        }
    }
}

fn code(out: &mut String, format: Format, body: &str) {
    match format {
        Format::Markdown => {
            let _ = writeln!(out, "\n```yaml\n{}\n```", body.trim_end());
        }
        Format::Text => {
            for line in body.trim_end().lines() {
                let _ = writeln!(out, "    {line}");
            }
        }
    }
}

fn root(out: &mut String, format: Format) {
    let modules = compiled_modules();
    let capabilities = local::capabilities();
    heading(out, format, 1, "donat — the integration surface");
    let _ = writeln!(
        out,
        "\nEverything below is read from this binary's own declarations, so it\n\
         describes exactly what this build can do.\n"
    );
    let _ = writeln!(
        out,
        "  connectors ({})   providers this engine can call, and their operations",
        modules.len()
    );
    let _ = writeln!(
        out,
        "  capabilities ({})  work done in-process, with no provider and no network",
        capabilities.len()
    );
    let _ = writeln!(
        out,
        "\nRead one with `donat help connectors <name>` or `donat help capabilities <name>`;\n\
         a bare `donat help <name>` finds either. `donat help all --format markdown`\n\
         writes the whole reference as one document.\n"
    );
    let _ = writeln!(
        out,
        "An operation is only callable from a Process activity. Nothing here is reachable\n\
         over GraphQL, REST, or MCP, and no configuration of it happens at runtime."
    );
}

fn connector_index(out: &mut String, format: Format) {
    heading(out, format, 1, "Connectors");
    let modules = compiled_modules();
    let _ = writeln!(
        out,
        "\n{} modules compiled into this binary.\n",
        modules.len()
    );
    if format == Format::Markdown {
        let _ = writeln!(out, "| module | operations | credential |");
        let _ = writeln!(out, "|---|---|---|");
    }
    for module in modules {
        let (count, credential) = match module.declaration() {
            ModuleDeclaration::Static(connector) => (
                connector
                    .operations()
                    .iter()
                    .filter(|operation| operation.is_executable())
                    .count()
                    .to_string(),
                application(connector).to_owned(),
            ),
            ModuleDeclaration::PerDeployment { .. } => {
                ("per deployment".to_owned(), "per deployment".to_owned())
            }
        };
        match format {
            Format::Markdown => {
                let _ = writeln!(
                    out,
                    "| `{}` | {count} | {credential} |",
                    module.declaration().name()
                );
            }
            Format::Text => {
                let _ = writeln!(
                    out,
                    "  {:<20} {:>14}   {credential}",
                    module.declaration().name(),
                    count
                );
            }
        }
    }
}

fn application(connector: &Connector) -> &'static str {
    match connector.credential().application() {
        CredentialApplication::Plan(plan) => plan.label(),
        CredentialApplication::DeploymentDeclaredHeaders => "headers the deployment declares",
    }
}

fn connector(out: &mut String, declaration: &ModuleDeclaration, format: Format) {
    let name = declaration.name();
    heading(out, format, 1, &format!("Connector `{name}`"));

    let ModuleDeclaration::Static(connector) = declaration else {
        let _ = writeln!(
            out,
            "\nThis connector's declaration is completed by deploy-time configuration —\n\
             an account identifier, a tenant host, or a region — so its operations exist\n\
             only once a deployment has named them. Configure an instance and run\n\
             `donat validate` to see it accepted; there is nothing this command can\n\
             expand without that configuration."
        );
        return;
    };

    let _ = writeln!(out, "\nversion:    {}", connector.version());
    let _ = writeln!(out, "origin:     {}", origin(connector.origin()));
    let _ = writeln!(out, "credential: {}", application(connector));

    let fields = connector.credential().fields();
    if !fields.is_empty() {
        let _ = writeln!(out, "\nCredential fields:");
        for field in fields {
            let kind = match field.classification() {
                FieldClassification::Secret => "secret; redacted everywhere",
                FieldClassification::NonSecret => "not secret; deploy-time configuration",
            };
            let _ = writeln!(out, "  {:<24} {kind}", field.name());
        }
    }

    heading(out, format, 2, "Operations");
    let _ = writeln!(out, "\n`*` marks an input the operation requires.");
    let executable: Vec<_> = connector
        .operations()
        .iter()
        .filter(|operation| operation.is_executable())
        .collect();
    if executable.is_empty() {
        let _ = writeln!(
            out,
            "\nNone yet. Every declared operation of this connector is inventory-only:\n\
             classified, but with no idempotency evidence that would let a Process run it."
        );
    }
    for operation in &executable {
        let projection = operation.project();
        let effect = projection
            .effect_class()
            .map_or_else(|| "unclassified".to_owned(), |class| format!("{class:?}"));
        let inputs: Vec<String> = projection
            .inputs()
            .iter()
            .map(|input| {
                format!(
                    "{}{}: {}",
                    input.name(),
                    if input.required() { "*" } else { "" },
                    scalar(input.scalar())
                )
            })
            .collect();
        let outputs: Vec<String> = projection
            .outputs()
            .iter()
            .map(|output| format!("{}: {}", output.name(), scalar(output.scalar())))
            .collect();

        match format {
            // Indented lines are not a code block in Markdown, so the text
            // layout would reflow into one paragraph per operation. A heading
            // and a list is the same information in the shape the format has
            // for it.
            Format::Markdown => {
                let _ = writeln!(out, "\n### `{}`\n", projection.id());
                let _ = writeln!(
                    out,
                    "`{} {}`\n",
                    projection.method(),
                    projection.path_template()
                );
                let _ = writeln!(
                    out,
                    "- effect: `{effect}`, deadline {:?}",
                    projection.deadline()
                );
                if !inputs.is_empty() {
                    let _ = writeln!(out, "- input: `{}`", inputs.join("`, `"));
                }
                if !outputs.is_empty() {
                    let _ = writeln!(out, "- output: `{}`", outputs.join("`, `"));
                }
            }
            Format::Text => {
                let _ = writeln!(out, "\n  {}", projection.id());
                let _ = writeln!(
                    out,
                    "    {} {}",
                    projection.method(),
                    projection.path_template()
                );
                let _ = writeln!(
                    out,
                    "    effect: {effect}    deadline: {:?}",
                    projection.deadline()
                );
                if !inputs.is_empty() {
                    let _ = writeln!(out, "    input:  {}", inputs.join(", "));
                }
                if !outputs.is_empty() {
                    let _ = writeln!(out, "    output: {}", outputs.join(", "));
                }
            }
        }
    }

    // The example is generated, so it names this connector's own credential
    // fields and one of its own operations rather than a shape someone hoped
    // was still current.
    heading(out, format, 2, "Configure an instance");
    let mut example = format!("connectors:\n  - name: my_{name}\n    module: {name}\n");
    if !fields.is_empty() {
        let secrets: Vec<_> = fields.iter().filter(|field| field.is_secret()).collect();
        let settings: Vec<_> = fields.iter().filter(|field| !field.is_secret()).collect();
        if !settings.is_empty() {
            example.push_str("    settings:\n");
            for field in settings {
                let _ = writeln!(example, "      {}: \"…\"", field.name());
            }
        }
        if !secrets.is_empty() {
            example.push_str("    secrets:\n");
            for field in secrets {
                let _ = writeln!(
                    example,
                    "      {}:\n        from_env: {}",
                    field.name(),
                    env_name(name, field.name())
                );
            }
        }
    }
    code(out, format, &example);

    if let Some(operation) = executable.first() {
        let projection = operation.project();
        heading(out, format, 2, "Call it from a Process");
        let mut call = format!(
            "activities:\n  - name: {}\n    connector: my_{name}\n    operation: {}\n",
            projection.id().replace('.', "_"),
            projection.id()
        );
        let required: Vec<_> = projection
            .inputs()
            .iter()
            .filter(|input| input.required())
            .collect();
        if !required.is_empty() {
            call.push_str("    input:\n");
            for input in required {
                let _ = writeln!(call, "      {}: \"…\"", input.name());
            }
        }
        code(out, format, &call);
    }
}

fn env_name(module: &str, field: &str) -> String {
    format!("{module}_{field}").to_uppercase()
}

fn origin(spec: &OriginSpec) -> String {
    match spec {
        OriginSpec::Fixed(origin) => origin.as_str().to_owned(),
        OriginSpec::TemplatedHost(_) => {
            "a host this deployment names, from a compiled template".to_owned()
        }
        OriginSpec::DeploymentOrigin { key } => format!("named by configuration key `{key}`"),
    }
}

/// A declared scalar, spelled the way metadata spells it.
///
/// Taken as `Debug` rather than as `ValueScalar` so this module does not
/// need a dependency on the value contract to print a type name.
fn scalar(scalar: &impl std::fmt::Debug) -> String {
    format!("{scalar:?}").to_lowercase()
}

fn capability_index(out: &mut String, format: Format) {
    heading(out, format, 1, "Local capabilities");
    let capabilities = local::capabilities();
    let _ = writeln!(
        out,
        "\nWork the engine does in its own process: no provider, no network, no credential.\n\
         {} of them.\n",
        capabilities.len()
    );
    if format == Format::Markdown {
        let _ = writeln!(out, "| capability | operations |");
        let _ = writeln!(out, "|---|---|");
    }
    for capability in capabilities {
        match format {
            Format::Markdown => {
                let _ = writeln!(
                    out,
                    "| `{}` | {} |",
                    capability.name(),
                    capability.operations().len()
                );
            }
            Format::Text => {
                let _ = writeln!(
                    out,
                    "  {:<20} {:>3} operations",
                    capability.name(),
                    capability.operations().len()
                );
            }
        }
    }
}

/// One capability, with the bounds each of its operations is held to.
///
/// The bounds are the whole point of printing this: a local operation has no
/// provider to refuse it, so what stops a bad input is a declared ceiling, and
/// an operator sizing a job needs to read those before running it rather than
/// after being refused. `unit` is printed with `max_units` because a count
/// with no unit is a number nobody can act on.
fn capability_detail(out: &mut String, capability: &LocalCapability, format: Format) {
    heading(
        out,
        format,
        1,
        &format!("Capability `{}`", capability.name()),
    );
    let _ = writeln!(out, "\nversion: {}", capability.version());
    let _ = writeln!(
        out,
        "\nRuns in the engine's own process — no provider, no network, no credential."
    );
    heading(out, format, 2, "Operations");
    for operation in capability.operations() {
        let bounds = operation.bounds();
        match format {
            Format::Markdown => {
                let _ = writeln!(out, "\n### `{}`\n", operation.id());
                let _ = writeln!(out, "- effect: `{:?}`", operation.effect_class());
                let _ = writeln!(out, "- deadline: {:?}", bounds.cpu_deadline());
                let _ = writeln!(
                    out,
                    "- input at most {}, output at most {}",
                    bytes(bounds.max_input_bytes()),
                    bytes(bounds.max_output_bytes())
                );
                let _ = writeln!(out, "- at most {} {}", bounds.max_units(), bounds.unit());
            }
            Format::Text => {
                let _ = writeln!(out, "\n  {}", operation.id());
                let _ = writeln!(
                    out,
                    "    effect: {:?}    deadline: {:?}",
                    operation.effect_class(),
                    bounds.cpu_deadline()
                );
                let _ = writeln!(
                    out,
                    "    input:  at most {}",
                    bytes(bounds.max_input_bytes())
                );
                let _ = writeln!(
                    out,
                    "    output: at most {}, and at most {} {}",
                    bytes(bounds.max_output_bytes()),
                    bounds.max_units(),
                    bounds.unit()
                );
            }
        }
    }
}

/// A byte ceiling an operator can read at a glance.
fn bytes(value: usize) -> String {
    const MIB: usize = 1024 * 1024;
    const KIB: usize = 1024;
    if value >= MIB && value.is_multiple_of(MIB) {
        format!("{} MiB", value / MIB)
    } else if value >= MIB {
        format!("{:.1} MiB", value as f64 / MIB as f64)
    } else if value >= KIB {
        format!("{} KiB", value / KIB)
    } else {
        format!("{value} B")
    }
}
