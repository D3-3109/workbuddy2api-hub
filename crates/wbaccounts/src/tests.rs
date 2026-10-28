use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::PathBuf,
    sync::{Arc, Barrier},
};
use wbcore::{
    ids, timeutil,
    types::{Egress, HeaderCase, InResponse, Product, Realm},
};
fn jwt(uid: &str, exp: i64) -> String {
    format!(
        "e30.{}.sig",
        URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({"sub":uid,"exp":exp,"iss":"https://www.workbuddy.ai"}))
                .unwrap()
        )
    )
}
fn account(uid: &str) -> Arc<Account> {
    Arc::new(Account::from_json(&json!({"uid":uid,"accessToken":jwt(uid,4_000_000_000i64),"refreshToken":"refresh-secret"})).unwrap())
}
fn response(status: u16, body: Value) -> InResponse {
    InResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        ..Default::default()
    }
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("wbaccounts-test-{}", ids::uuid4_hex()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn account_normalizes_and_redacts() {
    let a=Account::from_json(&json!({"accessToken":"ck_key-1.the-secret","realm":"intl","weight":1000,"disabledModels":["GPT","gpt"," hy3 "],"proxy":"http://user:pass@proxy","nickname":"$wbEncrypted:xxx","note":"hello","tier":{"password":"hidden","message":"ck_key-1.the-secret"}})).unwrap();
    assert_eq!(a.uid(), "apikey-key-1");
    assert_eq!(a.realm(), Realm::Cn);
    assert_eq!(a.weight(), 100);
    assert_eq!(a.to_json()["expiresAt"], 0);
    assert_eq!(a.to_json()["disabledModels"], json!(["gpt", "hy3"]));
    let public = a.public_metadata().to_string();
    for secret in [
        "the-secret",
        "refreshToken",
        "accessToken",
        "user:pass",
        "hidden",
    ] {
        assert!(!public.contains(secret), "{public}");
    }
    assert!(!format!("{a:?}").contains("the-secret"));
}
#[test]
fn public_metadata_exposes_restriction_cooldowns_and_safe_guard_diagnostics() {
    let dir = Temp::new();
    let a = account("u");
    a.save_to(&dir.0).unwrap();
    let now = timeutil::now();
    a.note_restricted(
        &format!("restricted {} refresh-secret", a.cred().access_token),
        now,
    )
    .unwrap();
    a.note_model_cap("hy3", now + 3600., now).unwrap();
    a.set_guards(Guards {
        daily_token_limit: 10,
        daily_tokens_today: Some(10),
        ..Default::default()
    });
    let public = a.public_metadata();
    assert!(public["restrictedFor"].as_f64().unwrap() > 7190.);
    assert!(public["cooldownFor"].as_f64().unwrap() > 7190.);
    assert_eq!(public["breakerFor"], 0.0);
    assert_eq!(public["degradeFor"], 0.0);
    assert_eq!(public["dailyLimitBlocked"], true);
    assert_eq!(public["file"], "u.json");
    assert_eq!(public["modelCooldowns"][0]["model"], "hy3");
    assert!(public["modelCooldowns"][0]["cappedAt"].is_i64());
    assert!(public["expiresIn"].is_string());
    let serialized = public.to_string();
    assert!(!serialized.contains("refresh-secret"));
    assert!(!serialized.contains(&a.cred().access_token));
}
#[test]
fn load_save_preserves_caps_and_restrictions_not_runtime() {
    let dir = Temp::new();
    let a = account("a/b");
    let now = timeutil::now();
    a.save_to(&dir.0).unwrap();
    a.note_restricted("restricted", now).unwrap();
    a.note_model_cap("hy3", now + 3600.0, now).unwrap();
    a.note_error("temporary", Some("paid"), 200.0, None, now);
    a.note_failure("failure", now);
    a.save().unwrap();
    let path = dir.0.join("a_b.json");
    let bytes = fs::read_to_string(&path).unwrap();
    assert!(!bytes.ends_with('\n'));
    assert!(!bytes.contains("lastError"));
    assert!(!bytes.contains("cooldownUntil"));
    let b = Account::load(path).unwrap();
    assert_eq!(b.throttle_wait("hy3", now), 7200.0);
    assert_eq!(b.cap_until("hy3", now), now + 3600.0);
    assert_eq!(b.public_metadata()["fails"], 0);
    assert_eq!(b.throttle_wait("paid", now + 7201.0), 0.0);
    a.save().unwrap(); // replace existing file on Windows too
}
#[test]
fn filename_collision_is_rejected() {
    let dir = Temp::new();
    let a = account("a/b");
    let b = account("a?b");
    a.save_to(&dir.0).unwrap();
    assert!(b.save_to(&dir.0).is_err());
    assert_eq!(Account::load(dir.0.join("a_b.json")).unwrap().uid(), "a/b");
}
#[test]
fn patch_and_upsert_failed_save_do_not_mutate_memory() {
    let dir = Temp::new();
    let a = account("u");
    let path = a.save_to(&dir.0).unwrap();
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(a.patch(&json!({"weight":9,"nickname":"changed"})).is_err());
    assert_eq!(a.weight(), 1);
    assert_eq!(a.to_json()["nickname"], "");
    let pool = AccountPool::new(vec![a.clone()]);
    let incoming = account("u");
    incoming.set_weight(8).unwrap();
    assert!(pool.upsert(incoming, Some(&dir.0)).is_err());
    assert_eq!(a.weight(), 1);
}
#[test]
fn import_shapes_and_validation() {
    let token = jwt("u", 4_000_000_000);
    let rows = import_document(
        &json!({"auth":{"accessToken":token,"refreshToken":"r"},"account":{"nickname":"nested"}}),
        None,
    )
    .unwrap();
    assert_eq!(rows[0].uid(), "u");
    assert_eq!(rows[0].to_json()["nickname"], "nested");
    let rows=import_document(&json!([{"uid":"snake","access_token":jwt("snake",4_000_000_000),"refresh_token":"r","expires_at":1_790_000_000_000i64,"email":"name"}]),None).unwrap();
    assert_eq!(rows[0].to_json()["expiresAt"], 1_790_000_000i64);
    assert_eq!(rows[0].to_json()["source"], "cockpit");
    let doc = export_document(&rows, None, true, None);
    assert_eq!(import_document(&doc, None).unwrap()[0].uid(), "snake");
    assert!(import_document(&json!({"accessToken":"garbage"}), None).is_err());
    assert!(import_document(&json!([1]), None).is_err());
    assert!(import_document(&json!([]), None).is_err());
    assert!(import_document(
        &json!([{"uid":"u","accessToken":token},{"uid":"u","accessToken":token}]),
        None
    )
    .is_err());
    let api = import_document(&json!({"accessToken":"pt_abc.secret"}), Some(Realm::Intl)).unwrap();
    assert_eq!(api[0].realm(), Realm::Cn);
}
#[test]
fn restricted_export_import_refresh_success_does_not_bypass() {
    let now = timeutil::now();
    let a = account("u");
    a.note_restricted("11140", now).unwrap();
    a.note_model_cap("hy3", now + 300.0, now).unwrap();
    a.note_success("hy3", now + 1.0);
    a.clear_error(None, now + 2.0);
    a.apply_refresh_response(&response(
        200,
        json!({"data":{"accessToken":jwt("u",4_000_000_001i64)}}),
    ))
    .unwrap();
    let b = import_document(
        &export_document(std::slice::from_ref(&a), None, true, None),
        None,
    )
    .unwrap()
    .remove(0);
    assert_eq!(b.throttle_wait("hy3", now), 7200.0);
    assert_eq!(b.cap_until("hy3", now), now + 300.0);
    a.note_restricted("again", now + 10.0).unwrap();
    assert_eq!(a.throttle_wait("", now), 7200.0);
    assert!(matches!(
        a.readiness("hy3", now),
        Readiness::Blocked {
            reason: BlockReason::Restricted,
            ..
        }
    ));
}
#[test]
fn tier_update_is_atomic_and_preserves_tokens_and_restrictions() {
    let dir = Temp::new();
    let a = account("u");
    a.save_to(&dir.0).unwrap();
    let now = timeutil::now();
    let token = a.cred().access_token;
    let mut guards = Guards::default();
    std::sync::Arc::make_mut(&mut guards.pro_models).insert("pro".into());
    a.set_guards(guards);
    assert!(matches!(
        a.readiness("pro", now),
        Readiness::Blocked {
            reason: BlockReason::ProRequired,
            ..
        }
    ));
    let tier = json!({"paid":true,"package":"Pro","package_code":"pro","protected_price":false,"trial":0,"checked_at":now,"source":"billing"});
    a.set_tier(tier.clone()).unwrap();
    assert_eq!(a.readiness("pro", now), Readiness::Ready);
    a.note_restricted("11140", now).unwrap();
    a.set_tier(tier.clone()).unwrap();
    assert_eq!(a.cred().access_token, token);
    assert_eq!(a.throttle_wait("pro", now), 7200.);
    let loaded = Account::load(a.path().unwrap()).unwrap();
    assert_eq!(loaded.to_json()["tier"], tier);
    assert_eq!(loaded.throttle_wait("", now), 7200.);
    assert!(a.set_tier(json!({"paid":"yes"})).is_err());
    assert_eq!(a.to_json()["tier"], tier);
    let path = a.path().unwrap();
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(a.set_tier(json!({"paid":false})).is_err());
    assert_eq!(a.to_json()["tier"], tier);
}
#[test]
fn guard_unknown_free_paid_model_and_near_expiry() {
    let a = account("u");
    let now = timeutil::now();
    let mut g = Guards {
        reserve_credits: 5.0,
        daily_token_limit: 100,
        daily_credit_limit: 5.0,
        model_daily_token_limit: 40,
        ..Default::default()
    };
    a.set_guards(g.clone());
    assert_eq!(a.readiness("paid", now), Readiness::Ready);
    a.set_credits(json!({"remain":5.9,"used":0,"size":10}))
        .unwrap();
    assert!(matches!(
        a.readiness("paid", now),
        Readiness::Blocked {
            reason: BlockReason::Reserve,
            ..
        }
    ));
    g.reserve_credits = 0.0;
    g.daily_tokens_today = Some(100);
    a.set_guards(g.clone());
    assert!(matches!(
        a.readiness("paid", now),
        Readiness::Blocked {
            reason: BlockReason::DailyTokens,
            ..
        }
    ));
    g.daily_tokens_today = Some(99);
    g.daily_credits_today = Some(5.0);
    std::sync::Arc::make_mut(&mut g.free_models).insert("hy3".into());
    a.set_guards(g.clone());
    assert_eq!(a.readiness("hy3", now), Readiness::Ready);
    assert!(matches!(
        a.readiness("paid", now),
        Readiness::Blocked {
            reason: BlockReason::DailyCredits,
            ..
        }
    ));
    g.model_daily_tokens.insert("hy3".into(), 40);
    a.set_guards(g.clone());
    assert!(matches!(
        a.readiness("hy3", now),
        Readiness::Blocked {
            reason: BlockReason::ModelDailyTokens,
            ..
        }
    ));
    std::sync::Arc::make_mut(&mut g.pro_models).insert("pro".into());
    a.set_guards(g);
    assert!(matches!(
        a.readiness("pro", now),
        Readiness::Blocked {
            reason: BlockReason::ProRequired,
            ..
        }
    ));
    a.set_guards(Guards::default());
    a.consume_credit(0.26);
    assert_eq!(a.to_json()["credits"]["remain"], 5.64);
    let exp = Account::from_json(
        &json!({"uid":"u","accessToken":jwt("u",now as i64+120),"refreshToken":"r"}),
    )
    .unwrap();
    assert_eq!(exp.readiness("", now), Readiness::NeedsRefresh);
}
#[test]
fn penalties_ladders_decay_and_model_isolation() {
    let a = account("u");
    for (i, wait) in [600., 1200., 2400., 4800., 7200., 7200.].iter().enumerate() {
        assert_eq!(a.note_soft_rate(1000. + i as f64), *wait);
    }
    assert_eq!(a.note_soft_rate(2000.), 600.);
    a.note_success("", 2100.);
    for (i, wait) in [60., 120., 240., 480., 600.].iter().enumerate() {
        assert_eq!(a.note_unscoped_rate("hy3", 3000. + i as f64), *wait);
    }
    assert_eq!(a.throttle_wait("other", 3005.), 0.0);
    a.note_success("hy3", 4000.);
    a.note_failure("err", 5000.);
    a.note_failure("err", 5001.);
    assert_eq!(a.throttle_wait("", 5001.), 0.);
    a.note_failure("err", 5002.);
    assert_eq!(a.throttle_wait("", 5002.), 1800.);
    a.note_success("", 7000.);
    for i in 0..5 {
        a.note_unknown_failure("err", 8000. + i as f64);
    }
    assert_eq!(a.throttle_wait("", 8004.), 7200.);
    a.note_success("", 16000.);
    a.note_failure("err", 17000.);
    a.note_failure("err", 18000.);
    a.note_failure("err", 18001.);
    assert_eq!(a.throttle_wait("", 18001.), 0.);
    assert!(!auth_failure_text("<html>401 Unauthorized</html>"));
    assert!(auth_failure_text("token expired"));
}
// Upstream provenance: 392410bdc5f4b464be34022162e4917a5681fb31,
// tests/_test_token_expired.py (verified against v1.8.0 / 4f7ae6f).
#[test]
fn served_success_clears_credential_park_and_badge_for_scoped_and_unscoped_calls() {
    let now = timeutil::now();
    for model in ["", "deepseek-v4.1-flash"] {
        let a = account("u");
        a.note_error("HTTP 401", None, 60.0, None, now);
        assert!(!a.ready(model));
        assert_eq!(a.public_metadata()["tokenExpired"], true);
        a.note_success(model, now);
        assert!(a.ready(model));
        let view = a.public_metadata();
        assert_eq!(view["tokenExpired"], false);
        assert_eq!(view["needRelogin"], false);
        assert_ne!(view["expiresIn"], "expired");
        assert_eq!(view["lastError"], "");
    }
}
#[test]
fn served_success_cannot_restore_missing_credentials_or_actual_expiry() {
    let now = timeutil::now();
    assert!(Account::from_json(&json!({"uid":"missing"})).is_err());
    let a = account("missing");
    // Constructor/import reject this; exercise readiness's defensive invariant.
    a.state.write().doc["accessToken"] = json!("");
    a.note_error("HTTP 401", None, 60.0, None, now);
    a.note_success("", now);
    assert!(matches!(
        a.readiness("", now),
        Readiness::Blocked {
            reason: BlockReason::MissingCredential,
            ..
        }
    ));
    assert_eq!(a.public_metadata()["tokenExpired"], true);
    let expired = Account::from_json(&json!({"uid":"expired",
        "accessToken":jwt("expired", now as i64 - 60)}))
    .unwrap();
    expired.note_success("", now);
    assert!(!expired.ready(""));
    assert_eq!(expired.public_metadata()["tokenExpired"], true);
}
#[test]
fn credential_recovery_keeps_restrictions_caps_and_unrelated_windows() {
    let dir = Temp::new();
    let now = timeutil::now();
    let a = account("u");
    let pool = AccountPool::new(vec![a.clone()]);
    a.note_restricted("11140", now).unwrap();
    a.note_model_cap("hy3", now + 9000.0, now).unwrap();
    a.note_error("HTTP 429", Some("other"), 10_000.0, None, now);
    a.note_error("HTTP 401", None, 60.0, None, now);
    a.note_success("hy3", now);
    assert_eq!(a.public_metadata()["tokenExpired"], false);
    a.clear_model_cooldown("hy3", now);
    pool.import(
        &json!({"uid":"u","accessToken":jwt("u",4_000_000_001),
        "restrictedUntil":0,"modelCaps":{}}),
        None,
        &dir.0,
    )
    .unwrap();
    a.apply_refresh_response(&response(
        200,
        json!({"data":{"accessToken":jwt("u",4_000_000_002)}}),
    ))
    .unwrap();
    assert_eq!(a.throttle_wait("hy3", now), 9000.0);
    assert_eq!(a.cap_until("hy3", now), now + 9000.0);
    assert_eq!(a.throttle_wait("other", now), 10_000.0);
    assert!(matches!(
        a.readiness("hy3", now),
        Readiness::Blocked {
            reason: BlockReason::Restricted,
            ..
        }
    ));
    assert!(pool
        .pick(&PickOptions {
            model: "hy3".into(),
            now,
            ..Default::default()
        })
        .is_none());
}

// Upstream provenance: a6bb76a3e557d8816955940d3a5f87222c40720a,
// tests/_test_window_cooldown_restore.py and wb_window_probe._load/_save.
fn write_windows(dir: &Temp, rows: Value) {
    fs::write(
        dir.0.join("model_windows.json"),
        serde_json::to_vec(&json!({
            "version":1,"saved_at":"2026-10-27 12:00:00","windows":rows
        }))
        .unwrap(),
    )
    .unwrap();
}
fn window(uid: &str, model: &str, reset: f64) -> Value {
    json!({"uid":uid,"model":model,"reset_at":reset,
        "attempts":0,"last_try":0.0,"last_result":""})
}
#[test]
fn restart_restores_model_window_for_readiness_pool_and_panel_until_exact_reset() {
    let dir = Temp::new();
    let now = timeutil::now();
    let reset = now + 21_600.0;
    account("u").save_to(&dir.0).unwrap();
    let mut row = window("u", "deepseek-v4.1-flash", reset);
    row["last_try"] = json!(now - 600.0);
    row["last_result"] = json!("old failure must not be replayed");
    write_windows(&dir, json!([row]));
    let (pool, issues) = AccountPool::load_dir(&dir.0).unwrap();
    assert!(issues.is_empty());
    let a = pool.get("u").unwrap();
    assert_eq!(a.throttle_wait("deepseek-v4.1-flash", now), 21_600.0);
    assert!(!a.ready("deepseek-v4.1-flash"));
    assert!(a.ready("hy3"));
    assert_eq!(a.throttle_wait("hy3", now), 0.0);
    let mut opts = PickOptions {
        model: "deepseek-v4.1-flash".into(),
        now,
        ..Default::default()
    };
    assert_eq!(pool.count_ready(&opts), 0);
    assert!(matches!(
        pool.acquire(&opts, 1),
        Err(PoolError::NoUsableAccount)
    ));
    let view = a.public_metadata();
    assert_eq!(
        view["modelCooldowns"],
        json!([{
            "model":"deepseek-v4.1-flash","expiresAt":reset as i64
        }])
    );
    assert_eq!(view["lastError"], "");
    assert!(view["lastErrorDetail"].is_null());
    opts.now = reset;
    assert_eq!(pool.count_ready(&opts), 1);
    assert_eq!(a.throttle_wait("deepseek-v4.1-flash", reset), 0.0);
}
#[test]
fn window_restore_skips_expired_missing_invalid_and_nonfinite_rows() {
    let dir = Temp::new();
    let now = timeutil::now();
    let a = account("u");
    let pool = AccountPool::new(vec![a.clone()]);
    write_windows(
        &dir,
        json!([
            window("u", "expired", now - 1.0), window("u", "equal", now),
            window("deleted", "m", now + 600.0), window("u", "", now + 600.0),
            {"uid":"u","model":"nan","reset_at":"NaN"},
            {"uid":"u","model":"inf","reset_at":"inf"},
            {"uid":"u","model":"bad","reset_at":{}},
            {"uid":"u","model":"missing"}, null, 42, "bad",
            {"uid":5,"model":"m","reset_at":now + 600.0}
        ]),
    );
    assert_eq!(pool.restore_window_cooldowns(&dir.0, now).unwrap(), 0);
    assert!(a.public_metadata()["modelCooldowns"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(a.ready("m"));
}
#[test]
fn window_restore_accepts_bom_legacy_map_bare_array_and_numeric_strings() {
    let now = timeutil::now();
    let reset = now + 600.0;
    let row = json!({"uid":"u","model":"m","reset_at":reset.to_string()});
    for document in [json!({"windows":{"u\0m":row}}), json!([row])] {
        let dir = Temp::new();
        account("u").save_to(&dir.0).unwrap();
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend(serde_json::to_vec(&document).unwrap());
        fs::write(dir.0.join("model_windows.json"), bytes).unwrap();
        let (pool, issues) = AccountPool::load_dir(&dir.0).unwrap();
        assert!(issues.is_empty());
        assert_eq!(pool.get("u").unwrap().throttle_wait("m", now), 600.0);
    }
}
#[test]
fn missing_corrupt_or_wrong_shape_sidecars_never_break_account_loading() {
    for content in [
        None,
        Some("{bad json"),
        Some("null"),
        Some("{\"windows\":3}"),
    ] {
        let dir = Temp::new();
        account("u").save_to(&dir.0).unwrap();
        if let Some(content) = content {
            fs::write(dir.0.join("model_windows.json"), content).unwrap();
        }
        let (pool, issues) = AccountPool::load_dir(&dir.0).unwrap();
        assert_eq!(pool.accounts().len(), 1);
        assert!(pool.get("u").unwrap().ready("m"));
        assert_eq!(issues.len(), usize::from(content.is_some()));
        if let Some(issue) = issues.first() {
            assert_eq!(issue.path, dir.0.join("model_windows.json"));
        }
    }
}
#[test]
fn window_reload_preserves_later_cooldowns_caps_restrictions_slots_and_error() {
    let dir = Temp::new();
    let now = timeutil::now();
    let a = account("u");
    a.save_to(&dir.0).unwrap();
    let slot = a.try_acquire_slot(1).unwrap();
    a.note_model_cap("hy3", now + 9000.0, now).unwrap();
    a.note_restricted("11140", now).unwrap();
    a.note_error("current error", Some("m"), 10_000.0, None, now);
    write_windows(
        &dir,
        json!([
            window("u", "m", now + 600.0),
            window("u", "hy3", now + 500.0),
            window("u", "new", now + 8000.0)
        ]),
    );
    let pool = AccountPool::new(vec![a.clone()]);
    assert!(pool.reload_dir(&dir.0).unwrap().is_empty());
    assert!(Arc::ptr_eq(&a, &pool.get("u").unwrap()));
    assert!(a.try_acquire_slot(1).is_none());
    assert_eq!(a.throttle_wait("m", now), 10_000.0);
    assert_eq!(a.throttle_wait("hy3", now), 9000.0);
    assert_eq!(a.throttle_wait("new", now), 8000.0);
    assert_eq!(a.to_json()["restrictedUntil"], now + 7200.0);
    assert_eq!(a.public_metadata()["lastError"], "current error");
    drop(slot);
}
#[test]
fn arm_and_clear_model_windows_clean_start_times_without_bypassing_caps() {
    let a = account("u");
    let now = timeutil::now();
    for until in [now, now - 1.0, f64::NAN, f64::INFINITY] {
        assert!(!a.arm_model_cooldown("m", until, None, now));
    }
    assert!(!a.arm_model_cooldown("", now + 60.0, None, now));
    assert!(a.arm_model_cooldown("m", now + 600.0, Some(now - 10.0), now));
    assert!(!a.arm_model_cooldown("m", now + 60.0, Some(now), now));
    assert_eq!(
        a.public_metadata()["modelCooldowns"][0]["startedAt"],
        (now - 10.0) as i64
    );
    a.clear_model_cooldown("m", now);
    assert!(!a.state.read().runtime.models_at.contains_key("m"));
    assert!(a.arm_model_cooldown("m", now + 600.0, None, now));
    assert!(a.public_metadata()["modelCooldowns"][0]
        .get("startedAt")
        .is_none());
    a.note_error("new error", Some("m"), 600.0, None, now);
    a.clear_error(Some("m"), now);
    assert!(!a.state.read().runtime.models_at.contains_key("m"));
    a.note_model_cap("hy3", now + 600.0, now).unwrap();
    a.note_error("cap", Some("hy3"), 600.0, None, now);
    a.clear_model_cooldown("hy3", now);
    assert_eq!(a.throttle_wait("hy3", now), 600.0);
    assert!(a.state.read().runtime.models_at.contains_key("hy3"));
    a.clear_model_cooldown("hy3", now + 600.0);
    assert!(!a.state.read().runtime.models_at.contains_key("hy3"));
}
#[test]
fn record_window_roundtrips_rust_restart_and_keeps_longest_deadline() {
    let dir = Temp::new();
    let now = timeutil::now();
    let a = account("u");
    a.save_to(&dir.0).unwrap();
    let pool = AccountPool::new(vec![a.clone()]);
    assert!(pool
        .record_model_window(&dir.0, "u", "m", now + 600.0, now)
        .unwrap());
    let before = fs::read(dir.0.join("model_windows.json")).unwrap();
    assert!(!pool
        .record_model_window(&dir.0, "u", "m", now + 300.0, now)
        .unwrap());
    assert_eq!(before, fs::read(dir.0.join("model_windows.json")).unwrap());
    assert!(pool
        .record_model_window(&dir.0, "u", "m", now + 900.0, now)
        .unwrap());
    let (restarted, issues) = AccountPool::load_dir(&dir.0).unwrap();
    assert!(issues.is_empty());
    assert_eq!(restarted.get("u").unwrap().throttle_wait("m", now), 900.0);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 2);
}
#[test]
fn record_window_preserves_unknown_fields_merges_duplicates_and_prunes_expired() {
    let dir = Temp::new();
    let now = timeutil::now();
    let pool = AccountPool::new(vec![account("u")]);
    let doc = json!({"version":1,"extension":{"keep":true},"windows":[
        {"uid":"u","model":"m","reset_at":now+600.0,"extra":"kept","attempts":2},
        {"uid":"u","model":"m","reset_at":now+900.0,"other_extra":7},
        window("u","old",now-1.0), window("another-account","m",now+900.0), null
    ]});
    fs::write(
        dir.0.join("model_windows.json"),
        serde_json::to_vec(&doc).unwrap(),
    )
    .unwrap();
    assert!(pool
        .record_model_window(&dir.0, "u", "m", now + 1200.0, now)
        .unwrap());
    let merged: Value =
        serde_json::from_slice(&fs::read(dir.0.join("model_windows.json")).unwrap()).unwrap();
    assert_eq!(merged["extension"], doc["extension"]);
    let rows = merged["windows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let row = rows.iter().find(|row| row["uid"] == "u").unwrap();
    assert_eq!(row["reset_at"], now + 1200.0);
    assert_eq!(row["extra"], "kept");
    assert_eq!(row["other_extra"], 7);
    assert_eq!(row["attempts"], 0);
}
#[test]
fn record_window_errors_never_overwrite_bad_state_or_remove_memory_cooldown() {
    let now = timeutil::now();
    for content in ["broken", "{\"windows\":null}"] {
        let dir = Temp::new();
        let a = account("u");
        let pool = AccountPool::new(vec![a.clone()]);
        fs::write(dir.0.join("model_windows.json"), content).unwrap();
        assert!(pool
            .record_model_window(&dir.0, "u", "m", now + 600.0, now)
            .is_err());
        assert_eq!(
            fs::read_to_string(dir.0.join("model_windows.json")).unwrap(),
            content
        );
        assert_eq!(a.throttle_wait("m", now), 600.0);
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }
    let dir = Temp::new();
    let a = account("u");
    let pool = AccountPool::new(vec![a.clone()]);
    let not_dir = dir.0.join("not-a-directory");
    fs::write(&not_dir, "unchanged").unwrap();
    assert!(pool
        .record_model_window(&not_dir, "u", "m", now + 600.0, now)
        .is_err());
    assert_eq!(a.throttle_wait("m", now), 600.0);
}
#[test]
fn non_file_window_state_is_rejected_without_breaking_account_load() {
    let dir = Temp::new();
    let now = timeutil::now();
    account("u").save_to(&dir.0).unwrap();
    fs::create_dir(dir.0.join("model_windows.json")).unwrap();
    let (pool, issues) = AccountPool::load_dir(&dir.0).unwrap();
    assert_eq!(pool.accounts().len(), 1);
    assert_eq!(issues.len(), 1);
    assert!(pool
        .record_model_window(&dir.0, "u", "m", now + 600.0, now)
        .is_err());
    assert!(dir.0.join("model_windows.json").is_dir());
    assert_eq!(pool.get("u").unwrap().throttle_wait("m", now), 600.0);
}
#[cfg(any(unix, windows))]
#[test]
fn symlink_window_state_is_not_followed_or_replaced() {
    let dir = Temp::new();
    let target_dir = Temp::new();
    let target = target_dir.0.join("outside.json");
    let content = b"{\"version\":1,\"windows\":[]}";
    fs::write(&target, content).unwrap();
    let path = dir.0.join("model_windows.json");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &path).unwrap();
    #[cfg(windows)]
    if let Err(error) = std::os::windows::fs::symlink_file(&target, &path) {
        // Windows without Developer Mode requires elevation to create symlinks.
        if error.raw_os_error() == Some(1314) {
            eprintln!("symlink case unavailable: Windows symlink privilege is disabled");
            return;
        }
        panic!("cannot create test symlink: {error}");
    }
    let now = timeutil::now();
    let pool = AccountPool::new(vec![account("u")]);
    assert!(pool.restore_window_cooldowns(&dir.0, now).is_err());
    assert!(pool
        .record_model_window(&dir.0, "u", "m", now + 600.0, now)
        .is_err());
    assert!(fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(target).unwrap(), content);
}
#[test]
fn record_window_validates_input_and_bounds_file_and_row_counts() {
    let dir = Temp::new();
    let now = timeutil::now();
    let a = account("u");
    let pool = AccountPool::new(vec![a.clone()]);
    for (uid, model, reset) in [
        ("missing", "m", now + 600.0),
        ("u", "", now + 600.0),
        ("u", "m", now),
        ("u", "m", f64::NAN),
        ("u", "m", f64::INFINITY),
    ] {
        assert!(!pool
            .record_model_window(&dir.0, uid, model, reset, now)
            .unwrap());
    }
    assert!(!dir.0.join("model_windows.json").exists());
    let oversized = vec![b' '; 4 * 1024 * 1024 + 1];
    fs::write(dir.0.join("model_windows.json"), &oversized).unwrap();
    assert!(pool.restore_window_cooldowns(&dir.0, now).is_err());
    assert!(pool
        .record_model_window(&dir.0, "u", "m", now + 600.0, now)
        .is_err());
    assert_eq!(
        fs::metadata(dir.0.join("model_windows.json"))
            .unwrap()
            .len(),
        oversized.len() as u64
    );
    let rows = (0..10_000)
        .map(|i| window("u", &format!("model-{i}"), now + 600.0))
        .collect::<Vec<_>>();
    write_windows(&dir, json!(rows));
    assert!(pool
        .record_model_window(&dir.0, "u", "extra", now + 600.0, now)
        .is_err());
    let stored: Value =
        serde_json::from_slice(&fs::read(dir.0.join("model_windows.json")).unwrap()).unwrap();
    assert_eq!(stored["windows"].as_array().unwrap().len(), 10_000);
    assert_eq!(a.throttle_wait("extra", now), 600.0);
}
#[test]
fn concurrent_window_records_from_distinct_pools_do_not_lose_rows() {
    let dir = Temp::new();
    let now = timeutil::now();
    let barrier = Arc::new(Barrier::new(8));
    let threads = (0..8)
        .map(|i| {
            let path = dir.0.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let pool = AccountPool::new(vec![account("u")]);
                barrier.wait();
                pool.record_model_window(path, "u", &format!("m-{i}"), now + 600.0, now)
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        assert!(thread.join().unwrap());
    }
    let pool = AccountPool::new(vec![account("u")]);
    assert_eq!(pool.restore_window_cooldowns(&dir.0, now).unwrap(), 8);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}

#[test]
fn weighted_round_robin_manual_priority_and_rotation() {
    let a = account("a");
    a.set_weight(5).unwrap();
    let b = account("b");
    b.set_weight(2).unwrap();
    let c = account("c");
    let pool = AccountPool::new(vec![a.clone(), b.clone(), c.clone()]);
    let opts = PickOptions::default();
    let mut counts = HashMap::new();
    for _ in 0..70 {
        *counts
            .entry(pool.pick(&opts).unwrap().uid().to_owned())
            .or_insert(0) += 1;
    }
    assert_eq!(counts.get("a"), Some(&50));
    assert_eq!(counts.get("b"), Some(&20));
    assert_eq!(counts.get("c"), None);
    a.set_weight(1).unwrap();
    b.set_weight(1).unwrap();
    let seq = (0..6)
        .map(|_| pool.pick(&opts).unwrap().uid().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(seq, ["a", "b", "c", "a", "b", "c"]);
}
#[test]
fn expiring_remaining_order_and_filters() {
    let a = account("a");
    let b = account("b");
    b.set_credits(json!({"packages":[{"package_code":"free","remain":10,"days_left":2.0}]}))
        .unwrap();
    let pool = AccountPool::new(vec![a.clone(), b.clone()]);
    let mut opts = PickOptions {
        model: "m".into(),
        ..Default::default()
    };
    assert_eq!(pool.pick(&opts).unwrap().uid(), "b");
    let mut guards = b.guards();
    guards.expiring_window_days = 0.0;
    b.set_guards(guards);
    pool.apply_remaining_weights(HashMap::from([(("a".into(), "m".into()), 4)]));
    assert_eq!(pool.pick(&opts).unwrap().uid(), "a");
    opts.allowed = Some(HashSet::new());
    assert!(pool.pick(&opts).is_none());
    opts.allowed = Some(HashSet::from(["b".into()]));
    assert_eq!(pool.pick(&opts).unwrap().uid(), "b");
    opts.realm = Some(Realm::Cn);
    assert!(pool.pick(&opts).is_none());
}
#[test]
fn affinity_ttl_history_pages_and_allowed() {
    let mut aff = SessionAffinity::default();
    aff.bind("s", "a", 0.);
    aff.bind("s", "b", 1.);
    aff.bind("s", "c", 2.);
    aff.bind("s", "d", 3.);
    assert_eq!(aff.recent("s", 4.), ["d", "c", "b"]);
    aff.demote("s", Some("d"));
    assert_eq!(aff.get("s", 5.), Some("c".into()));
    assert!(aff.recent("s", 8000.).is_empty());
    let accounts = (0..6).map(|i| account(&i.to_string())).collect::<Vec<_>>();
    let pool = AccountPool::new(accounts);
    let mut opts = PickOptions {
        session_key: "s".into(),
        ..Default::default()
    };
    assert_eq!(pool.pick(&opts).unwrap().uid(), "0");
    assert_eq!(pool.pick(&opts).unwrap().uid(), "0");
    opts.allowed = Some(HashSet::from(["1".into()]));
    assert_eq!(pool.pick(&opts).unwrap().uid(), "1");
    opts.allowed = None;
    opts.page = Some((0, 2));
    for _ in 0..10 {
        assert_eq!(
            pool.pick(&opts).unwrap().uid().parse::<usize>().unwrap() % 2,
            0
        );
    }
    let messages = vec![
        json!({"content":"你好","role":"system"}),
        json!({"role":"user","content":"hello"}),
    ];
    let (key, page) = affinity_route(&messages, false, 0, 3);
    assert_eq!(
        key,
        format!(
            "pfx-{}",
            &ids::sha256_hex(&wbcore::pyjson::dumps_sorted(&json!(messages)))[..16]
        )
    );
    assert_eq!(page, None);
}
#[test]
fn leases_release_on_error_panic_limit_change_and_threads() {
    let a = account("u");
    let pool = AccountPool::new(vec![a.clone()]);
    let opts = PickOptions::default();
    let lease = pool.acquire(&opts, 1).unwrap();
    assert_eq!(a.in_flight(), 1);
    assert!(matches!(pool.acquire(&opts, 1), Err(PoolError::Busy)));
    drop(lease);
    assert_eq!(a.in_flight(), 0);
    let run = || -> std::result::Result<(), ()> {
        let _lease = pool.acquire(&opts, 1).unwrap();
        Err(())
    };
    assert!(run().is_err());
    assert_eq!(a.in_flight(), 0);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _lease = pool.acquire(&opts, 1).unwrap();
        panic!("simulated cancellation")
    }));
    assert_eq!(a.in_flight(), 0);
    let unlimited = a.try_acquire_slot(0).unwrap();
    assert!(a.try_acquire_slot(1).is_none());
    drop(unlimited);
    assert_eq!(a.in_flight(), 0);
    let barrier = Arc::new(Barrier::new(9));
    let threads = (0..8)
        .map(|_| {
            let a = a.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let slot = a.try_acquire_slot(2);
                barrier.wait();
                slot.is_some()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    assert_eq!(
        threads
            .into_iter()
            .filter(|t| t.thread().id() != std::thread::current().id())
            .map(|t| t.join().unwrap() as usize)
            .sum::<usize>(),
        2
    );
    assert_eq!(a.in_flight(), 0);
}
#[test]
fn import_reload_delete_readd_preserves_arc_slots_and_penalties() {
    let dir = Temp::new();
    let a = account("u");
    a.save_to(&dir.0).unwrap();
    let pool = AccountPool::new(vec![a.clone()]);
    let slot = a.try_acquire_slot(1).unwrap();
    a.note_restricted("11140", timeutil::now()).unwrap();
    pool.reload_dir(&dir.0).unwrap();
    assert!(Arc::ptr_eq(&a, &pool.get("u").unwrap()));
    assert_eq!(a.in_flight(), 1);
    let added = pool
        .import(
            &json!({"uid":"u","accessToken":jwt("u",4_000_000_001i64)}),
            None,
            &dir.0,
        )
        .unwrap();
    assert!(Arc::ptr_eq(&a, &added[0]));
    assert!(a.throttle_wait("", timeutil::now()) > 7000.);
    pool.delete("u", true).unwrap();
    assert!(pool.get("u").is_none());
    let back = pool.upsert(account("u"), Some(&dir.0)).unwrap();
    assert!(Arc::ptr_eq(&a, &back));
    assert!(back.try_acquire_slot(1).is_none());
    drop(slot);
    assert_eq!(back.in_flight(), 0);
}
#[test]
fn load_directory_reports_invalid_and_skips_metadata() {
    let dir = Temp::new();
    fs::write(dir.0.join("settings.json"), "{}").unwrap();
    fs::write(dir.0.join("bad.json"), "garbage").unwrap();
    account("u").save_to(&dir.0).unwrap();
    let (pool, issues) = AccountPool::load_dir(&dir.0).unwrap();
    assert_eq!(pool.accounts().len(), 1);
    assert_eq!(issues.len(), 1);
}
#[test]
fn restriction_wave_is_deduplicated() {
    let pool = AccountPool::new(vec![account("a"), account("b")]);
    pool.note_account_restriction(Realm::Intl, "a", 1000.);
    pool.note_account_restriction(Realm::Intl, "a", 1001.);
    assert_eq!(pool.restriction_hold_remaining(Realm::Intl, 1001.), 0.);
    pool.note_account_restriction(Realm::Intl, "b", 1002.);
    let opts = PickOptions {
        realm: Some(Realm::Intl),
        now: 1002.,
        ..Default::default()
    };
    assert!(matches!(
        pool.acquire(&opts, 1),
        Err(PoolError::Restricted {
            retry_after: 1800.0
        })
    ));
    assert_eq!(pool.restriction_hold_remaining(Realm::Cn, 1002.), 0.);
}
#[test]
fn response_classification_and_reset_times() {
    assert_eq!(
        classify_response(&response(403, json!({"code":"11140"}))),
        ResponseAction::Restricted
    );
    assert_eq!(
        classify_response(&response(200, json!({"code":11140}))),
        ResponseAction::Restricted
    );
    assert_eq!(
        classify_response(&response(500, json!({"code":11134}))),
        ResponseAction::ModelError
    );
    assert_eq!(
        classify_response(&response(503, json!({}))),
        ResponseAction::Failure
    );
    assert_eq!(
        classify_response(&response(400, json!({"code":11101}))),
        ResponseAction::PassThrough
    );
    let reset = parse_rate_limit_reset("usage will reset at 2026-10-06 21:00:09 UTC+8").unwrap();
    assert_eq!(reset, 1_791_320_409. - 28800.);
    assert_eq!(
        parse_rate_limit_reset(
            r#"{"msg":"\u5c06\u5728 2026-10-06 21:00:09 UTC+08:00 \u91cd\u7f6e"}"#
        ),
        Some(reset)
    );
    assert!(rate_limit_is_account_level("account rate limit", None));
    assert!(!rate_limit_is_account_level("accountId: 123", None));
    assert!(!rate_limit_is_account_level(
        "account rate limit",
        Some(reset)
    ));
    let a = account("u");
    let now = reset - 300.;
    let r = response(
        429,
        json!({"code":6004,"msg":"reset at 2026-10-06 21:00:09 UTC+8"}),
    );
    assert!(matches!(
        a.apply_response(&r, "hy3", now).unwrap(),
        ResponseAction::RateLimited { cap: true, .. }
    ));
    assert_eq!(a.cap_until("hy3", now), reset);
    assert_eq!(
        a.apply_response(
            &response(429, json!({"code":14003,"msg":"too many requests"})),
            "paid",
            now
        )
        .unwrap(),
        ResponseAction::RateLimited {
            account_scope: false,
            reset_at: None,
            soft: true,
            cap: false
        }
    );
    assert_eq!(a.throttle_wait("paid", now), 60.);
}
#[test]
fn desktop_chat_headers_order_ids_and_body() {
    let a = account("u-123");
    let c = a.cred();
    let options = ChatOptions {
        session_key: "pfx-0123456789abcdef".into(),
        ..Default::default()
    };
    let body = json!({"model":"hy3","messages":[],"stream":true,"n":0.00001});
    let req = build_chat_request(&c, &body, &options).unwrap();
    assert_eq!(req.case, HeaderCase::Exact);
    assert_eq!(req.url, "https://www.workbuddy.ai/v2/chat/completions");
    assert_eq!(
        req.header("X-Conversation-ID"),
        Some("c26f60d7-a3c6-4ac1-a98a-d43ea8b5b6a1")
    );
    assert_eq!(
        req.header("X-Request-ID"),
        req.header("X-Conversation-Message-ID")
    );
    assert_eq!(
        req.header("X-Root-Request-ID"),
        req.header("X-Conversation-Request-ID")
    );
    assert!(req.header("X-Machine-ID").is_none());
    assert!(req.header("Content-Encoding").is_none());
    assert_eq!(
        req.body.unwrap(),
        wbcore::pyjson::dumps_compact(&body).into_bytes()
    );
    let names = req
        .headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        &names[..8],
        &[
            "Content-Type",
            "Accept",
            "X-Requested-With",
            "Origin",
            "Referer",
            "X-CodeBuddy-Request",
            "Accept-Language",
            "X-Agent-Intent"
        ]
    );
}
#[test]
fn cli_vscode_gzip_tail_headers_and_no_switch() {
    let a = account("u");
    let mut c = a.cred();
    c.realm = Realm::Cn;
    c.product = Product::Cli;
    c.enterprise_id = "enterprise".into();
    c.access_token = "ck_id.secret".into();
    let opts = ChatOptions {
        turn_request_id: Some("turn-id".into()),
        device_token: Some("device".into()),
        ..Default::default()
    };
    let body = json!({"model":"m","metadata":{},"conversation_id":"remove","previous_response_id":"remove","messages":[]});
    let req = build_chat_request(&c, &body, &opts).unwrap();
    assert_eq!(req.header("X-Conversation-Request-ID"), Some("turn-id"));
    assert_eq!(req.header("X-Private-Data"), Some("true"));
    assert_eq!(req.header("X-API-Key"), Some("ck_id.secret"));
    assert!(req.header("Origin").is_none());
    assert!(req.header("X-Device-Token").is_none());
    assert_eq!(req.headers.last().unwrap().0, "Content-Encoding");
    let bytes = req.body.unwrap();
    assert_eq!(&bytes[..10], &[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 2, 255]);
    let mut raw = String::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .read_to_string(&mut raw)
        .unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["cache_retention"], "long");
    assert!(v.get("metadata").is_none());
    assert!(body.get("metadata").is_some());
    assert_eq!(c.product, Product::Cli);
    c.product = Product::Vscode;
    c.enterprise_id.clear();
    let req = build_chat_request(&c, &body, &opts).unwrap();
    assert_eq!(req.url, "https://www.workbuddy.cn/v2/chat/completions");
    assert_eq!(req.header("X-Device-Token"), Some("device"));
    assert_eq!(req.header("X-Model-ID"), Some("m"));
    assert!(req.header("traceparent").is_none());
    assert!(req.header("X-Private-Data").is_none());
    assert_eq!(
        req.header("X-Request-ID"),
        req.header("X-Conversation-Request-ID")
    );
}
#[test]
fn noncanonical_loaded_path_import_refresh_restart_has_no_stale_duplicate() {
    let dir = Temp::new();
    let fixture = dir.0.join("fixture.json");
    let now = timeutil::now();
    fs::write(
        &fixture,
        serde_json::to_vec(&account("test-user").to_json()).unwrap(),
    )
    .unwrap();
    let (pool, issues) = AccountPool::load_dir(&dir.0).unwrap();
    assert!(issues.is_empty());
    let a = pool.get("test-user").unwrap();
    a.note_restricted("11140", now).unwrap();
    pool.import(&json!({"uid":"test-user","accessToken":jwt("test-user",4_000_000_001i64),"refreshToken":"import-r"}),None,&dir.0).unwrap();
    let newest = jwt("test-user", 4_000_000_002i64);
    a.apply_refresh_response(&response(
        200,
        json!({"data":{"accessToken":newest,"refreshToken":"newest-r"}}),
    ))
    .unwrap();
    assert_eq!(a.path().unwrap(), fixture);
    assert!(!dir.0.join("test-user.json").exists());
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    let (restarted, issues) = AccountPool::load_dir(&dir.0).unwrap();
    assert!(issues.is_empty());
    assert_eq!(restarted.accounts().len(), 1);
    let loaded = restarted.get("test-user").unwrap();
    assert_eq!(loaded.cred().access_token, newest);
    assert_eq!(loaded.cred().refresh_token, "newest-r");
    assert_eq!(loaded.throttle_wait("", now), 7200.);
}
#[test]
fn refresh_request_and_response_synchronization() {
    let dir = Temp::new();
    let a = account("u");
    a.save_to(&dir.0).unwrap();
    let req = build_refresh_request(&a.cred(), &RequestOptions::default()).unwrap();
    assert_eq!(req.case, HeaderCase::Title);
    assert_eq!(req.body.as_deref(), Some(b"{}".as_slice()));
    assert!(req.header("Authorization").is_none());
    assert_eq!(req.header("X-Refresh-Token"), Some("refresh-secret"));
    let lock = a.try_refresh_slot().unwrap();
    assert!(a.try_refresh_slot().is_none());
    drop(lock);
    assert!(a.try_refresh_slot().is_some());
    let new = jwt("u", 4_000_000_123);
    a.apply_refresh_response(&response(
        200,
        json!({"data":{"data":{"accessToken":new,"refreshToken":"rotated","expiresAt":1}}}),
    ))
    .unwrap();
    assert_eq!(a.cred().refresh_token, "rotated");
    assert_eq!(a.to_json()["expiresAt"], 4_000_000_123i64);
    assert_eq!(
        Account::load(a.path().unwrap())
            .unwrap()
            .cred()
            .access_token,
        new
    );
    assert!(a
        .apply_refresh_response(&response(
            200,
            json!({"data":{"accessToken":jwt("other",4_000_000_000)}})
        ))
        .is_err());
    assert_eq!(a.uid(), "u");
    assert!(a
        .apply_refresh_response(&response(200, json!({"data":{}})))
        .is_err());
    assert!(matches!(
        a.readiness("", timeutil::now()),
        Readiness::Blocked {
            reason: BlockReason::Expired,
            ..
        }
    ));
    let api = Account::from_json(&json!({"accessToken":"ck_a.secret"})).unwrap();
    assert!(build_refresh_request(&api.cred(), &RequestOptions::default()).is_err());
}
#[test]
fn request_header_injection_rejected() {
    let mut c = account("u").cred();
    c.access_token = "token\r\nInjected: yes".into();
    assert!(build_chat_request(&c, &json!({}), &ChatOptions::default()).is_err());
    c = account("u").cred();
    let opts = ChatOptions {
        device_token: Some("x\r\ny".into()),
        ..Default::default()
    };
    c.realm = Realm::Cn;
    assert!(build_chat_request(&c, &json!({}), &opts).is_err());
    let mut opts = RequestOptions::default();
    opts.cn_chat_base = Some("https://user:password@evil/".into());
    assert!(build_refresh_request(&c, &opts).is_err());
}
#[test]
fn oauth_requests_encoding_pending_and_success() {
    let opts = RequestOptions {
        proxy: "http://proxy".into(),
        ..Default::default()
    };
    let start = build_oauth_start_request(Realm::Intl, "CLI &x=y", &opts).unwrap();
    assert!(start.url.ends_with("platform=CLI%20%26x%3Dy"));
    assert_eq!(start.egress, Egress::Direct);
    assert_eq!(start.case, HeaderCase::Title);
    let login = parse_oauth_start(
        &response(
            200,
            json!({"data":{"state":"state","authUrl":"https://example.org/login"}}),
        ),
        1000.,
    )
    .unwrap();
    assert_eq!(login.expires_at, 1600.);
    let poll = build_oauth_poll_request(Realm::Intl, "a/b?c=d", &opts).unwrap();
    assert!(poll.url.ends_with("state=a%2Fb%3Fc%3Dd"));
    assert!(poll.body.is_none());
    assert!(matches!(
        parse_oauth_poll(
            &response(200, json!({"code":11217,"msg":"waiting"})),
            Realm::Intl,
            "CLI"
        )
        .unwrap(),
        OAuthPoll::Pending { .. }
    ));
    let logged=parse_oauth_poll(&response(200,json!({"code":0,"data":{"accessToken":jwt("u",4_000_000_000),"refreshToken":"r","expiresAt":4_000_000_000_000i64}})),Realm::Intl,"CLI").unwrap();
    let OAuthPoll::Authorized(a) = logged else {
        panic!("not authorized")
    };
    assert_eq!(a.uid(), "u");
    assert_eq!(a.to_json()["expiresAt"], 4_000_000_000i64);
}
/// Consume shared immutable Python-oracle fixtures without running Python/network.
#[test]
fn python_golden_penalties_rates_restrictions_and_slots() {
    let golden: Value =
        serde_json::from_str(include_str!("../../../tests/baseline/golden.json")).unwrap();
    let mut checked = 0;
    for case in golden["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let input = &case["input"];
        let expected = &case["expected"];
        if id.starts_with("backoff.") {
            let (base, cap, offset) = match id {
                "backoff.soft_backoff" => (600., 7200., 1),
                "backoff.unverified_soft_backoff" => (60., 600., 1),
                "backoff.breaker_backoff" => (1800., 21600., 3),
                "backoff.degrade_backoff" => (600., 7200., 5),
                _ => continue,
            };
            let actual: Vec<_> = input["counts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| backoff(n.as_u64().unwrap() as u32, base, cap, offset))
                .collect();
            assert_eq!(json!(actual), *expected, "{id}");
            checked += 1;
        } else if id.starts_with("rate_limit.classify.") {
            let detail = input["detail"].as_str().unwrap();
            let reset = parse_rate_limit_reset(detail);
            assert_eq!(
                rate_limit_is_soft_shape(detail),
                expected["soft_shape"].as_bool().unwrap(),
                "{id}"
            );
            assert_eq!(reset, expected["reset_at"].as_f64(), "{id}");
            assert_eq!(
                rate_limit_is_account_level(detail, reset),
                expected["account_level"].as_bool().unwrap(),
                "{id}"
            );
            checked += 1;
        } else if id.starts_with("restriction.detect.") {
            assert_eq!(
                restriction_is_account_level(input["args"][0].as_str().unwrap_or("")),
                expected.as_bool().unwrap(),
                "{id}"
            );
            checked += 1;
        } else if matches!(
            id,
            "account.breaker_success"
                | "account.failure_window"
                | "account.unknown_degrade"
                | "account.independent_soft_ladders"
        ) {
            let a = account("golden");
            let mut now = 1_700_000_000.;
            for (step, expected) in input["steps"]
                .as_array()
                .unwrap()
                .iter()
                .zip(expected.as_array().unwrap())
            {
                now += step["advance"].as_f64().unwrap_or(0.);
                let message = step["args"][0].as_str().unwrap_or("");
                match step["method"].as_str().unwrap() {
                    "note_failure" => a.note_failure(message, now),
                    "note_unknown_failure" => a.note_unknown_failure(message, now),
                    "note_soft_rate" => {
                        assert_eq!(a.note_soft_rate(now), expected["return"].as_f64().unwrap());
                    }
                    "note_unscoped_rate" => {
                        assert_eq!(
                            a.note_unscoped_rate(message, now),
                            expected["return"].as_f64().unwrap()
                        );
                    }
                    "note_success" => a.note_success("", now),
                    other => panic!("unhandled oracle step {other}"),
                }
                let s = a.state.read();
                let r = &s.runtime;
                let e = &expected["state"];
                assert_eq!(
                    json!([r.fails, r.unknown, r.soft, r.unscoped]),
                    json!([
                        e["fails"],
                        e["degrade_count"],
                        e["soft_streak"],
                        e["unscoped_streak"]
                    ]),
                    "{id}"
                );
                assert_eq!(
                    json!([r.cooldown, r.breaker, r.degrade]),
                    json!([e["cooldown_until"], e["breaker_until"], e["degrade_until"]]),
                    "{id}"
                );
                drop(s);
                assert_eq!(
                    a.throttle_wait("", now),
                    expected["throttle_wait"].as_f64().unwrap(),
                    "{id}"
                );
                assert_eq!(
                    a.readiness("", now) == Readiness::Ready,
                    expected["ready"].as_bool().unwrap(),
                    "{id}"
                );
            }
            checked += 1;
        } else if id.starts_with("concurrency.account.") && input["limit"].as_u64().unwrap() > 0 {
            let accounts = HashMap::from([("a", account("a")), ("b", account("b"))]);
            let mut held: HashMap<String, Vec<AccountSlot>> = HashMap::new();
            for (action, expected) in input["actions"]
                .as_array()
                .unwrap()
                .iter()
                .zip(expected.as_array().unwrap())
            {
                let uid = action["id"].as_str().unwrap();
                let a = &accounts[uid];
                if action["op"] == "acquire" {
                    let slot = a.try_acquire_slot(input["limit"].as_u64().unwrap() as usize);
                    assert_eq!(slot.is_some(), expected["return"].as_bool().unwrap());
                    if let Some(slot) = slot {
                        held.entry(uid.into()).or_default().push(slot);
                    }
                } else {
                    held.entry(uid.into()).or_default().pop();
                }
                for (uid, a) in &accounts {
                    assert_eq!(
                        a.in_flight(),
                        expected["inflight"][*uid].as_u64().unwrap_or(0) as usize,
                        "{id}"
                    );
                }
            }
            checked += 1;
        } else if id == "restriction.wave_uid_dedup" {
            let pool = AccountPool::new(Vec::new());
            for (event, expected) in input["events"]
                .as_array()
                .unwrap()
                .iter()
                .zip(expected.as_array().unwrap())
            {
                let realm = Realm::parse(event["realm"].as_str().unwrap()).unwrap();
                let now = 1_700_000_000. + event["offset"].as_f64().unwrap();
                pool.note_account_restriction(realm, event["uid"].as_str().unwrap(), now);
                assert_eq!(
                    pool.restriction_hold_remaining(realm, now),
                    expected["remaining"].as_f64().unwrap()
                );
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 22, "selected oracle fixture coverage changed");
}
#[test]
fn billing_headers_and_enterprise_request() {
    let a = Account::from_json(
        &json!({"accessToken":"pt_id.secret","uid":"u-123","enterpriseId":"ent"}),
    )
    .unwrap();
    let req = build_billing_request(
        &a.cred(),
        BillingOperation::EnterpriseUsage,
        &json!({}),
        &RequestOptions::default(),
    )
    .unwrap();
    assert_eq!(
        req.url,
        "https://www.codebuddy.cn/v2/billing/meter/get-enterprise-user-usage"
    );
    assert_eq!(
        req.header("X-Machine-ID"),
        Some("98b53497-f10c-4475-a88a-9054ddd676f0")
    );
    assert_eq!(req.header("X-Client-Platform"), Some("web"));
    assert!(req.header("X-No-Enterprise-Id").is_none());
    assert_eq!(req.case, HeaderCase::Title);
    assert!(build_billing_request(
        &a.cred(),
        BillingOperation::Checkin,
        &json!({}),
        &RequestOptions::default()
    )
    .is_err());
}
