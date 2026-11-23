use super::*;
use serde_json::{json, Value};

fn setting(value: Value) -> Setting {
    let mut setting = Setting::default();
    setting.extra.insert("event_policy".into(), value);
    setting
}

fn policy(value: Value) -> EventPolicy {
    let mut policy = EventPolicy::new();
    policy.setting(&mut setting(value)).unwrap()();
    policy
}

fn configuration() -> Value {
    json!({
        "enabled": true,
        "allowed_kinds": [1, 5, 30023, 30078],
        "identifiers": {
            "30078": ["profile", "settings", "archive"]
        }
    })
}

fn event(kind: u16, tags: Vec<Vec<String>>) -> Event {
    Event::new([1; 32], [1; 32], 1, kind, tags, String::new(), [0; 64]).unwrap()
}

fn identifier(value: &str) -> Vec<Vec<String>> {
    tags(&[&["d", value]])
}

fn tags(tags: &[&[&str]]) -> Vec<Vec<String>> {
    tags.iter()
        .map(|tag| tag.iter().map(|value| value.to_string()).collect())
        .collect()
}

#[test]
fn enabled_empty_allowlist_denies_all_writes() {
    assert!(policy(json!({"enabled": true}))
        .check(&event(1, vec![]))
        .is_err());
}

#[test]
fn rejects_every_unlisted_kind() {
    let policy = policy(configuration());
    let allowed = [1, 5, 30023, 30078];
    for kind in 0..=u16::MAX {
        let event = event(kind, identifier("profile"));
        assert_eq!(
            policy.check(&event).is_ok(),
            allowed.contains(&kind),
            "kind {kind}"
        );
    }
}

#[test]
fn matches_the_first_d_tag_regardless_of_position() {
    let policy = policy(configuration());
    for tags in [
        vec![],
        tags(&[&[]]),
        tags(&[&["d"]]),
        identifier(""),
        identifier("unlisted"),
        tags(&[&["d", "unlisted"], &["d", "profile"]]),
        tags(&[&["d"], &["d", "profile"]]),
    ] {
        assert!(policy.check(&event(30078, tags)).is_err());
    }
    for tags in [
        tags(&[&["alt", "description"], &["d", "profile"]]),
        tags(&[&["d", "profile"], &["d"]]),
        tags(&[&["d", "profile"], &["d", "unlisted"]]),
    ] {
        assert!(policy.check(&event(30078, tags)).is_ok());
    }
    assert!(policy
        .check(&event(30023, identifier("any-identifier")))
        .is_ok());
}

#[test]
fn empty_identifier_matches_missing_or_empty_values() {
    let policy = policy(json!({
        "enabled": true,
        "allowed_kinds": [30078],
        "identifiers": {"30078": [""]}
    }));
    for tags in [vec![], tags(&[&["d"]]), identifier("")] {
        assert!(policy.check(&event(30078, tags)).is_ok());
    }
    assert!(policy.check(&event(30078, identifier("profile"))).is_err());
}

#[test]
fn invalid_or_uncommitted_settings_preserve_the_current_policy() {
    let mut policy = policy(configuration());
    let unchanged = |policy: &EventPolicy| {
        assert!(policy.check(&event(1, vec![])).is_ok());
        assert!(policy.check(&event(1059, vec![])).is_err());
    };
    for enabled in [false, true] {
        for invalid in [
            json!({"enabled": enabled, "allowed_kinds": "invalid"}),
            json!({"enabled": enabled, "allowed_kinds": [65536]}),
            json!({"enabled": enabled, "allowed_kinds": [1], "identifiers": {"30078": ["id"]}}),
            json!({"enabled": enabled, "allowed_kinds": [1], "identifiers": {"1": ["id"]}}),
            json!({"enabled": enabled, "enabeld": true}),
        ] {
            let error = policy.setting(&mut setting(invalid)).err().unwrap();
            assert!(error.to_string().contains("event_policy"), "{error}");
            unchanged(&policy);
        }
    }
    let mut candidate = setting(json!({"enabled": true, "allowed_kinds": [1059]}));
    drop(policy.setting(&mut candidate).unwrap());
    unchanged(&policy);
    policy.setting(&mut candidate).unwrap()();
    assert!(policy.check(&event(1, vec![])).is_err());
    assert!(policy.check(&event(1059, vec![])).is_ok());
}

#[test]
fn missing_or_disabled_policy_preserves_existing_relay_behavior() {
    let mut missing = EventPolicy::new();
    missing.setting(&mut Setting::default()).unwrap()();
    assert!(missing.check(&event(1059, vec![])).is_ok());
    for value in [
        json!({"allowed_kinds": [1]}),
        json!({"enabled": false, "allowed_kinds": [1]}),
    ] {
        assert!(policy(value).check(&event(1059, vec![])).is_ok());
    }
}

#[test]
fn nip11_reports_restricted_writes_only_while_enabled() {
    let mut setting = Setting::default();
    let mut policy = EventPolicy::new();
    for (value, restricted) in [
        (configuration(), true),
        (json!({"enabled": false}), false),
        (json!({"enabled": "TRUE"}), true),
        (json!({}), false),
    ] {
        setting.extra.insert("event_policy".into(), value);
        policy.setting(&mut setting).unwrap()();
        let info: Value = serde_json::from_str(&setting.render_information().unwrap()).unwrap();
        assert_eq!(
            info["limitation"]["restricted_writes"]
                .as_bool()
                .unwrap_or(false),
            restricted
        );
    }
}

#[test]
fn only_event_commands_are_filtered_even_with_invalid_policy() {
    let mut policy = EventPolicy::new();
    assert!(policy
        .setting(&mut setting(json!({"enabled": "invalid"})))
        .is_err());
    assert!(policy.check(&event(1, vec![])).is_err());
    let raw = r#"["REQ","read",{"kinds":[1059]}]"#;
    for incoming in [
        serde_json::from_str(raw).unwrap(),
        IncomingMessage::Auth(event(22242, vec![])),
    ] {
        assert!(matches!(
            policy.filter(ClientMessage::new(1, raw.into(), incoming)),
            ExtensionMessageResult::Continue(_)
        ));
    }
}

#[test]
fn rejections_are_ok_responses_for_the_event() {
    let policy = policy(configuration());
    for event in [event(1059, vec![]), event(30078, identifier("unlisted"))] {
        let id = event.id_str();
        let msg = ClientMessage::new(1, String::new(), IncomingMessage::Event(event));
        let ExtensionMessageResult::Stop(response) = policy.filter(msg) else {
            panic!("expected an explicit rejection");
        };
        let (command, response_id, accepted, reason): (String, String, bool, String) =
            serde_json::from_str(&response.to_string()).unwrap();
        assert_eq!((command.as_str(), response_id, accepted), ("OK", id, false));
        assert!(reason.starts_with("blocked:"));
    }
}
