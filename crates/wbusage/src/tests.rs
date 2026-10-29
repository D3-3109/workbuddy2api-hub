use super::*;
use std::io::Write;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("wbusage-{}", wbcore::ids::uuid4_hex()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn store(&self) -> UsageStore {
        UsageStore::new(&self.0)
    }
    fn copy(&self, source: &str, target: &str) {
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pricing_tooltip");
        fs::copy(fixtures.join(source), self.0.join(target)).unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-9 * expected.abs().max(1.0),
        "{actual} != {expected}"
    );
}
fn fixture() -> Temp {
    let t = Temp::new();
    t.copy("usage_sample.jsonl", "usage.jsonl");
    t.copy("policies_live_before.jsonl", "pricing-policies.jsonl");
    t.copy("timeline_live_before.jsonl", "pricing-timeline.jsonl");
    t
}

#[test]
fn fixture_policy_hashes_and_cost_sources() {
    let t = fixture();
    let store = t.store();
    let mut n = 0;
    scan::scan(&t.0.join("pricing-policies.jsonl"), |p| {
        assert_eq!(
            text(&p["id"]),
            policy_id(
                &text(&p["model"]),
                &p["flat"],
                &p["bands"],
                number(&p, "usd_cny")
            )
        );
        n += 1;
    })
    .unwrap();
    assert_eq!(n, 52);
    t.copy("cost_sources_before.json", "expected.json");
    let expected: Value =
        serde_json::from_slice(&fs::read(t.0.join("expected.json")).unwrap()).unwrap();
    let mut i = 0;
    scan::scan(&t.0.join("usage.jsonl"), |row| {
        assert_eq!(
            store.cost_for_row(&row).unwrap()["source"],
            expected[i.to_string()],
            "source at row {i}"
        );
        i += 1;
    })
    .unwrap();
    assert_eq!(i, 496);
}

#[test]
fn fixture_totals_perf_recent_analytics() {
    let t = fixture();
    let store = t.store();
    let all = json!({"realm":"all"});
    let totals = store.query("/usage", &all, &json!({})).unwrap();
    assert_eq!(totals["requests"], 495);
    assert_eq!(totals["errors"], 1);
    assert_eq!(totals["prompt_tokens"], 51818553);
    assert_eq!(totals["total_tokens"], 53593076);
    close(number(&totals, "credit"), 6.81);
    close(number(&totals, "cost_cny"), 16.129016759762507);
    let perf = store.query("/usage/perf", &all, &json!({})).unwrap();
    assert_eq!(perf["sampled"], 496);
    assert_eq!(perf["ttft_ms"]["p50"], 2324);
    close(number(&perf["ttft_ms"], "avg"), 2559.4);
    let recent = store
        .query(
            "/usage/recent",
            &json!({"realm":"all","limit":2}),
            &json!({}),
        )
        .unwrap();
    assert_eq!(recent["total_pages"], 248);
    assert_eq!(recent["rows"][0]["cost_source"], "833aa685e15a");
    close(number(&recent["rows"][0], "cost_cny"), 0.011640);
    let analytics = store.query("/usage/analytics", &all, &json!({})).unwrap();
    assert_eq!(analytics["summary"]["window"]["requests"], 495);
    close(
        number(&analytics["summary"]["window"], "cost_cny"),
        number(&totals, "cost_cny"),
    );
    close(
        number(&analytics["summary"]["window"], "cache_hit_pct"),
        93.8,
    );
}

#[test]
fn abort_and_error_counting_differs_by_endpoint() {
    let t = Temp::new();
    let store = t.store();
    let now = timeutil::now();
    for (result, error, tokens) in [
        ("completed", false, 10),
        ("client_aborted", false, 20),
        ("failed", true, 30),
        ("failed", false, 40),
    ] {
        store.append(json!({"at":now,"model":"unknown","account":"a","outcome":result,"error":error,"total_tokens":tokens,"credit":1,"elapsed_ms":100})).unwrap();
    }
    let all = json!({"realm":"all"});
    let totals = store.query("/usage", &all, &Value::Null).unwrap();
    assert_eq!(totals["requests"], 1);
    assert_eq!(totals["errors"], 3);
    assert_eq!(totals["total_tokens"], 10);
    close(number(&totals, "credit"), 3.0);
    let perf = store.query("/usage/perf", &all, &Value::Null).unwrap();
    assert_eq!(perf["client_aborted"], 1);
    assert_eq!(perf["errors"], 2);
    close(number(&perf, "success_rate_pct"), 33.3);
    let analytics = store.query("/usage/analytics", &all, &Value::Null).unwrap();
    assert_eq!(analytics["summary"]["window"]["total_tokens"], 80);
    let by = store
        .query("/usage/by-account", &all, &Value::Null)
        .unwrap();
    assert_eq!(by["accounts"][0]["requests"], 3);
    assert_eq!(by["accounts"][0]["total_tokens"], 70);
    let daily = store.daily_totals().unwrap();
    assert_eq!(daily["tokens"]["a"], 80);
    assert_eq!(daily["models"]["a"]["unknown"], 80);
    close(num(&daily["credits"]["a"]), 3.0);
    assert_eq!(store.daily_totals().unwrap(), daily);
    let ts = store.query("/usage/timeseries",&json!({"realm":"all","range":"custom","since":now-1.0,"until":now+1.0,"bucket_seconds":60}),&Value::Null).unwrap();
    assert_eq!(ts["bucket_seconds"], 60);
    assert_eq!(ts["series"][0]["requests"], 1);
    assert_eq!(ts["series"][0]["errors"], 3);
    assert_eq!(ts["credits"].as_array().unwrap().len(), 4);
}

#[test]
fn incremental_partial_invalid_and_replaced_log() {
    let t = Temp::new();
    let store = t.store();
    let now = timeutil::now();
    let path = t.0.join("usage.jsonl");
    let row = pyjson::dumps_u(&json!({"at":now,"account":"a","model":"x","total_tokens":3}));
    fs::write(&path, format!("{row}\n\nnot-json\n{row}")).unwrap();
    assert_eq!(store.daily_totals().unwrap()["tokens"]["a"], 3);
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    assert_eq!(store.daily_totals().unwrap()["tokens"]["a"], 6);
    assert_eq!(store.daily_totals().unwrap()["tokens"]["a"], 6);
    fs::write(&path, format!("{row}\n")).unwrap();
    assert_eq!(store.daily_totals().unwrap()["tokens"]["a"], 3);
    let recent = store
        .query("/usage/recent", &json!({"realm":"all"}), &Value::Null)
        .unwrap();
    assert_eq!(recent["total"], 1);
    fs::remove_file(&path).unwrap();
    assert_eq!(store.daily_totals().unwrap()["tokens"], json!({}));
}

#[test]
fn append_format_policy_record_and_disable() {
    let t = Temp::new();
    let store = t.store();
    let snapshot = json!({"meta":{"usd_cny":7.1},"models":{"m":{"flat":{"input_cache_hit":1.0,"input_cache_miss":2.0,"output":3.0},"currency":"USD","unit":1000000}}});
    let recorded = store.record_policies(&snapshot, 10.0, false).unwrap();
    assert_eq!(recorded["new_policies"], 1);
    assert_eq!(recorded["timeline_changed"], true);
    let again = store.record_policies(&snapshot, 11.0, false).unwrap();
    assert_eq!(again["new_policies"], 0);
    assert_eq!(again["timeline_changed"], false);
    store.append(json!({"model":"m","at":12.0,"prompt_tokens":1000000,"cached_tokens":2000000,"completion_tokens":1000000,"reasoning_tokens":999999,"message":"中文"})).unwrap();
    let bytes = fs::read_to_string(t.0.join("usage.jsonl")).unwrap();
    assert!(bytes.starts_with("{\"at\": 12.0, "));
    assert!(bytes.contains("中文"));
    let row: Value = serde_json::from_str(bytes.trim()).unwrap();
    assert_eq!(row["cost_policy"], recorded["assignment"]["m"]);
    close(number(&store.cost_for_row(&row).unwrap(), "cny"), 28.4);
    store.set_pricing_enabled(false);
    let cost = store.cost_for_row(&row).unwrap();
    assert_eq!(cost["disabled"], true);
    assert_eq!(store.current_policy_id("m").unwrap(), None);
    assert!(store.record_policies(&snapshot, 12.0, false).is_err());
    assert_eq!(
        store.refresh_pricing().unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(
        store
            .query("/usage/remaining", &Value::Null, &Value::Null)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
}

#[test]
fn timeline_backfill_bands_and_markup() {
    let t = Temp::new();
    let store = t.store();
    let entry = json!({"currency":"USD","flat":{"input_cache_miss":10.0,"input_cache_hit":1.0,"output":50.0},"bands":[{"min_prompt_tokens":272000,"flat":{"input_cache_hit":2.0,"input_cache_miss":20.0,"output":75.0}}]});
    store
        .record_policies(
            &json!({"meta":{"usd_cny":7.1},"models":{"other":entry}}),
            10.0,
            false,
        )
        .unwrap();
    store
        .record_policies(
            &json!({"meta":{"usd_cny":7.1},"models":{"m":entry}}),
            20.0,
            false,
        )
        .unwrap();
    let row = json!({"model":"m","at":15.0,"prompt_tokens":300000,"cached_tokens":100000,"completion_tokens":1000});
    let cost = store.cost_for_row(&row).unwrap();
    assert_eq!(cost["backfilled"], true);
    assert_eq!(cost["band"], 0);
    assert_eq!(cost["band_note"], "输入长度 ≥ 272,000 tokens");
    close(number(&cost, "cny"), 30.3525);
    fs::write(t.0.join("pricing-markup.json"), r#"{"m":2.0}"#).unwrap();
    close(number(&store.cost_for_row(&row).unwrap(), "cny"), 60.705);
    let missing = store
        .cost_for_row(&json!({"model":"does-not-exist"}))
        .unwrap();
    assert_eq!(missing["known"], false);
    assert_eq!(missing["rates"], Value::Null);
}

#[test]
fn malformed_oversized_and_partial_lines_stay_bounded() {
    let t = Temp::new();
    let store = t.store();
    let path = t.0.join("usage.jsonl");
    let mut bytes = vec![b'x'; scan::MAX_LINE + 128];
    bytes.extend_from_slice(b"\n\xff\n[]\n\n");
    bytes.extend_from_slice(b"{\"at\":1,\"total_tokens\":2}\n{\"at\":2}");
    fs::write(&path, bytes).unwrap();
    let totals = store
        .query("/usage", &json!({"realm":"all"}), &Value::Null)
        .unwrap();
    assert_eq!(totals["requests"], 1);
    let recent = store
        .query("/usage/recent", &json!({"realm":"all"}), &Value::Null)
        .unwrap();
    assert_eq!(recent["total"], 4);
    assert_eq!(recent["rows"].as_array().unwrap().len(), 1);
    assert!(store
        .append(json!({"message":"x".repeat(scan::MAX_LINE)}))
        .is_err());
    assert!(store.append(json!([])).is_err());
}

#[test]
fn custom_bounds_realm_fallback_and_perf_sampling() {
    let t = Temp::new();
    let store = t.store();
    for i in 0..12 {
        store.append(json!({"at":i,"model":if i < 2 {"minimax-m3"} else {"m"},"ttft_ms":i,"total_tokens":1})).unwrap();
    }
    let window =
        json!({"realm":"all","range":"custom","since":"4","until":["2"],"bucket_seconds":"60"});
    assert_eq!(
        store.query("/usage", &window, &Value::Null).unwrap()["requests"],
        3
    );
    let series = store
        .query("/usage/timeseries", &window, &Value::Null)
        .unwrap();
    assert_eq!(series["series"][0]["requests"], 3);
    let perf = store
        .query(
            "/usage/perf",
            &json!({"realm":"all","sample":10}),
            &Value::Null,
        )
        .unwrap();
    assert_eq!(perf["sample_from"], 2);
    assert_eq!(perf["sample_capped"], true);
    assert_eq!(perf["sampled"], 10);
    // Python ties-to-even rank: round(.5 * 9) = 4, selecting 6 not 7.
    assert_eq!(perf["ttft_ms"]["p50"], 6);
    assert_eq!(
        store
            .query("/usage", &json!({"realm":"cn"}), &Value::Null)
            .unwrap()["requests"],
        2
    );
    assert_eq!(
        store
            .query("/usage", &json!({"range":"7d","realm":"all"}), &Value::Null)
            .unwrap()["requests"],
        12
    );
    let page = store
        .query(
            "/usage/recent",
            &json!({"realm":"all","limit":5,"page":100}),
            &Value::Null,
        )
        .unwrap();
    assert_eq!(page["page"], 3);
    assert_eq!(page["rows"].as_array().unwrap().len(), 2);
    assert_eq!(page["rows"][0]["at"], 1);
}

#[test]
fn daily_rollover_and_same_size_replacement() {
    let t = Temp::new();
    let store = t.store();
    let now = timeutil::now();
    let midnight = timeutil::local_midnight(now, 0);
    let row = json!({"at":midnight+1.0,"account":"a","total_tokens":3});
    store.append(row.clone()).unwrap();
    assert_eq!(store.daily_totals_at(now).unwrap()["tokens"]["a"], 3);
    let mut replacement = row;
    replacement["total_tokens"] = json!(7);
    replacement["iso"] = json!(timeutil::local_iso(midnight + 1.0));
    replacement["cost_policy"] = Value::Null;
    fs::write(
        t.0.join("usage.jsonl"),
        format!("{}\n", pyjson::dumps_u(&replacement)),
    )
    .unwrap();
    assert_eq!(store.daily_totals_at(now).unwrap()["tokens"]["a"], 7);
    assert_eq!(
        store
            .daily_totals_at(timeutil::local_midnight(now, -1) + 1.0)
            .unwrap()["tokens"],
        json!({})
    );
}

#[test]
fn concurrent_append_serializes_complete_rows() {
    let t = Temp::new();
    let store = std::sync::Arc::new(t.store());
    let now = timeutil::now();
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let store = store.clone();
            std::thread::spawn(move || {
                for _ in 0..10 {
                    store
                        .append(json!({"at":now,"account":"a","total_tokens":1}))
                        .unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(store.daily_totals().unwrap()["tokens"]["a"], 40);
    assert_eq!(
        store
            .query("/usage", &json!({"realm":"all"}), &Value::Null)
            .unwrap()["requests"],
        40
    );
}

#[test]
fn utc_windows_last_match_wins_and_null_cache_rate() {
    let t = Temp::new();
    let store = t.store();
    store.record_policies(&json!({"meta":{"usd_cny":7.1},"models":{"m":{"currency":"CNY","flat":{"input_cache_miss":1},"bands":[
        {"days":["mon"],"start":0,"end":2400,"flat":{"input_cache_miss":2}},
        {"days":null,"start":1600,"end":2400,"flat":{"input_cache_miss":3,"input_cache_hit":null}}
    ]}}}),0.0,false).unwrap();
    // Monday 2026-10-26, 17:00 UTC; later matching band overrides Monday band.
    let at = timeutil::timegm_stamp("2026-10-26 17:00:00").unwrap();
    let cost = store
        .cost_for_row(&json!({"at":at,"model":"m","prompt_tokens":1000000,"cached_tokens":1000000}))
        .unwrap();
    assert_eq!(cost["band"], 1);
    close(number(&cost, "cny"), 3.0);
    assert_eq!(cost["band_note"], "每天 16:00–24:00 UTC");
}

#[test]
fn analytics_key_buckets_and_zero_traffic_accounts() {
    let t = Temp::new();
    let store = t.store();
    for key in [None, Some(""), Some("deleted"), Some("live")] {
        let mut row = json!({"at":1,"account":"old","realm":"cn","model":"m","total_tokens":10});
        if let Some(k) = key {
            row["key"] = json!(k);
        }
        store.append(row).unwrap();
    }
    let metadata = json!({"accounts":[{"uid":"new","nickname":"New","realm":"intl","credits":{"remaining":5}}],"api_keys":[{"id":"live","name":"Named"},{"id":"disabled","enabled":false}]});
    let out = store
        .query("/usage/analytics", &Value::Null, &metadata)
        .unwrap();
    assert_eq!(out["accounts"].as_array().unwrap().len(), 2);
    let keys = out["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 4);
    assert!(keys.iter().any(|k| k["key"] == "__before_keys__"));
    assert!(keys.iter().any(|k| k["key"] == "__no_key__"));
    assert!(keys.iter().any(|k| k["key"] == "__unknown_key__"));
    assert!(keys.iter().any(|k| k["name"] == "Named"));
}

/// The index must answer every aggregate view exactly like the streaming scan,
/// including JSON number types and key order, on irregular rows.
#[test]
fn indexed_views_match_streaming_views() {
    let t = fixture();
    let store = t.store();
    let now = timeutil::now();
    let mut extra = String::new();
    let odd = [
        json!({"at":now-10.0,"model":"hy3","account":"x","credit":"1.5","total_tokens":"7","prompt_tokens":3.5,"key":"k1"}),
        json!({"at":now-9.0,"model":"","account":"","realm":"cn","error":"boom","total_tokens":null,"key":null}),
        json!({"at":now-8.0,"model":"minimax-m3","outcome":"client_aborted","credit":2,"ttft_ms":0,"tokens_per_sec":0}),
        json!({"at":now-7.0,"model":"gpt-5.5","account":"y","cached_tokens":10,"prompt_tokens":5,"completion_tokens":1e6,"cost_policy":"nope"}),
        json!({"at":now-6.0}),
        json!({"at":now-5.0,"model":"deepseek-v4.1-flash","account":"x","realm":"intl","credit":0.25,"key":"","elapsed_ms":"12"}),
        json!({"at":"bad","model":7,"account":true,"key":3,"realm":["cn"]}),
    ];
    for row in &odd {
        extra.push_str(&pyjson::dumps_u(row));
        extra.push('\n');
    }
    extra.push_str("{not json}\n[1,2]\n   \n");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(t.0.join("usage.jsonl"))
        .unwrap();
    file.write_all(extra.as_bytes()).unwrap();
    file.write_all(b"{\"at\":1,\"partial\":").unwrap();
    drop(file);
    let metadata = json!({"realm":"intl","accounts":[{"uid":"acct-0001","realm":"cn","nickname":"A"},{"uid":"x","realm":"intl"}],
        "api_keys":[{"id":"k1","name":"One"},{"id":"gone","enabled":false}],"model_realms":{"hy3":"cn"}});
    let windows = [
        json!({}),
        json!({"realm":"all"}),
        json!({"realm":"cn","range":"today"}),
        json!({"realm":"intl","range":"custom","since":1790418153.0,"until":1790500000.0}),
        json!({"range":"custom","since":now-9.5,"until":now,"bucket":"60"}),
        json!({"realm":"all","range":"week","limit":"3","page":"2","sample":"10"}),
        json!({"realm":"all","limit":"1000","page":"1","sample":"20000"}),
        json!({"realm":"nowhere","page":"99"}),
    ];
    let pricing = store.pricing().unwrap();
    let log = t.0.join("usage.jsonl");
    for params in &windows {
        let ctx = Context::new(params, &metadata);
        let mut ix = index::Index::default();
        ix.refresh(&log).unwrap();
        let pairs = [
            (
                "totals",
                views::totals(&log, &ctx, &pricing, 1.0).unwrap(),
                index::totals(&mut ix, &log, &ctx, &pricing, 1.0).unwrap(),
            ),
            (
                "recent",
                views::recent(&log, &ctx, &pricing).unwrap(),
                index::recent(&mut ix, &log, &ctx, &pricing)
                    .unwrap()
                    .unwrap(),
            ),
            (
                "perf",
                views::perf(&log, &ctx).unwrap(),
                index::perf(&mut ix, &log, &ctx).unwrap().unwrap(),
            ),
            (
                "timeseries",
                views::timeseries(&log, &ctx).unwrap(),
                index::timeseries(&mut ix, &log, &ctx).unwrap().unwrap(),
            ),
            (
                "analytics",
                analytics::query(&log, &ctx, &pricing).unwrap(),
                index::analytics(&mut ix, &ctx, &pricing),
            ),
            (
                "by_account",
                views::by_account(&log).unwrap(),
                index::by_account(&mut ix),
            ),
        ];
        for (name, mut streamed, mut indexed) in pairs {
            // An open-ended timeseries window ends at the clock of each call.
            if name == "timeseries" {
                for v in [&mut streamed, &mut indexed] {
                    v["since"] = Value::Null;
                    v["until"] = Value::Null;
                }
            }
            assert_eq!(
                pyjson::dumps_u(&streamed),
                pyjson::dumps_u(&indexed),
                "{name} differs for {params}"
            );
        }
    }
    let daily = store.daily_totals_at(now).unwrap();
    let streamed = UsageStore {
        index: None,
        ..t.store()
    }
    .daily_totals_at(now)
    .unwrap();
    assert_eq!(pyjson::dumps_u(&daily), pyjson::dumps_u(&streamed));
}

#[test]
fn index_follows_appends_and_replacement() {
    let t = Temp::new();
    let store = t.store();
    let all = json!({"realm":"all"});
    store
        .append(json!({"at":1,"account":"a","total_tokens":2}))
        .unwrap();
    assert_eq!(
        store.query("/usage", &all, &Value::Null).unwrap()["total_tokens"],
        2
    );
    store
        .append(json!({"at":2,"account":"a","total_tokens":3}))
        .unwrap();
    assert_eq!(
        store.query("/usage", &all, &Value::Null).unwrap()["total_tokens"],
        5
    );
    assert_eq!(store.indexed_rows(), 2);
    fs::write(
        t.0.join("usage.jsonl"),
        format!(
            "{}\n",
            pyjson::dumps_u(&json!({"at":1,"account":"b","total_tokens":9}))
        ),
    )
    .unwrap();
    let totals = store.query("/usage", &all, &Value::Null).unwrap();
    assert_eq!(totals["total_tokens"], 9);
    assert_eq!(store.indexed_rows(), 1);
    let recent = store.query("/usage/recent", &all, &Value::Null).unwrap();
    assert_eq!(recent["rows"][0]["account"], "b");
}
