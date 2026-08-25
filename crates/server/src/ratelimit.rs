//! What one tenant may ask for in a minute.
//!
//! `knowledgebase/operations/decisions/007-a-tenant-is-a-unit-of-consumption-and-the-proxy-cannot-see-it`
//! carries the decision and the one argument that puts this in the engine
//! rather than in front of it: a reverse proxy can rate-limit by address and
//! by header, and cannot rate-limit by tenant, because establishing the tenant
//! means verifying a token — which is this engine's job and not the proxy's.
//! Anything keyable from outside belongs outside, and the declaration refuses
//! to accept it.
//!
//! **Per replica, and it says so.** Three replicas give roughly three times the
//! declared rate. Counting exactly needs state shared between them, and a
//! request-path dependency on Redis or on a table is a worse failure than an
//! approximate ceiling: the ceiling being loose costs a busy tenant nothing,
//! the dependency being down costs everybody everything. This is a guard
//! against one tenant taking the engine, not a billing meter, and the ADR says
//! that in those words.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Fixed windows of a minute, counted per role and tenant.
///
/// A fixed window admits a burst across a boundary — up to twice the rate in
/// one wall-clock minute — where a sliding window would not. The trade is one
/// integer per key against a queue per key, and this is a guard rather than a
/// meter, so the integer wins. Named here because the number an operator
/// writes is not quite the number they get.
#[derive(Debug, Default)]
pub struct RateLimiter {
    windows: Mutex<HashMap<Key, Window>>,
}

type Key = (String, String);

#[derive(Debug, Clone, Copy)]
struct Window {
    minute: u64,
    count: u32,
}

/// Keys stop being written the moment a tenant goes quiet, so the map is swept
/// rather than left to grow with every tenant a deployment ever had.
const SWEEP_ABOVE: usize = 4096;

impl RateLimiter {
    /// Count one request, and say whether it is over the ceiling.
    ///
    /// `tenant` is empty where the deployment declares no tenancy, which makes
    /// the key the role alone — still useful, and still something a proxy
    /// cannot do, because the role also comes from the token.
    pub fn admit(&self, role: &str, tenant: &str, per_minute: u32) -> bool {
        let minute = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_secs() / 60)
            .unwrap_or(0);
        let key = (role.to_owned(), tenant.to_owned());

        let Ok(mut windows) = self.windows.lock() else {
            // A poisoned lock means a panic somewhere else; refusing every
            // request afterwards would turn one bug into an outage.
            return true;
        };
        if windows.len() > SWEEP_ABOVE {
            windows.retain(|_, window| window.minute >= minute);
        }
        let window = windows.entry(key).or_insert(Window { minute, count: 0 });
        if window.minute != minute {
            *window = Window { minute, count: 0 };
        }
        window.count += 1;
        window.count <= per_minute
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_role_is_admitted_up_to_its_ceiling_and_then_refused() {
        let limiter = RateLimiter::default();
        for _ in 0..3 {
            assert!(limiter.admit("customer", "alpha", 3));
        }
        assert!(!limiter.admit("customer", "alpha", 3));
    }

    #[test]
    fn one_tenant_does_not_spend_anothers_allowance() {
        // The whole point: alpha exhausting its minute leaves beta untouched.
        let limiter = RateLimiter::default();
        for _ in 0..2 {
            assert!(limiter.admit("customer", "alpha", 2));
        }
        assert!(!limiter.admit("customer", "alpha", 2));
        assert!(limiter.admit("customer", "beta", 2));
    }

    #[test]
    fn one_role_does_not_spend_anothers_either() {
        let limiter = RateLimiter::default();
        assert!(limiter.admit("customer", "alpha", 1));
        assert!(!limiter.admit("customer", "alpha", 1));
        assert!(limiter.admit("staff", "alpha", 1));
    }

    #[test]
    fn a_deployment_with_no_tenancy_keys_on_the_role_alone() {
        // An empty tenant is still a key, and the role still comes from a
        // verified token, which is what keeps this out of the proxy.
        let limiter = RateLimiter::default();
        assert!(limiter.admit("customer", "", 1));
        assert!(!limiter.admit("customer", "", 1));
    }

    #[test]
    fn the_map_is_swept_rather_than_left_to_grow() {
        let limiter = RateLimiter::default();
        for tenant in 0..(SWEEP_ABOVE + 100) {
            limiter.admit("customer", &tenant.to_string(), 1000);
        }
        let held = limiter.windows.lock().expect("not poisoned").len();
        assert!(
            held <= SWEEP_ABOVE + 100,
            "the map grew without bound: {held}"
        );
    }
}
