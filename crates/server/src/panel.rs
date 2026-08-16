//! The UI, served by this engine when a deployment asks it to.
//!
//! **This grants nothing, and it is not an admin surface.** What is served here
//! is a directory of static files — HTML, JavaScript, CSS. The panel remains an
//! ordinary client of `/v1/graphql`: it holds no credential, its role comes
//! from a verified token like every other client's, and everything it can read
//! or write is an explicit per-role permission somebody wrote in YAML. Serving
//! its files changes where they are fetched from and nothing else.
//!
//! `knowledgebase/platform/decisions/001-*` rejected this, on two grounds. The
//! first — that the engine would grow a surface whose only purpose is
//! administration, and the next request would be "let it edit metadata" — is
//! about *power*, and none is granted here; that rule stands and is restated in
//! the amendment to that decision. The second, that this binary should not be
//! in the business of serving static assets, is what changed: the engine
//! already serves `/auth/login`, `/auth/callback` and the provider proxy, and
//! since the reset link it redirects a browser to a panel path. It already
//! assumes a panel at a known address. Serving it is a smaller assumption than
//! pointing at it.
//!
//! What it buys is the one thing the login work depends on: **one origin**. The
//! provider's session cookie is `__Host-`-prefixed and it compares `Origin`
//! against its own public URL, so the panel, the proxy and the engine have to
//! look like one address to a browser. Today that is a reverse proxy's job and
//! a deployment's `DONAT_UPSTREAM` to get right. Served from here, there is
//! nothing to get right.
//!
//! Unset `DONAT_UI_DIR` and none of this is mounted, which is the default.

use axum::Router;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

/// The environment variable naming the built panel's directory.
pub const DIR_ENV: &str = "DONAT_UI_DIR";

/// What this setting used to be called.
///
/// `DONAT_ADMIN_DIR` shipped in 0.6.0, and a deployment configured against that
/// release must not change behaviour because the name here got better.
///
/// It is the *stronger* of the two, which reads backwards until you notice
/// that the image sets `DONAT_UI_DIR` itself: a value under this name was
/// written by a person, a value under the current one may be the image's own
/// default. Whatever this one says — a directory or, emptied, nothing at all —
/// is therefore an instruction, and is followed.
pub const LEGACY_DIR_ENV: &str = "DONAT_ADMIN_DIR";

/// Paths that belong to the engine, whatever the panel's router thinks.
///
/// The panel is a single-page application: every path it does not have a file
/// for has to answer with `index.html`, or a link straight to one of its routes
/// is a 404. That rule must not reach the engine's own paths. Without this, a
/// mistyped `/v1/graphqlx` returns an HTML page and HTTP 200, and whoever
/// typed it goes looking for a bug in their client — the failure a wrong URL
/// should produce is a 404, loudly.
const ENGINE_ROOTS: &[&str] = &[
    "/v1",
    "/v1alpha1",
    "/v1beta1",
    "/api",
    "/mcp",
    "/auth",
    "/healthz",
    "/readyz",
];

/// True when this path is the engine's to answer, panel or no panel.
///
/// Matched on whole segments, not on characters: `/healthzz` and `/v1beta` are
/// nobody's endpoints, and claiming them for the engine would mean a panel
/// route could be shadowed by a name that merely starts the same way.
pub fn is_engine_path(path: &str) -> bool {
    ENGINE_ROOTS
        .iter()
        .any(|root| path == *root || path.starts_with(&format!("{root}/")))
}

/// Serve `dir` for everything the engine's own routes did not claim.
///
/// A fallback rather than a route, so the engine's paths always win: this runs
/// only when nothing else matched. `ServeDir` resolves the file, and anything
/// it has none for becomes `index.html` — the single-page application's own
/// routing takes it from there.
pub fn serve<S>(app: Router<S>, dir: &str) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let index = std::path::Path::new(dir).join("index.html");
    let files = ServeDir::new(dir).fallback(ServeFile::new(index));

    app.fallback(move |request: Request| {
        let files = files.clone();
        async move {
            if is_engine_path(request.uri().path()) {
                return StatusCode::NOT_FOUND.into_response();
            }
            match files.oneshot(request).await {
                Ok(response) => response.into_response(),
                // The directory is deployment configuration; a panel that
                // cannot be read is an operator's problem, and saying so beats
                // a blank page.
                Err(error) => {
                    tracing::warn!(target: "donat::panel", "the UI could not be read: {error}");
                    (StatusCode::INTERNAL_SERVER_ERROR, "the UI is not readable").into_response()
                }
            }
        }
    })
}

/// A `Response` for the mounted-or-not decision, so `main` reads as one line.
pub fn configured() -> Option<String> {
    configured_from(&|name: &str| std::env::var(name).ok())
}

/// The same decision, against any source of settings — which is what lets it
/// be tested without a process-wide environment.
pub fn configured_from(read: &impl Fn(&str) -> Option<String>) -> Option<String> {
    // The two names are not symmetric, because the image sets one of them
    // itself: `DONAT_UI_DIR` is present on every deployment whether or not
    // anybody asked for it, and `DONAT_ADMIN_DIR` is present only when a
    // person wrote it. So the old name is the one carrying an intention, and
    // it wins — otherwise an operator upgrading from 0.6.0 would edit the
    // setting they were told to use and watch nothing happen, which is the
    // failure `knowledgebase/operations/decisions/006-*` refuses configuration
    // for. That decision's answer to a fact named twice is to refuse at boot;
    // it cannot be applied literally here, because "named twice" is the normal
    // state of every upgraded deployment and refusing would break all of them
    // on a value the image supplied.
    //
    // An empty value is a deliberate "serve nothing" under either name. Naming
    // one empty and the other a path is a contradiction, and it resolves to
    // off — the only answer that cannot expose something nobody asked to
    // publish.
    let named = |name: &str| read(name).map(|value| value.trim().to_string());
    let (current, legacy) = (named(DIR_ENV), named(LEGACY_DIR_ENV));
    if current.as_deref() == Some("") || legacy.as_deref() == Some("") {
        return None;
    }
    legacy.or(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine's paths are the engine's, and a wrong one is a 404.
    ///
    /// This is the whole risk of serving a single-page application from an API:
    /// its fallback answers everything, so without a list like this a client
    /// with a typo in its URL gets an HTML page and a 200, which looks like a
    /// working endpoint returning nonsense.
    #[test]
    fn the_engines_own_paths_are_never_the_panels() {
        for path in [
            "/v1/graphql",
            "/v1/graphqlx",
            "/v1alpha1/graphql",
            "/v1beta1/relay",
            "/api/rest/anything",
            "/mcp",
            "/auth/login",
            "/auth/v1/oidc/authorize",
            "/healthz",
            "/readyz",
        ] {
            assert!(is_engine_path(path), "{path} belongs to the engine");
        }
    }

    /// And the panel's are the panel's, including the ones it reaches by a
    /// whole page load: an emailed reset link and the account screen the login
    /// hands over to are both navigations, not route changes.
    #[test]
    fn the_panels_own_paths_reach_the_panel() {
        for path in [
            "/",
            "/index.html",
            "/assets/index-abc123.js",
            "/idp/authorize",
            "/idp/reset/u-1/link-1",
            "/account",
            "/clients",
            "/users/42",
        ] {
            assert!(!is_engine_path(path), "{path} belongs to the panel");
        }
    }

    /// A path that merely starts with the same letters is not a prefix match.
    #[test]
    fn a_lookalike_path_is_not_the_engines() {
        assert!(!is_engine_path("/v1beta"));
        assert!(!is_engine_path("/authorize"));
        assert!(!is_engine_path("/healthzz"));
    }

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |name: &str| {
            owned
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    /// The setting, under the name it has now.
    #[test]
    fn a_directory_is_served_when_one_is_named() {
        assert_eq!(
            configured_from(&env(&[(DIR_ENV, " /usr/share/donat/ui ")])),
            Some("/usr/share/donat/ui".to_string())
        );
        assert_eq!(configured_from(&env(&[])), None);
    }

    /// And under the name 0.6.0 shipped, because a rename here is not a reason
    /// for somebody's deployment to stop serving its interface.
    #[test]
    fn the_name_it_used_to_have_still_works() {
        assert_eq!(
            configured_from(&env(&[(LEGACY_DIR_ENV, "/old/place")])),
            Some("/old/place".to_string())
        );
    }

    /// A directory named under the old name is served, even though the image
    /// names one under the current one.
    ///
    /// This is the upgrade that matters: `DONAT_UI_DIR` is in the image's own
    /// `ENV`, so it is set on every deployment. Letting it win would mean an
    /// operator who points 0.6.0's setting at their own build gets the stock
    /// interface instead, with nothing said — they would edit the variable
    /// they were told to edit and watch nothing happen.
    #[test]
    fn a_directory_someone_actually_named_beats_the_images_default() {
        assert_eq!(
            configured_from(&env(&[
                (DIR_ENV, "/usr/share/donat/ui"),
                (LEGACY_DIR_ENV, "/opt/my-panel"),
            ])),
            Some("/opt/my-panel".to_string())
        );
    }

    /// Emptying the current name means *serve nothing* — an intention the old
    /// name must not quietly overturn.
    #[test]
    fn switching_it_off_is_not_undone_by_the_old_name() {
        assert_eq!(
            configured_from(&env(&[(DIR_ENV, ""), (LEGACY_DIR_ENV, "/old")])),
            None
        );
    }

    /// The same intention written the way the previous version documented it,
    /// against the name this image sets for itself.
    ///
    /// This is the upgrade, not a hypothetical: `DONAT_UI_DIR` comes from the
    /// image's own `ENV`, so it is present on every deployment whether or not
    /// anyone asked for it. Reading it as an instruction would turn an
    /// interface back on that an operator switched off, during an upgrade they
    /// expected to change nothing.
    #[test]
    fn switching_it_off_by_its_old_name_still_switches_it_off() {
        assert_eq!(
            configured_from(&env(&[
                (DIR_ENV, "/usr/share/donat/ui"),
                (LEGACY_DIR_ENV, "")
            ])),
            None
        );
    }
}
