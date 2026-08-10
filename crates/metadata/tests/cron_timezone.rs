//! A cron schedule may be declared in an IANA timezone (spec 021 §2). The
//! declaration is deploy-time data and is checked at load: an unknown zone, or
//! a zone without the two DST policies it needs, stops the boot instead of
//! becoming a schedule that fires at a time nobody chose.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use donat_metadata::{
    CronTrigger, DstRepeatedTime, DstSkippedTime, LoadError, Metadata, load_metadata_dir,
};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "donat_metadata_cron_tz_{tag}_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A metadata directory whose only interesting content is `cron_triggers.yaml`.
fn build(tag: &str, cron_triggers: &str) -> PathBuf {
    let dir = tempdir(tag);
    write(&dir, "version.yaml", "version: 3\n");
    write(&dir, "databases/databases.yaml", "[]\n");
    write(&dir, "cron_triggers.yaml", cron_triggers);
    dir
}

fn cron_error(result: Result<Metadata, LoadError>) -> String {
    match result {
        Err(LoadError::CronTriggers { message, .. }) => message,
        Err(other) => panic!("expected a cron trigger error, got {other}"),
        Ok(_) => panic!("expected a cron trigger error, but the metadata loaded"),
    }
}

#[test]
fn a_schedule_declares_its_zone_and_both_dst_policies() {
    let dir = build(
        "ok",
        "\
- name: send_reminders
  webhook: http://localhost/hook
  schedule: '0 9 * * 1-5'
  timezone: Europe/Berlin
  dst:
    skipped_time: fire_after_gap
    repeated_time: fire_at_second
",
    );
    let md = load_metadata_dir(&dir).expect("a fully declared zoned schedule loads");
    let t = &md.cron_triggers[0];
    assert_eq!(t.timezone.as_deref(), Some("Europe/Berlin"));
    let dst = t.dst.as_ref().expect("policies are kept");
    assert_eq!(dst.skipped_time, DstSkippedTime::FireAfterGap);
    assert_eq!(dst.repeated_time, DstRepeatedTime::FireAtSecond);
}

/// The Donat export has no `timezone` field at all, and everything already
/// deployed relies on the schedule meaning UTC. Loading such a trigger must
/// keep meaning exactly that.
#[test]
fn a_schedule_without_a_zone_stays_utc() {
    let dir = build(
        "utc",
        "\
- name: send_reminders
  webhook: http://localhost/hook
  schedule: '0 9 * * 1-5'
",
    );
    let md = load_metadata_dir(&dir).unwrap();
    assert!(md.cron_triggers[0].timezone.is_none());
    assert!(md.cron_triggers[0].dst.is_none());

    // And it round-trips without inventing the two new keys.
    let out = serde_json::to_value(&md.cron_triggers[0]).unwrap();
    let obj = out.as_object().unwrap();
    assert!(!obj.contains_key("timezone"));
    assert!(!obj.contains_key("dst"));
}

#[test]
fn an_unknown_zone_is_refused_at_load() {
    let dir = build(
        "badzone",
        "\
- name: t
  webhook: http://h
  schedule: '0 9 * * *'
  timezone: Europe/Berlim
  dst: { skipped_time: skip, repeated_time: fire_at_first }
",
    );
    let message = cron_error(load_metadata_dir(&dir));
    assert!(
        message.contains("Europe/Berlim"),
        "the message names the zone: {message}"
    );
}

#[test]
fn a_zone_without_declared_dst_policies_is_refused_at_load() {
    let dir = build(
        "nopolicy",
        "\
- name: t
  webhook: http://h
  schedule: '0 9 * * *'
  timezone: Europe/Berlin
",
    );
    let message = cron_error(load_metadata_dir(&dir));
    assert!(
        message.contains("dst"),
        "the message names what is missing: {message}"
    );
}

/// Both policies are stated or neither is: a half-declared `dst` is a
/// declaration the runtime would have to complete on the author's behalf.
#[test]
fn a_half_declared_dst_policy_is_refused_at_load() {
    let dir = build(
        "halfpolicy",
        "\
- name: t
  webhook: http://h
  schedule: '0 9 * * *'
  timezone: Europe/Berlin
  dst: { skipped_time: skip }
",
    );
    match load_metadata_dir(&dir) {
        Err(LoadError::Yaml { .. }) => {}
        other => panic!("expected a parse refusal, got {other:?}"),
    }
}

/// DST policies without a zone would never be consulted, and a declaration the
/// runtime ignores is a defect (ADR-034).
#[test]
fn dst_policies_without_a_zone_are_refused_at_load() {
    let dir = build(
        "policynozone",
        "\
- name: t
  webhook: http://h
  schedule: '0 9 * * *'
  dst: { skipped_time: skip, repeated_time: fire_at_first }
",
    );
    let message = cron_error(load_metadata_dir(&dir));
    assert!(
        message.contains("timezone"),
        "the message says what is missing: {message}"
    );
}

/// The spellings are part of the contract: they say how many runs happen and
/// when, so they are pinned here rather than left to the enum's field order.
#[test]
fn policy_spellings_are_stable() {
    let t: CronTrigger = serde_yaml::from_str(
        "\
name: t
webhook: http://h
schedule: '0 2 * * *'
timezone: Pacific/Auckland
dst: { skipped_time: fire_after_gap, repeated_time: fire_at_first }
",
    )
    .unwrap();
    let out = serde_yaml::to_string(&t.dst).unwrap();
    assert!(out.contains("skipped_time: fire_after_gap"), "{out}");
    assert!(out.contains("repeated_time: fire_at_first"), "{out}");

    let other: CronTrigger = serde_yaml::from_str(
        "\
name: t
webhook: http://h
schedule: '0 2 * * *'
timezone: Pacific/Auckland
dst: { skipped_time: skip, repeated_time: fire_at_second }
",
    )
    .unwrap();
    let out = serde_yaml::to_string(&other.dst).unwrap();
    assert!(out.contains("skipped_time: skip"), "{out}");
    assert!(out.contains("repeated_time: fire_at_second"), "{out}");
}
