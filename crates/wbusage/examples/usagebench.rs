//! Times every usage view on a directory and dumps outputs for comparison.
//! usage: usagebench <usage_dir> <out_dir> [repeats]
use serde_json::{json, Value};
use std::time::Instant;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = &args[1];
    let out = std::path::Path::new(&args[2]);
    let repeats: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
    std::fs::create_dir_all(out).unwrap();
    let store = wbusage::UsageStore::new(dir);
    let mut accounts = Vec::new();
    for i in 0..40 {
        accounts.push(json!({"uid":format!("acct-{i:04}"),"nickname":format!("N{i}"),"realm":if i<10 {"cn"} else {"intl"},"domain":"d","credits":{"remain":i}}));
    }
    let meta = json!({"accounts":accounts,"realm":"intl","api_keys":[{"id":"k1","name":"one","enabled":true},{"id":"k2","name":"two","realm":"cn","enabled":false}]});
    let max_at = 1793252291.0_f64;
    let lo = max_at - 20.0 * 86400.0;
    let hi = max_at - 2.0 * 86400.0;
    let custom = |extra: Value| {
        let mut v = json!({"range":"custom","since":lo,"until":hi});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    let cases: Vec<(&str, &str, Value)> = vec![
        ("totals_all", "/usage", json!({"realm":"all"})),
        ("totals_intl_custom", "/usage", custom(json!({}))),
        ("totals_today", "/usage", json!({"range":"today"})),
        ("recent_p1", "/usage/recent", json!({"limit":"100"})),
        (
            "recent_p50_cn",
            "/usage/recent",
            json!({"limit":"100","page":"50","realm":"cn"}),
        ),
        ("perf", "/usage/perf", json!({})),
        (
            "perf_all_20000",
            "/usage/perf",
            json!({"realm":"all","sample":"20000"}),
        ),
        (
            "timeseries_custom",
            "/usage/timeseries",
            custom(json!({"realm":"all"})),
        ),
        ("timeseries_default", "/usage/timeseries", json!({})),
        ("analytics_custom", "/usage/analytics", custom(json!({}))),
        (
            "analytics_week",
            "/usage/analytics",
            json!({"range":"week"}),
        ),
        ("by_account", "/usage/by-account", json!({})),
    ];
    let mut report = Vec::new();
    for (name, path, params) in &cases {
        let mut times = Vec::new();
        let mut last = Value::Null;
        for _ in 0..repeats {
            let t = Instant::now();
            last = store.query(path, params, &meta).unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let mut v = last;
        for k in ["started", "since", "until", "log_file"] {
            if v.get(k).is_some() && !name.contains("custom") {
                v[k] = Value::Null;
            }
        }
        v["started"] = Value::Null;
        std::fs::write(
            out.join(format!("{name}.json")),
            serde_json::to_string_pretty(&v).unwrap(),
        )
        .unwrap();
        println!(
            "{name:22} first {:8.1} ms   best {:8.1} ms",
            times[0],
            times.iter().cloned().fold(f64::MAX, f64::min)
        );
        report.push(json!({"case":name,"first_ms":times[0],"times":times}));
    }
    let t = Instant::now();
    let d = store.daily_totals().unwrap();
    println!(
        "daily_totals           {:8.1} ms",
        t.elapsed().as_secs_f64() * 1000.0
    );
    std::fs::write(
        out.join("daily.json"),
        serde_json::to_string_pretty(&d).unwrap(),
    )
    .unwrap();
    std::fs::write(
        out.join("report.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}
