use crate::Auth;
use anyhow::Result;
use nostr_relay::{Extension, Setting};
use serde_json::{json, Value};

fn rejects(extension: &mut dyn Extension, values: impl IntoIterator<Item = Value>) {
    for value in values {
        let mut setting = Setting::default();
        let name = extension.name();
        setting.extra.insert(name.to_owned(), value.clone());
        let error = extension
            .setting(&mut setting)
            .err()
            .unwrap_or_else(|| panic!("accepted invalid [{name}]: {value}"));
        assert!(error.to_string().contains(name), "{error}");
    }
}

fn common(extension: &mut dyn Extension) -> Result<()> {
    extension.setting(&mut Setting::default())?();
    rejects(
        extension,
        [
            json!({"enabled": "yes"}),
            json!({"enabled": 1}),
            json!({"enabled": false, "enabld": true}),
        ],
    );
    Ok(())
}

#[test]
fn auth_configuration_is_strict_including_nested_permissions() -> Result<()> {
    let mut auth = Auth::new();
    common(&mut auth)?;
    let mut setting = Setting::default();
    setting.extra.insert(
        "auth".to_owned(),
        json!({
            "enabled": true, "req": null, "event": null
        }),
    );
    let parsed: crate::auth::AuthSetting = setting.parse_extension("auth")?;
    assert!(parsed.enabled && parsed.req.is_none() && parsed.event.is_none());
    auth.setting(&mut setting)?();
    rejects(
        &mut auth,
        [
            json!({"req": {"pubkey_whitelst": []}}),
            json!({"event": {"ip_whitelist": true}}),
            json!({"req": []}),
            json!({"event": []}),
            json!({"event": false}),
        ],
    );
    Ok(())
}

#[cfg(feature = "count")]
#[actix_rt::test]
async fn count_configuration_is_strict() -> Result<()> {
    let app = crate::create_test_app("config-count")?;
    common(&mut crate::Count::new(app.db))
}

#[cfg(feature = "search")]
#[test]
fn search_configuration_is_strict() -> Result<()> {
    common(&mut crate::Search::new())
}

#[cfg(feature = "metrics")]
#[test]
fn metrics_configuration_is_strict() -> Result<()> {
    let handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .build_recorder()
        .handle();
    common(&mut crate::Metrics {
        handle: actix_web::web::Data::new(handle),
    })
}

#[cfg(feature = "rate_limiter")]
#[test]
fn rate_limiter_configuration_rejects_invalid_rules() -> Result<()> {
    let mut limiter = crate::Ratelimiter::new();
    common(&mut limiter)?;
    rejects(
        &mut limiter,
        [
            json!({"event": [{"period": "1s", "limit": 1, "typo": true}]}),
            json!({"clear_interval": 0}),
            json!({"event": [{"period": 0, "limit": 1}]}),
            json!({"event": [{"period": "1s", "limit": 0}]}),
            json!({"event": [{"period": "1s", "limit": true}]}),
            json!({"event": [{"period": "1s", "limit": 1.5}]}),
            json!({"event": [["name", "description", "1s", 1, null, null]]}),
            json!({"event": [{"period": "1ns", "limit": 2}]}),
            json!({"event": [{"period": u64::MAX, "limit": 1}]}),
        ],
    );
    Ok(())
}
