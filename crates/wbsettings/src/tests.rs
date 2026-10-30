use super::*;
use std::sync::Barrier;

struct Store {
    dir: PathBuf,
    settings: Settings,
}
impl Store {
    fn new(data: Value) -> Self {
        let dir = std::env::temp_dir().join(format!("wbsettings-test-{}", ids::token_hex(12)));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
        Self {
            settings: Settings::load(&path).unwrap(),
            dir,
        }
    }
    fn disk(&self) -> Value {
        serde_json::from_slice(&fs::read(self.settings.path()).unwrap()).unwrap()
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn fresh_defaults_and_read_tolerance() {
    let store = Store::new(
        json!({"account_max_concurrent":true,"daily_chat_web_concurrency":99,
        "usage":{"hour_keep_hours":"many","day_keep_days":99999},"log_db_max_days":null}),
    );
    let data = store.settings.snapshot();
    assert_eq!(data["daily_chat_web_concurrency"], 1);
    assert_eq!(data["account_max_concurrent"], 0);
    assert_eq!(data["usage"]["hour_keep_hours"], 48);
    assert_eq!(data["usage"]["day_keep_days"], 32);
    assert_eq!(data["log_db_max_days"], Value::Null);
    assert_eq!(data["limits"]["expiring_window_days"]["global"], 7);
    assert_eq!(data["schedule"]["daily_chat"], json!(["09:00", "21:00"]));
    fs::write(store.settings.path(), b"{broken").unwrap();
    assert!(store.settings.verify_password("admin"));
    assert_eq!(store.settings.snapshot()["log_db_max_days"], 7);
    fs::remove_file(store.settings.path()).unwrap();
    assert_eq!(
        Settings::load(&store.dir).unwrap().view()["daily_chat_web_concurrency"],
        1
    );
}

#[test]
fn partial_update_preserves_unknown_top_and_nested_values() {
    let store = Store::new(json!({"user_custom_top":{"keep":1},
        "upstream":{"header_timeout_seconds":120,"unknown_upstream_key":42,"nested":{"deep":7}},
        "usage":{"unknown_usage_key":9,"nested":{"deep":1}}}));
    store.settings.patch(json!({"upstream":{"idle_timeout_seconds":600},"usage":{"hour_keep_hours":120},"daily_chat_web_concurrency":4})).unwrap();
    let disk = store.disk();
    assert_eq!(disk["user_custom_top"], json!({"keep":1}));
    assert_eq!(disk["upstream"]["unknown_upstream_key"], 42);
    assert_eq!(disk["upstream"]["nested"], json!({"deep":7}));
    assert_eq!(disk["upstream"]["header_timeout_seconds"], 120);
    assert_eq!(disk["upstream"]["idle_timeout_seconds"], 600);
    assert_eq!(disk["usage"]["nested"], json!({"deep":1}));
    assert_eq!(store.settings.view()["daily_chat_web_concurrency"], 4);
    assert_eq!(store.settings.view()["daily_chat_web_concurrency_max"], 10);
    let base = json!({"nested":{"x":1,"keep":true},"a":1});
    assert_eq!(
        deep_merge(&base, &json!({"nested":{"x":2},"b":3})),
        json!({"nested":{"x":2,"keep":true},"a":1,"b":3})
    );
    assert_eq!(base["nested"]["x"], 1);
}

#[test]
fn invalid_patch_is_transactional_and_does_not_touch_disk() {
    let store = Store::new(json!({"ui_language":"zh"}));
    let before = fs::read(store.settings.path()).unwrap();
    for patch in [
        json!(null),
        json!([]),
        json!({"ui_language":"en","daily_chat_web_concurrency":0}),
        json!({"daily_chat_web_concurrency":true}),
        json!({"account_max_concurrent":1.5}),
        json!({"usage":{"day_keep_days":"32"}}),
        json!({"usage":{"day_keep_days":367}}),
        json!({"upstream":{"unknown":1}}),
        json!({"daily_chat_web":"false"}),
        json!({"limits":{"daily_token_limit":{"global":true}}}),
        json!({"limits":{"daily_token_limit":{"cn":-1}}}),
        json!({"daily_chat_model":"a/b"}),
        json!({"ui_language":"xx"}),
    ] {
        assert!(
            store.settings.patch(patch.clone()).is_err(),
            "accepted {patch}"
        );
        assert_eq!(fs::read(store.settings.path()).unwrap(), before);
    }
    store
        .settings
        .patch(json!({"unrecognized_secret":"not-stored","panel_password_hash":"bad"}))
        .unwrap();
    assert_eq!(store.disk(), json!({"ui_language":"zh"}));
}

#[test]
fn keys_preserve_blank_secrets_omitted_fields_and_upsert_siblings() {
    let store = Store::new(json!({}));
    store.settings.patch(json!({"api_keys":[
        {"id":"one","key":"secret-one","name":"One","realm":"intl","models":"GPT-*,gpt-*;claude-?","accounts":["u1","u1"],"concurrency_limit":3},
        {"id":"two","key":"secret-two","name":"Two"}]})).unwrap();
    let view = store
        .settings
        .patch(
            json!({"api_keys":[{"id":"one","key":"","name":"Renamed"}],"deleted_api_key_ids":[]}),
        )
        .unwrap();
    assert_eq!(store.settings.keys().len(), 2);
    let first = store
        .settings
        .keys()
        .into_iter()
        .find(|e| e["id"] == "one")
        .unwrap();
    assert_eq!(first["key"], "secret-one");
    assert_eq!(first["realm"], "intl");
    assert_eq!(first["models"], json!(["gpt-*", "claude-?"]));
    assert_eq!(first["accounts"], json!(["u1"]));
    assert_eq!(first["concurrency_limit"], 3);
    assert_eq!(view["api_keys"][0]["concurrency_limit"], 3);
    assert!(view["api_keys"][0].get("key").is_none());
    assert_eq!(
        store.settings.match_key(" secret-one ", &[]).unwrap()["source"],
        "panel"
    );
    assert!(store
        .settings
        .patch(json!({"api_keys":[{"id":"new","key":""}]}))
        .is_err());
}

#[test]
fn replacement_and_explicit_deletion_are_one_way_and_wipe_secrets() {
    let store = Store::new(json!({}));
    store
        .settings
        .patch(
            json!({"api_keys":[{"id":"one","key":"secret-one"},{"id":"two","key":"secret-two"}]}),
        )
        .unwrap();
    store
        .settings
        .patch(json!({"api_keys":[{"id":"one"}]}))
        .unwrap();
    assert_eq!(store.settings.keys().len(), 1);
    assert_eq!(store.settings.view()["deleted_api_keys"][0]["id"], "two");
    assert!(!store.disk().to_string().contains("secret-two"));
    assert!(store.settings.match_key("secret-two", &[]).is_none());
    store.settings.patch(json!({"api_keys":[{"id":"two","key":"revive-secret","enabled":true}],"deleted_api_key_ids":[]})).unwrap();
    assert!(store.settings.match_key("revive-secret", &[]).is_none());
    store
        .settings
        .patch(json!({"api_keys":[{"id":"one"}],"deleted_api_key_ids":["one"]}))
        .unwrap();
    assert!(store.settings.keys().is_empty());
    assert_eq!(
        store.settings.view()["deleted_api_keys"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(!store.disk().to_string().contains("secret-one"));
}

#[test]
fn stale_key_ids_adopt_existing_identity_and_duplicate_ids_are_stable() {
    let store = Store::new(
        json!({"api_keys":[{"id":"dup","key":"secret-a"},{"id":"dup","key":"secret-b"}]}),
    );
    assert_eq!(store.settings.keys()[1]["id"], "dup-2");
    assert_eq!(store.settings.reveal_key("dup-2"), Some("secret-b".into()));
    store.settings.patch(json!({"api_keys":[{"id":"stale","key":"secret-a","name":"rename"}],"deleted_api_key_ids":[]})).unwrap();
    assert_eq!(store.settings.keys().len(), 2);
    assert_eq!(
        store.settings.match_key("secret-a", &[]).unwrap()["id"],
        "dup"
    );
    let legacy = Store::new(json!({"api_key_set":true,"api_key":"legacy-secret"}));
    assert_eq!(legacy.settings.keys()[0]["id"], "legacy");
    legacy
        .settings
        .patch(json!({"api_keys":[{"id":"legacy","name":"saved"}]}))
        .unwrap();
    assert!(legacy.disk().get("api_key").is_none());
    assert_eq!(legacy.settings.keys()[0]["key"], "legacy-secret");
}

#[test]
fn launcher_auth_precedence_and_disabled_keys() {
    let store = Store::new(json!({}));
    let (key, created) = store.settings.ensure_launcher_key().unwrap();
    assert!(created);
    assert_eq!(
        store.settings.ensure_launcher_key().unwrap(),
        (key.clone(), false)
    );
    assert!(key.starts_with("wb-"));
    let extras = vec![key.clone()];
    assert!(store.settings.auth_required(&extras));
    assert_eq!(
        store.settings.match_key(&key, &extras).unwrap()["source"],
        "launcher"
    );
    store
        .settings
        .patch(json!({"api_keys":[{"id":"disabled","key":"disabled-secret","enabled":false}]}))
        .unwrap();
    assert!(store.settings.match_key(&key, &extras).is_none());
    assert!(store
        .settings
        .match_key("disabled-secret", &extras)
        .is_none());
    store.settings.patch(json!({"auth_disabled":true})).unwrap();
    assert!(!store.settings.auth_required(&extras));
}

#[test]
fn model_and_account_allowlists() {
    let key = json!({"models":["GPT-*","claude-?","model-[a-c]","no-[!x]"],"accounts":["u1","u2"]});
    assert!(key_allows_model(&key, "gpt-4"));
    assert!(key_allows_model(&key, "Claude-X"));
    assert!(key_allows_model(&key, "model-b"));
    assert!(!key_allows_model(&key, "model-x"));
    assert!(key_allows_model(&key, "no-y"));
    assert!(!key_allows_model(&key, "no-x"));
    assert!(!key_allows_model(&key, ""));
    assert!(!key_allows_model(&key, "claude-long"));
    assert!(key_allows_account(&key, "u1"));
    assert!(!key_allows_account(&key, "u*"));
    assert!(key_allows_model(&json!({}), "anything"));
    assert!(key_allows_account(&json!({}), "anything"));
    assert_eq!(mask_key("abcdefgh"), "********");
    assert_eq!(mask_key("abcdefghijkl"), "abcd******ijkl");
}

#[test]
fn key_slots_release_even_after_limit_changes_and_unwind() {
    let store = Store::new(json!({}));
    store
        .settings
        .patch(json!({"api_keys":[{"id":"a","key":"secret-a","concurrency_limit":1}]}))
        .unwrap();
    let entry = store.settings.keys()[0].clone();
    let guard = store.settings.try_acquire_key(&entry).unwrap();
    assert!(store
        .settings
        .try_acquire_key(&entry)
        .unwrap_err_is_rate_limited());
    store
        .settings
        .patch(json!({"api_keys":[{"id":"a","concurrency_limit":0}]}))
        .unwrap();
    drop(guard);
    assert_eq!(store.settings.key_inflight("a"), 0);
    let new_entry = store.settings.keys()[0].clone();
    let guard = store.settings.try_acquire_key(&new_entry).unwrap();
    assert_eq!(store.settings.key_inflight("a"), 1);
    drop(guard);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = store.settings.try_acquire_key(&entry).unwrap();
        panic!("request aborted");
    }));
    assert_eq!(store.settings.key_inflight("a"), 0);
}
trait RateResult {
    fn unwrap_err_is_rate_limited(self) -> bool;
}
impl RateResult for Result<KeySlot> {
    fn unwrap_err_is_rate_limited(self) -> bool {
        matches!(self, Err(Error::RateLimited { .. }))
    }
}

#[test]
fn concurrent_permits_never_exceed_cap() {
    let store = Store::new(json!({}));
    let settings = store.settings.clone();
    let barrier = Arc::new(Barrier::new(9));
    let release = Arc::new(Barrier::new(9));
    let mut threads = Vec::new();
    for _ in 0..8 {
        let settings = settings.clone();
        let barrier = barrier.clone();
        let release = release.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            let guard = settings.try_acquire_key(&json!({"id":"a","concurrency_limit":2}));
            release.wait();
            guard.is_ok()
        }));
    }
    barrier.wait();
    release.wait();
    let successful = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .filter(|ok| *ok)
        .count();
    assert_eq!(successful, 2);
    assert_eq!(settings.key_inflight("a"), 0);
}

#[test]
fn concurrent_independent_stores_preserve_all_updates() {
    let store = Store::new(json!({"unknown":"preserved"}));
    let barrier = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8).map(|i| {
        let path = store.settings.path().to_owned(); let barrier = barrier.clone();
        std::thread::spawn(move || {
            let settings = Settings::load(path).unwrap(); barrier.wait();
            settings.patch(json!({"api_keys":[{"id":format!("k{i}"),"key":format!("secret-{i}")}],"deleted_api_key_ids":[]})).unwrap();
        })
    }).collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(store.settings.keys().len(), 8);
    assert_eq!(store.disk()["unknown"], "preserved");
}

#[test]
fn password_hashing_rotation_and_no_secrets_in_view() {
    let store = Store::new(
        json!({"launcher_key":"launcher-unique-secret","unknown":{"token":"unknown-unique-secret"},
        "upstream":{"device_token":"device-unique-secret","device_token_file":"secret-file-path","extension":{"secret":"nested-unique-secret"}},
        "proxy_slots":[{"url":"http://username:proxy-unique-secret@localhost:8888"}]}),
    );
    assert!(store.settings.verify_password("admin"));
    assert!(!store.settings.verify_password(""));
    let token = store.settings.sessions().issue();
    assert!(store.settings.set_password("123").is_err());
    store.settings.set_password("panel-unique-secret").unwrap();
    assert!(!store.settings.sessions().valid(&token));
    assert!(store.settings.verify_password("panel-unique-secret"));
    assert!(!store.settings.verify_password("admin"));
    assert!(!store.settings.password_is_default());
    store
        .settings
        .patch(json!({"api_keys":[{"id":"a","key":"api-unique-secret","concurrency_limit":3}]}))
        .unwrap();
    let disk = store.disk();
    assert_eq!(disk["panel_password_salt"].as_str().unwrap().len(), 32);
    assert_eq!(disk["panel_password_hash"].as_str().unwrap().len(), 64);
    assert_eq!(disk["panel_password_rounds"], 120000);
    let view = store.settings.view();
    let serialized = view.to_string();
    for secret in [
        "launcher-unique-secret",
        "unknown-unique-secret",
        "device-unique-secret",
        "secret-file-path",
        "nested-unique-secret",
        "proxy-unique-secret",
        "panel-unique-secret",
        "api-unique-secret",
        disk["panel_password_hash"].as_str().unwrap(),
        disk["panel_password_salt"].as_str().unwrap(),
    ] {
        assert!(!serialized.contains(secret), "leaked {secret}");
    }
    for field in [
        "panel_password_hash",
        "panel_password_salt",
        "launcher_key",
        "api_key",
        "panel_password",
        "unknown",
    ] {
        assert!(view.get(field).is_none());
    }
    assert_eq!(view["api_keys"][0]["concurrency_limit"], 3);
    store.settings.set_password("admin").unwrap();
    assert!(store.settings.password_is_default());
    assert!(store.settings.verify_password("admin"));
    assert_eq!(store.disk()["panel_password_hash"], "");
    assert!(store.disk().get("panel_password_salt").is_none());
}

#[test]
fn identity_versions_require_password_and_do_not_persist_it() {
    let store = Store::new(json!({}));
    assert!(store
        .settings
        .patch(json!({"identity_versions":{"sdk":"1.2.3"}}))
        .is_err());
    store
        .settings
        .patch(json!({"identity_versions":{"sdk":"1.2.3"},"panel_password":"admin"}))
        .unwrap();
    assert_eq!(store.settings.view()["identity_versions"]["sdk"], "1.2.3");
    assert!(store.disk().get("panel_password").is_none());
    store
        .settings
        .patch(json!({"identity_versions":{},"panel_password":"admin"}))
        .unwrap();
    assert!(store.disk().get("identity_versions").is_none());
}

#[test]
fn realm_limit_migration_override_and_pricing_migration() {
    let store =
        Store::new(json!({"daily_token_limit":42,"pricing_refresh_hours":6,"unknown":"keep"}));
    assert!(store.disk().get("daily_token_limit").is_none());
    assert_eq!(store.disk()["pricing_refresh_minutes"], 360.0);
    assert_eq!(store.settings.limit("daily_token_limit", Realm::Intl), 42);
    store
        .settings
        .patch(json!({"limits":{"daily_token_limit":{"cn":0}}}))
        .unwrap();
    assert_eq!(store.settings.limit("daily_token_limit", Realm::Cn), 0);
    assert_eq!(store.settings.limit("daily_token_limit", Realm::Intl), 42);
    store
        .settings
        .patch(json!({"limits":{"daily_token_limit":{"cn":""}},"reserve_credits":25}))
        .unwrap();
    assert_eq!(store.settings.limit("daily_token_limit", Realm::Cn), 42);
    assert_eq!(store.settings.limit("reserve_credits", Realm::Intl), 25);
    assert_eq!(store.settings.limit("unknown", Realm::Intl), 0);
    assert_eq!(store.settings.view()["daily_token_limit"], 42);
}

#[test]
fn retired_dashboard_layout_keys_are_dropped_and_ignored() {
    const KEYS: [&str; 4] = [
        "accounts_collapsed",
        "accounts_separate_tab",
        "key_before_hidden",
        "section_max_height",
    ];
    let store = Store::new(json!({
        "accounts_collapsed": true, "accounts_separate_tab": true,
        "key_before_hidden": true, "section_max_height": 900, "ui_language": "en"
    }));
    // Loading rewrites the file without them and keeps everything else.
    let disk = store.disk();
    for key in KEYS {
        assert!(disk.get(key).is_none(), "{key} still on disk");
        assert!(
            store.settings.view().get(key).is_none(),
            "{key} still in the view"
        );
    }
    assert_eq!(disk["ui_language"], "en");
    // A client that still sends them is not rejected, but nothing is stored.
    store
        .settings
        .patch(
            json!({"accounts_collapsed": true, "section_max_height": 10, "hidden_pages": ["logs"]}),
        )
        .unwrap();
    let disk = store.disk();
    for key in KEYS {
        assert!(disk.get(key).is_none(), "{key} was stored by a patch");
    }
    assert_eq!(store.settings.view()["hidden_pages"], json!(["logs"]));
}

#[test]
fn schedules_ui_and_clamps() {
    let store = Store::new(json!({}));
    store
        .settings
        .patch(
            json!({"schedule":{"checkin":"21,9,9,99","daily_chat":"9：05, 21:30"},
        "hidden_pages":["logs","settings","logs","bad page","tasks"],
        "pricing_refresh_minutes":999999,"credits_refresh_hours":9999}),
        )
        .unwrap();
    let view = store.settings.view();
    assert_eq!(view["schedule"]["checkin"], json!([9, 21]));
    assert_eq!(view["schedule"]["daily_chat"], json!(["09:05", "21:30"]));
    assert_eq!(view["hidden_pages"], json!(["logs", "tasks"]));
    assert_eq!(view["pricing_refresh_minutes"], 43200.0);
    assert_eq!(view["credits_refresh_hours"], 720.0);
    store
        .settings
        .patch(json!({"schedule":{"daily_chat":[]}}))
        .unwrap();
    assert_eq!(
        store.settings.view()["schedule"]["daily_chat"],
        json!(["09:00", "21:00"])
    );
}

#[test]
fn proxy_slots_dedupe_monotonic_ids_and_probe_updates() {
    let store = Store::new(json!({"proxy_slots":[{"id":"slot-40","url":"http://old:1"}]}));
    let slots = store.settings.save_proxy_slots(json!([{"url":"http://a:1","name":"first"},{"url":"socks5h://b:2"},{"url":"http://a:1"}])).unwrap();
    assert_eq!(slots.len(), 2);
    assert_eq!(slots[0]["id"], "slot-41");
    assert_eq!(slots[1]["id"], "slot-42");
    store
        .settings
        .update_proxy_slot(
            "slot-41",
            json!({"ip":"1.2.3.4","country_code":"us","probed_at":123}),
        )
        .unwrap();
    store
        .settings
        .save_proxy_slots(json!([{"id":"slot-41","url":"http://a:1","name":"renamed"}]))
        .unwrap();
    let first = store.settings.find_proxy_slot("slot-41").unwrap();
    assert_eq!(first["ip"], "1.2.3.4");
    assert_eq!(first["country_code"], "US");
    assert_eq!(first["name"], "renamed");
    store.settings.save_proxy_slots(json!([])).unwrap();
    assert_eq!(
        store
            .settings
            .save_proxy_slots(json!([{"url":"http://c:3"}]))
            .unwrap()[0]["id"],
        "slot-43"
    );
    assert!(matches!(
        store
            .settings
            .update_proxy_slot("slot-41", json!({"ip":"5.6.7.8"})),
        Err(Error::NotFound(_))
    ));
    assert!(store.settings.save_proxy_slots(json!({})).is_err());
    assert!(store
        .settings
        .save_proxy_slots(json!([{"url":"file:///secret"}]))
        .is_err());
}

#[test]
fn write_failures_leave_no_temp_files_and_are_reported() {
    let store = Store::new(json!({}));
    let bad = store.dir.join("not-a-directory");
    fs::write(&bad, b"file").unwrap();
    // Windows reports a child of a regular file as NotFound on read, while
    // Unix reports NotADirectory. Either way persistence must report failure.
    if let Ok(settings) = Settings::load(bad.join("settings.json")) {
        assert!(settings.patch(json!({"ui_language":"en"})).is_err());
    }
    store.settings.patch(json!({"ui_language":"en"})).unwrap();
    assert!(fs::read_dir(&store.dir).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
}

#[test]
fn malformed_password_and_public_fields_fail_closed() {
    let store = Store::new(json!({
        "panel_password_hash":12345,
        "auto_switch_products":{"secret":"malformed-object-secret"},
        "daily_chat_web_prompts":[{"secret":"malformed-list-secret"}],
        "identity_versions":{"unknown_secret":"version-secret"},
        "api_keys":[{"id":"a","key":"api-secret","name":{"secret":"name-secret"},"created_at":{"secret":"date-secret"}}]
    }));
    assert!(!store.settings.verify_password("admin"));
    assert!(!store.settings.password_is_default());
    let view = store.settings.view().to_string();
    for value in [
        "malformed-object-secret",
        "malformed-list-secret",
        "version-secret",
        "name-secret",
        "date-secret",
    ] {
        assert!(!view.contains(value));
    }
}

#[test]
fn failed_atomic_replace_removes_temporary_file() {
    let store = Store::new(json!({}));
    fs::remove_file(store.settings.path()).unwrap();
    fs::create_dir(store.settings.path()).unwrap();
    assert!(atomic_write(store.settings.path(), &json!({"secret":"not-left-behind"})).is_err());
    assert!(fs::read_dir(&store.dir).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
}

#[cfg(unix)]
#[test]
fn atomic_write_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let store = Store::new(json!({}));
    fs::set_permissions(store.settings.path(), fs::Permissions::from_mode(0o644)).unwrap();
    store.settings.patch(json!({"ui_language":"en"})).unwrap();
    assert_eq!(
        fs::metadata(store.settings.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
