//! Read-only billing operations. No check-in, reward claim or activity traffic.
use crate::{
    dispatch,
    state::App,
    wire::{self, Reply},
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock, Weak},
    time::{Duration, Instant},
};
use tokio::sync::Mutex as AsyncMutex;
use wbaccounts::{Account, BillingOperation, RequestOptions};
use wbcore::{
    pyval::py_round,
    timeutil,
    types::{InResponse, OutRequest, Realm},
};

const BODY_LIMIT: usize = 2 * 1024 * 1024;
const MAX_PAGES: usize = 10;
const FETCH_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Debug)]
struct Failure {
    status: u16,
    message: &'static str,
    code: Option<i64>,
    skipped: bool,
    retry_after: u64,
}
type Result<T> = std::result::Result<T, Failure>;
impl Failure {
    fn new(status: u16, message: &'static str) -> Self {
        Self {
            status,
            message,
            code: None,
            skipped: false,
            retry_after: 0,
        }
    }
    fn value(&self) -> Value {
        let mut v = json!({"ok":false,"error":self.message,"status":self.status});
        if let Some(code) = self.code {
            v["code"] = json!(code);
        }
        if self.status == 401 {
            v["need_relogin"] = json!(true);
        }
        if self.skipped {
            v["skipped"] = json!(true);
        }
        if self.retry_after > 0 {
            v["retry_after"] = json!(self.retry_after);
        }
        v
    }
    fn terminal(&self) -> bool {
        self.skipped || matches!(self.status, 401 | 403 | 429)
    }
}
fn malformed() -> Failure {
    Failure::new(502, "billing response is missing valid balance data")
}

#[derive(Default)]
struct Flight {
    finished: Option<Instant>,
    result: Option<Value>,
}
// Separate from refresh_lock: billing holds its lock while calling refresh_account.
// Weak entries prevent deleted accounts/app instances from accumulating forever.
type LockKey = (usize, bool, String);
fn flight_lock(app: &App, sweep: bool, uid: &str) -> Arc<AsyncMutex<Flight>> {
    static LOCKS: OnceLock<Mutex<HashMap<LockKey, Weak<AsyncMutex<Flight>>>>> = OnceLock::new();
    let mut locks = LOCKS.get_or_init(Default::default).lock();
    locks.retain(|_, lock| lock.strong_count() > 0);
    let key = (app as *const App as usize, sweep, uid.to_owned());
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(AsyncMutex::new(Flight::default()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}
fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str()?.parse().ok())
        .filter(|n| n.is_finite())
}
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn first_number(v: &Value, keys: &[&str]) -> f64 {
    keys.iter().find_map(|key| number(&v[*key])).unwrap_or(0.0)
}
fn epoch(v: &Value) -> Option<f64> {
    number(v)
        .filter(|n| *n != 0.0)
        .map(|n| if n > 1e11 { n / 1000.0 } else { n })
}
fn flag(v: &Value) -> bool {
    wbcore::pyval::truthy(v)
}

/// Pure conversion matching the Python package contract, including *cycle*
/// capacity precedence, deduction rather than billing-cycle expiry, and rounding.
fn parse_package(raw: &Value, now: f64) -> Value {
    let size = first_number(
        raw,
        &[
            "CycleCapacitySizePrecise",
            "CycleCapacitySize",
            "CapacitySizePrecise",
            "CapacitySize",
        ],
    );
    let mut remain = first_number(
        raw,
        &[
            "CycleCapacityRemainPrecise",
            "CycleCapacityRemain",
            "CapacityRemainPrecise",
            "CapacityRemain",
        ],
    );
    let mut used = first_number(
        raw,
        &[
            "CycleCapacityUsedPrecise",
            "CycleCapacityUsed",
            "CapacityUsedPrecise",
            "CapacityUsed",
        ],
    );
    if size > 0.0 && used <= 0.0 && remain <= size {
        used = (size - remain).max(0.0);
    } else if size > 0.0 && remain <= 0.0 && used < size {
        remain = (size - used).max(0.0);
    }
    let cycle_end = if text(raw, "CycleEndTime").is_empty() {
        text(raw, "ExpiredTime")
    } else {
        text(raw, "CycleEndTime")
    };
    let deduction = epoch(&raw["DeductionEndTime"])
        .map(timeutil::local_stamp)
        .unwrap_or_default();
    let expiry = if deduction.is_empty() {
        cycle_end
    } else {
        &deduction
    };
    let diff = timeutil::parse_local_stamp(expiry).map(|end| end - now);
    let no_expiry = diff.is_some_and(|n| n > 730.0 * 86400.0);
    let days = diff
        .filter(|_| !no_expiry)
        .map(|n| py_round(n / 86400.0, 1));
    let expired = !no_expiry && diff.is_some_and(|n| n < 0.0);
    let reason = raw["AccountAttributes"]
        .as_array()
        .and_then(|xs| xs.iter().find(|x| x["Key"] == "grantReason"))
        .map(|x| text(x, "Value"))
        .unwrap_or("");
    let created = epoch(&raw["CreateTime"])
        .map(timeutil::local_stamp)
        .unwrap_or_else(|| text(raw, "CreateTime").into());
    json!({
        "name":if text(raw,"PackageName").is_empty() { "Package" } else { text(raw,"PackageName") },
        "package_code":text(raw,"PackageCode"), "product_name":text(raw,"ProductName"),
        "sub_product_name":text(raw,"SubProductName"), "grant_reason":reason,
        "resource_id":text(raw,"ResourceId"), "deal_name":text(raw,"DealName"), "create_time":created,
        "remain":py_round(remain,2), "used":py_round(used,2), "size":py_round(size,2),
        "unit":if text(raw,"CapacityUnit").is_empty() { "credits" } else { text(raw,"CapacityUnit") },
        "in_usage":flag(&raw["InUsage"]) || (!expired && remain > 0.0 && used > 0.0),
        "auto_renew":flag(&raw["AutoRenewFlag"]) || flag(&raw["SupportAutoRenew"]),
        "cycle_start_time":text(raw,"CycleStartTime"), "cycle_end_time":cycle_end,
        "expire_time":expiry, "no_expiry":no_expiry, "days_left":days, "is_expired":expired,
        "status":raw.get("Status").cloned().unwrap_or(json!(0))
    })
}

/// A 200 response is not proof of success: check both application and Tencent
/// envelopes before allowing an empty balance to replace a known snapshot.
fn envelope_data(value: &Value, tencent: bool) -> Result<&Value> {
    if !value.is_object() {
        return Err(malformed());
    }
    if let Some(code) = value.get("code") {
        let code = code
            .as_i64()
            .or_else(|| code.as_str()?.parse().ok())
            .ok_or_else(malformed)?;
        if code != 0 {
            let mut e = Failure::new(
                match code {
                    401 => 401,
                    11140 => 403,
                    14003 => 429,
                    _ => 502,
                },
                "billing service rejected the request",
            );
            e.code = Some(code);
            return Err(e);
        }
    } else if !tencent {
        return Err(malformed());
    }
    if value.get("error").is_some_and(|v| !v.is_null())
        || value.get("Error").is_some_and(|v| !v.is_null())
    {
        return Err(malformed());
    }
    let data = if tencent {
        let response = &value["data"]["Response"];
        if !response.is_object() || response.get("Error").is_some_and(|v| !v.is_null()) {
            return Err(malformed());
        }
        &response["Data"]
    } else {
        &value["data"]
    };
    if !data.is_object()
        || data.get("Error").is_some_and(|v| !v.is_null())
        || data.get("error").is_some_and(|v| !v.is_null())
    {
        return Err(malformed());
    }
    Ok(data)
}
fn account_rows(data: &Value) -> Result<Vec<Value>> {
    match data.get("Accounts") {
        Some(Value::Array(xs)) if xs.iter().all(Value::is_object) => Ok(xs.clone()),
        None | Some(Value::Null) if number(&data["TotalCount"]) == Some(0.0) => Ok(Vec::new()),
        _ => Err(malformed()),
    }
}
fn validate_capacities(raw: &Value) -> Result<()> {
    // Missing/invalid numeric capacity fields must not silently become a zero balance.
    for suffix in ["Size", "Remain", "Used"] {
        let keys = [
            format!("CycleCapacity{suffix}Precise"),
            format!("CycleCapacity{suffix}"),
            format!("Capacity{suffix}Precise"),
            format!("Capacity{suffix}"),
        ];
        if suffix == "Size" && !keys.iter().any(|k| number(&raw[k]).is_some()) {
            return Err(malformed());
        }
        if keys
            .iter()
            .any(|k| raw.get(k).is_some_and(|v| !v.is_null() && v != ""))
            && !keys.iter().any(|k| number(&raw[k]).is_some())
        {
            return Err(malformed());
        }
    }
    let has_balance = [
        "CycleCapacityRemainPrecise",
        "CycleCapacityRemain",
        "CapacityRemainPrecise",
        "CapacityRemain",
        "CycleCapacityUsedPrecise",
        "CycleCapacityUsed",
        "CapacityUsedPrecise",
        "CapacityUsed",
    ]
    .iter()
    .any(|k| number(&raw[*k]).is_some());
    if !has_balance {
        return Err(malformed());
    }
    Ok(())
}
fn summary_totals(rows: &[Value]) -> Result<(f64, f64, f64)> {
    let (mut remain, mut used, mut size) = (0.0, 0.0, 0.0);
    for row in rows {
        size += number(&row["CycleTotalCapacity"]).ok_or_else(malformed)?;
        remain += number(&row["CycleRemainCapacity"]).ok_or_else(malformed)?;
        used += number(&row["CycleUsedCapacity"]).ok_or_else(malformed)?;
    }
    if ![remain, used, size].iter().all(|n| n.is_finite()) {
        return Err(malformed());
    }
    Ok((remain, used, size))
}
fn credits_snapshot(
    mut packages: Vec<Value>,
    totals: Option<(f64, f64, f64)>,
    paid: Option<bool>,
    checkin: Value,
    now: f64,
) -> Result<Value> {
    let days = |p: &Value| {
        let d = number(&p["days_left"]).unwrap_or(99999.0);
        if d < 0.0 {
            99999.0 + d.abs()
        } else {
            d
        }
    };
    packages.sort_by(|a, b| {
        (!flag(&a["in_usage"]))
            .cmp(&(!flag(&b["in_usage"])))
            .then(flag(&a["is_expired"]).cmp(&flag(&b["is_expired"])))
            .then_with(|| days(a).total_cmp(&days(b)))
            .then_with(|| {
                number(&b["remain"])
                    .unwrap_or(0.0)
                    .total_cmp(&number(&a["remain"]).unwrap_or(0.0))
            })
    });
    let (remain, used, size) = totals.unwrap_or_else(|| {
        packages.iter().fold((0.0, 0.0, 0.0), |(r, u, s), p| {
            (
                r + number(&p["remain"]).unwrap_or(0.0),
                u + number(&p["used"]).unwrap_or(0.0),
                s + number(&p["size"]).unwrap_or(0.0),
            )
        })
    });
    if ![remain, used, size].iter().all(|n| n.is_finite()) {
        return Err(malformed());
    }
    let (remain, used, size) = (py_round(remain, 2), py_round(used, 2), py_round(size, 2));
    let earliest = packages.iter().filter(|p| number(&p["remain"]).is_some_and(|n| n > 0.0) && !flag(&p["is_expired"]) && !flag(&p["no_expiry"]) && number(&p["days_left"]).is_some())
        .min_by(|a,b| days(a).total_cmp(&days(b)))
        .map(|p| json!({"name":p["name"],"package_code":p["package_code"],"remain":p["remain"],"cycle_end_time":p["cycle_end_time"],"days_left":p["days_left"]}));
    Ok(json!({"remain":remain,"used":used,"size":size,
        "used_percent":if size > 0.0 { format!("{:.1}%",used/size*100.0) } else { "0.0%".into() },
        "remain_percent":if size > 0.0 { format!("{:.1}%",remain/size*100.0) } else { "100.0%".into() },
        "is_paid_user":paid,"checkin":checkin,"earliest_expiring":earliest,"packages":packages,
        "updated_at":now,"updated_iso":timeutil::local_stamp(now)}))
}
fn parse_enterprise(data: &Value, now: f64) -> Result<Value> {
    let used = py_round(
        number(&data["credit"])
            .filter(|n| *n >= 0.0)
            .ok_or_else(malformed)?,
        2,
    );
    let size = py_round(
        number(&data["limitNum"])
            .filter(|n| *n >= 0.0)
            .ok_or_else(malformed)?,
        2,
    );
    let remain = if size > 0.0 {
        py_round((size - used).max(0.0), 2)
    } else {
        0.0
    };
    let end = if text(data, "cycleEndTime").is_empty() {
        text(data, "cycleResetTime")
    } else {
        text(data, "cycleEndTime")
    };
    let package = json!({"name":"企业额度","package_code":"enterprise","product_name":"","sub_product_name":"",
        "grant_reason":"企业空间发放","resource_id":"","deal_name":"","create_time":text(data,"cycleStartTime"),
        "remain":remain,"used":used,"size":size,"unit":"credits","in_usage":used>0.0,"auto_renew":true,
        "cycle_start_time":text(data,"cycleStartTime"),"cycle_end_time":end,"expire_time":"",
        "no_expiry":true,"days_left":null,"is_expired":false,"status":0});
    let mut credits = credits_snapshot(
        if size > 0.0 { vec![package] } else { vec![] },
        Some((remain, used, size)),
        Some(true),
        Value::Null,
        now,
    )?;
    credits["is_enterprise"] = json!(true);
    Ok(credits)
}
fn parse_tier(data: &Value, now: f64) -> Option<Value> {
    let paid = data.get("IsPaidUser")?.as_bool()?;
    Some(
        json!({"paid":paid,"package":text(data,"SubscriptionPackageName"),"package_code":text(data,"SubscriptionPackageCode"),
        "protected_price":flag(&data["IsProtectedPriceUser"]),"trial":number(&data["ProTrialStatus"]).unwrap_or(0.0) as i64,
        "checked_at":now,"source":"billing"}),
    )
}

fn restriction(app: &App, account: &Account) -> Result<()> {
    let now = timeutil::now();
    let until = number(&account.public_metadata()["restrictedUntil"]).unwrap_or(0.0);
    let wait = (until - now)
        .max(app.restriction_wait(account.realm()) as f64)
        .max(app.pool.restriction_hold_remaining(account.realm(), now));
    if wait > 0.0 {
        let mut e = Failure::new(
            403,
            "billing refresh skipped while the account or realm is restricted",
        );
        e.skipped = true;
        e.retry_after = wait.ceil() as u64;
        return Err(e);
    }
    Ok(())
}
fn options(app: &App, account: &Account, seconds: f64) -> RequestOptions {
    RequestOptions {
        proxy: account.resolve_proxy_in(&app.settings.proxy_slots_shared()),
        cn_chat_base: app.env.cn_chat_upstream.clone(),
        timeout_secs: seconds,
    }
}
async fn execute(app: &App, mut request: OutRequest) -> Result<InResponse> {
    let timeout = Duration::from_secs_f64(request.timeout_secs);
    app.rewrite_test_upstream(&mut request);
    // app.http verifies certificates/hostnames, does not follow redirects, and
    // collect enforces decoded-body limits. This outer deadline also bounds drip feeds.
    tokio::time::timeout(timeout, async {
        app.http
            .execute(request)
            .await
            .map_err(|_| Failure::new(502, "billing request failed"))?
            .into_in_response(BODY_LIMIT)
            .await
            .map_err(|_| {
                Failure::new(
                    502,
                    "billing response exceeded its limit or could not be read",
                )
            })
    })
    .await
    .map_err(|_| Failure::new(504, "billing request timed out"))?
}
struct Fetch<'a> {
    app: &'a App,
    account: &'a Arc<Account>,
    refreshed: bool,
}
impl Fetch<'_> {
    async fn data(&mut self, operation: BillingOperation, body: &Value) -> Result<Value> {
        loop {
            restriction(self.app, self.account)?;
            let cred = self.account.cred();
            let seconds = match operation {
                BillingOperation::Resource => 30.0,
                BillingOperation::CheckinStatus => 10.0,
                _ => 15.0,
            };
            let request = wbaccounts::build_billing_request(
                &cred,
                operation,
                body,
                &options(self.app, self.account, seconds),
            )
            .map_err(|_| Failure::new(400, "cannot construct billing request for this account"))?;
            let response = execute(self.app, request).await?;
            let value = serde_json::from_slice::<Value>(&response.body).ok();
            let code = value.as_ref().and_then(wbaccounts::json_code);
            if code == Some(11140) || wbaccounts::restriction_is_account_level(&response.text()) {
                // Never retain the upstream body (it can contain credentials).
                let saved = self.account.note_restricted(
                    "billing request rejected by account restriction",
                    timeutil::now(),
                );
                self.app
                    .restricted(self.account.realm(), self.account.uid());
                self.app.pool.note_account_restriction(
                    self.account.realm(),
                    self.account.uid(),
                    timeutil::now(),
                );
                if saved.is_err() {
                    return Err(Failure::new(
                        500,
                        "account restriction could not be persisted",
                    ));
                }
                let mut e = Failure::new(403, "billing request rejected by account restriction");
                e.code = Some(11140);
                return Err(e);
            }
            if response.status == 401 || code == Some(401) {
                if !self.refreshed && !cred.is_api_key() && !cred.refresh_token.is_empty() {
                    self.refreshed = true;
                    restriction(self.app, self.account)?;
                    if dispatch::refresh_account(self.app, self.account, true)
                        .await
                        .is_ok()
                    {
                        continue;
                    }
                }
                return Err(Failure::new(
                    401,
                    "billing authentication failed; sign in again or replace the static key",
                ));
            }
            if !(200..300).contains(&response.status) {
                return Err(Failure::new(
                    match response.status {
                        403 => 403,
                        429 => 429,
                        _ => 502,
                    },
                    "billing service returned an unsuccessful HTTP status",
                ));
            }
            let value = value.ok_or_else(malformed)?;
            return envelope_data(&value, matches!(operation, BillingOperation::Resource)).cloned();
        }
    }
    async fn pages(
        &mut self,
        operation: BillingOperation,
        mut body: Value,
        page_size: usize,
    ) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        for page in 1..=MAX_PAGES {
            body["PageNumber"] = json!(page);
            let data = self.data(operation, &body).await?;
            let batch = account_rows(&data)?;
            let count = batch.len();
            if count > page_size {
                return Err(malformed());
            }
            let total = match data.get("TotalCount") {
                None | Some(Value::Null) => None,
                Some(v) => Some(
                    number(v)
                        .filter(|n| *n >= 0.0 && n.fract() == 0.0)
                        .ok_or_else(malformed)? as usize,
                ),
            };
            rows.extend(batch);
            if total.is_some_and(|n| n < rows.len()) {
                return Err(malformed());
            }
            if total == Some(rows.len()) || (total.is_none() && count < page_size) {
                return Ok(rows);
            }
            if count == 0 {
                return Err(malformed());
            }
        }
        Err(Failure::new(
            502,
            "billing package pagination exceeded its safe limit; previous balance was retained",
        ))
    }
}
struct Snapshot {
    credits: Value,
    tier: Option<Value>,
    warnings: Vec<&'static str>,
}
fn previous_paid(account: &Account) -> Option<bool> {
    let view = account.public_metadata();
    view["tier"]["paid"]
        .as_bool()
        .or_else(|| view["credits"]["is_paid_user"].as_bool())
}
async fn detailed(fetch: &mut Fetch<'_>) -> Result<Snapshot> {
    let summary = fetch.data(BillingOperation::Summary, &json!({})).await?;
    let summary_rows = summary["Packages"]
        .as_array()
        .filter(|xs| xs.iter().all(Value::is_object))
        .ok_or_else(malformed)?;
    let tier = parse_tier(&summary, timeutil::now());
    let codes: Vec<_> = summary_rows
        .iter()
        .filter_map(|p| p["PackageCode"].as_str().filter(|s| !s.is_empty()))
        .collect();
    let mut packages = Vec::new();
    let mut seen = HashSet::new();
    if !codes.is_empty() {
        let free = fetch
            .pages(
                BillingOperation::FreePackages,
                json!({"PackageCodes":codes,"PageNumber":1,"PageSize":200,"Status":[0]}),
                200,
            )
            .await?;
        let paid = fetch.pages(BillingOperation::PaidPackages,json!({"PackageCodes":codes,"PageNumber":1,"PageSize":200,"Status":[0,3],"NeedRenewInfo":true}),200).await?;
        for raw in free.iter().chain(paid.iter()) {
            if let Some(id) = raw.get("AccountId").filter(|id| flag(id)) {
                if !seen.insert(id.to_string()) {
                    continue;
                }
            }
            validate_capacities(raw)?;
            packages.push(parse_package(raw, timeutil::now()));
        }
    }
    let totals = if packages.is_empty() {
        Some(summary_totals(summary_rows)?)
    } else {
        None
    };
    let mut warnings = Vec::new();
    let checkin = match fetch
        .data(BillingOperation::CheckinStatus, &json!({}))
        .await
    {
        Ok(data) if data.get("today_checked_in").is_some() => {
            json!({"today_checked_in":flag(&data["today_checked_in"]),
            "streak_days":number(&data["streak_days"]).unwrap_or(0.0),"daily_credit":number(&data["daily_credit"]).unwrap_or(0.0),
            "today_credit":number(&data["today_credit"]).unwrap_or(0.0),"active":data["active"].as_bool().unwrap_or(true)})
        }
        Err(e) if e.terminal() => return Err(e),
        _ => {
            warnings.push("check-in status was unavailable; no check-in was performed");
            fetch.account.public_metadata()["credits"]["checkin"].clone()
        }
    };
    let paid = tier
        .as_ref()
        .and_then(|t| t["paid"].as_bool())
        .or_else(|| previous_paid(fetch.account));
    Ok(Snapshot {
        credits: credits_snapshot(packages, totals, paid, checkin, timeutil::now())?,
        tier,
        warnings,
    })
}
async fn resource(fetch: &mut Fetch<'_>) -> Result<Snapshot> {
    let rows = fetch.pages(BillingOperation::Resource,json!({"PageNumber":1,"PageSize":100,"ProductCode":"p_tcaca","Status":[0,3],
        "PackageEndTimeRangeBegin":timeutil::local_stamp(timeutil::now()),"PackageEndTimeRangeEnd":"2036-01-01 00:00:00"}),100).await?;
    let mut packages = Vec::new();
    for row in rows {
        validate_capacities(&row)?;
        packages.push(parse_package(&row, timeutil::now()));
    }
    let view = fetch.account.public_metadata();
    let mut tier = None;
    let mut warnings = Vec::new();
    if view["tier"]["paid"].as_bool().is_none()
        || number(&view["tier"]["checked_at"])
            .is_none_or(|n| n <= 0.0 || timeutil::now() - n >= 86400.0)
    {
        match fetch.data(BillingOperation::Summary, &json!({})).await {
            Ok(data) => {
                tier = parse_tier(&data, timeutil::now());
                if tier.is_none() {
                    warnings.push(
                        "billing summary did not provide a tier; the previous tier was retained",
                    );
                }
            }
            Err(e) if e.terminal() => return Err(e),
            Err(_) => warnings
                .push("billing tier summary was unavailable; the previous tier was retained"),
        }
    }
    let paid = tier
        .as_ref()
        .and_then(|t| t["paid"].as_bool())
        .or_else(|| previous_paid(fetch.account));
    Ok(Snapshot {
        credits: credits_snapshot(
            packages,
            None,
            paid,
            view["credits"]["checkin"].clone(),
            timeutil::now(),
        )?,
        tier,
        warnings,
    })
}
async fn fetch_snapshot(app: &App, account: &Arc<Account>) -> Result<Snapshot> {
    restriction(app, account)?;
    let mut fetch = Fetch {
        app,
        account,
        refreshed: false,
    };
    let cred = account.cred();
    // Enterprise quotas have their own documented endpoint. Do not query personal
    // packages or reward endpoints for an enterprise identity/static API key.
    if cred.is_api_key() || !cred.enterprise_id.is_empty() {
        let data = fetch
            .data(BillingOperation::EnterpriseUsage, &json!({}))
            .await?;
        return Ok(Snapshot {
            credits: parse_enterprise(&data, timeutil::now())?,
            tier: None,
            warnings: vec![],
        });
    }
    if account.realm() == Realm::Cn {
        match detailed(&mut fetch).await {
            Ok(snapshot) => return Ok(snapshot),
            Err(e) if e.terminal() => return Err(e),
            Err(_) => {
                let mut snapshot = resource(&mut fetch).await?;
                snapshot.warnings.push("CN detailed billing was unavailable; the documented resource fallback was used");
                return Ok(snapshot);
            }
        }
    }
    resource(&mut fetch).await
}
async fn refresh(app: &App, account: &Arc<Account>) -> Value {
    let started = Instant::now();
    let lock = flight_lock(app, false, account.uid());
    let mut flight = match tokio::time::timeout(FETCH_TIMEOUT, lock.lock()).await {
        Ok(guard) => guard,
        Err(_) => {
            return Failure::new(503, "a billing refresh is already running for this account")
                .value();
        }
    };
    if flight.finished.is_some_and(|at| at >= started) {
        if let Some(result) = &flight.result {
            return result.clone();
        }
    }
    let result = async {
        restriction(app, account)?;
        if !account.enabled() {
            let mut e = Failure::new(409, "billing refresh skipped for a disabled account");
            e.skipped = true;
            return Err(e);
        }
        let snapshot = fetch_snapshot(app, account).await?;
        // No mutation of restriction/cap state, and no stale full-account upsert.
        account
            .set_credits(snapshot.credits)
            .map_err(|_| Failure::new(500, "billing credits could not be persisted"))?;
        let mut value = json!({"ok":true,"credits":account.public_metadata()["credits"]});
        if let Some(tier) = snapshot.tier {
            account.set_tier(tier).map_err(|_| {
                Failure::new(
                    500,
                    "billing balance was saved but account tier could not be persisted",
                )
            })?;
            value["tier"] = account.public_metadata()["tier"].clone();
        }
        if !snapshot.warnings.is_empty() {
            value["warnings"] = json!(snapshot.warnings);
        }
        Ok(value)
    };
    let value = match tokio::time::timeout(FETCH_TIMEOUT, result).await {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => error.value(),
        Err(_) => Failure::new(
            504,
            "billing refresh timed out; no partial balance was saved",
        )
        .value(),
    };
    flight.finished = Some(Instant::now());
    flight.result = Some(value.clone());
    value
}
fn views(app: &App) -> Vec<Value> {
    app.pool
        .accounts()
        .iter()
        .map(|a| a.public_metadata())
        .collect()
}
fn fail(status: u16, message: &'static str) -> Reply {
    wire::json_reply(status, Failure::new(status, message).value())
}
fn requested_realm(payload: &Value) -> Result<Option<Realm>> {
    match payload.get("realm") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() || s == "all" => Ok(None),
        Some(Value::String(s)) => Realm::parse(s)
            .map(Some)
            .ok_or_else(|| Failure::new(400, "realm must be intl, cn or all")),
        _ => Err(Failure::new(400, "realm must be intl, cn or all")),
    }
}
async fn sweep(app: &App, payload: &Value) -> Reply {
    let realm = match requested_realm(payload) {
        Ok(r) => r,
        Err(e) => return wire::json_reply(e.status, e.value()),
    };
    let targets = if !text(payload, "uid").is_empty() {
        match app.pool.get(text(payload, "uid")) {
            Some(a) if realm.is_none_or(|r| r == a.realm()) => vec![a],
            Some(_) => return fail(400, "account does not belong to the requested realm"),
            None => return fail(404, "no such account"),
        }
    } else {
        app.pool
            .accounts()
            .into_iter()
            .filter(|a| realm.is_none_or(|r| r == a.realm()))
            .collect::<Vec<_>>()
    };
    let sweep_lock = flight_lock(app, true, "");
    let _guard = if targets.len() > 1 {
        match sweep_lock.try_lock() {
            Ok(guard) => Some(guard),
            Err(_) => {
                return wire::json_reply(
                    200,
                    json!({"results":[],"accounts":views(app),"busy":true}),
                );
            }
        }
    } else {
        None
    };
    let mut results = Vec::new();
    for (index, account) in targets.iter().enumerate() {
        // One active account at a time, with a fixed safe gap. No burst fan-out.
        if index > 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let mut result = refresh(app, account).await;
        result["uid"] = json!(account.uid());
        results.push(result);
    }
    wire::json_reply(
        200,
        json!({"results":results,"accounts":views(app),"busy":false}),
    )
}
async fn detail(app: &App, payload: &Value) -> Reply {
    let uid = text(payload, "uid");
    let Some(account) = app.pool.get(uid) else {
        return fail(404, "no such account");
    };
    let realm = match requested_realm(payload) {
        Ok(r) => r,
        Err(e) => return wire::json_reply(e.status, e.value()),
    };
    if realm.is_some_and(|r| r != account.realm()) {
        return fail(400, "account does not belong to the requested realm");
    }
    if payload.get("refresh").is_some_and(|v| !v.is_boolean()) {
        return fail(400, "refresh must be a boolean");
    }
    let cached = account.public_metadata()["credits"].clone();
    let needs = payload["refresh"].as_bool().unwrap_or(true)
        || !cached["packages"].as_array().is_some_and(|p| !p.is_empty());
    let mut result = if needs {
        refresh(app, &account).await
    } else {
        json!({"ok":true})
    };
    let view = account.public_metadata();
    result["uid"] = json!(account.uid());
    result["nickname"] = view["nickname"].clone();
    result["realm"] = json!(account.realm().as_str());
    result["credits"] = view["credits"].clone();
    wire::json_reply(200, result)
}
fn valid_proxy(proxy: &str) -> bool {
    proxy.is_empty()
        || url::Url::parse(proxy).is_ok_and(|u| {
            matches!(u.scheme(), "http" | "https" | "socks5" | "socks5h")
                && u.host_str().is_some()
                && matches!(u.path(), "" | "/")
                && u.query().is_none()
                && u.fragment().is_none()
        })
}
async fn add_api_key(app: &App, payload: &Value) -> Reply {
    let key = if text(payload, "key").trim().is_empty() {
        text(payload, "accessToken").trim()
    } else {
        text(payload, "key").trim()
    };
    if !wbcore::types::is_static_api_key(key)
        || key.len() <= 3
        || key.len() > 8192
        || !key.bytes().all(|b| b.is_ascii_graphic())
    {
        return fail(
            400,
            "a valid static API key with the ck_ or pt_ prefix is required",
        );
    }
    match requested_realm(payload) {
        Ok(None | Some(Realm::Cn)) => {}
        _ => return fail(400, "static API keys require the cn realm"),
    }
    for field in ["uid", "nickname", "proxySlot", "proxy"] {
        if payload.get(field).is_some_and(|v| !v.is_string()) {
            return fail(400, "account metadata fields must be strings");
        }
    }
    let uid = if text(payload, "uid").trim().is_empty() {
        format!("apikey-{}", wbaccounts::api_key_id(key))
    } else {
        text(payload, "uid").trim().into()
    };
    if uid == "apikey-"
        || uid.len() > 128
        || !uid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return fail(400, "cannot determine a safe account uid");
    }
    let lock = flight_lock(app, false, &uid);
    let _guard = match lock.try_lock() {
        Ok(g) => g,
        Err(_) => return fail(409, "an account billing operation is already running"),
    };
    let existing = app.pool.get(&uid);
    if let Some(account) = &existing {
        if account.realm() != Realm::Cn || !account.cred().is_api_key() {
            return fail(409, "uid belongs to a different account type");
        }
        if let Err(e) = restriction(app, account) {
            return wire::json_reply(e.status, e.value());
        }
    }
    let mut doc = existing.as_ref().map(|a| a.to_json()).unwrap_or_else(|| json!({"uid":uid,"realm":"cn","platform":"CLI","source":"api-key","enabled":true,"tasksEnabled":false}));
    doc["accessToken"] = json!(key);
    doc["refreshToken"] = json!("");
    doc["expiresAt"] = json!(0);
    for field in ["nickname", "proxySlot", "proxy"] {
        if let Some(v) = payload.get(field) {
            doc[field] = v.clone();
        }
    }
    if !valid_proxy(text(&doc, "proxy")) {
        return fail(400, "proxy must be an HTTP, HTTPS or SOCKS5 URL");
    }
    if !text(&doc, "proxySlot").is_empty()
        && app
            .settings
            .find_proxy_slot(text(&doc, "proxySlot"))
            .is_none()
    {
        return fail(400, "no such proxy slot");
    }
    let account = match Account::from_json(&doc) {
        Ok(a) => Arc::new(a),
        Err(_) => return fail(400, "cannot construct static API key account"),
    };
    if let Err(e) = restriction(app, &account) {
        return wire::json_reply(e.status, e.value());
    }
    let mut request = match wbaccounts::build_billing_request(
        &account.cred(),
        BillingOperation::EnterpriseUsage,
        &json!({}),
        &options(app, &account, 15.0),
    ) {
        Ok(r) => r,
        Err(_) => return fail(400, "cannot construct static API key probe"),
    };
    // Match the dedicated documented probe, not the personal billing identity.
    request.headers.retain(|(k, _)| {
        [
            "authorization",
            "x-api-key",
            "content-type",
            "accept",
            "user-agent",
        ]
        .contains(&k.to_ascii_lowercase().as_str())
    });
    for (name, value) in &mut request.headers {
        if name.eq_ignore_ascii_case("accept") {
            *value = "application/json".into();
        } else if name.eq_ignore_ascii_case("user-agent") {
            *value = "WorkBuddy/5.8.3 WorkBuddy/5.8.3 CLI/2.161.0".into();
        }
    }
    let probed = async {
        let response = execute(app,request).await?;
        if !(200..300).contains(&response.status) { return Err(Failure::new(400,"static key balance probe failed; the key was not saved")); }
        let value: Value = serde_json::from_slice(&response.body).map_err(|_| malformed())?;
        let data = envelope_data(&value,false)?;
        let credits = parse_enterprise(data,timeutil::now())?;
        let probe = json!({"ok":true,"used":credits["used"],"limit":credits["size"],"cycle_end":data["cycleEndTime"]});
        Ok((credits,probe))
    }.await;
    let (credits, probe) = match probed {
        Ok(v) => v,
        Err(_) => {
            return fail(
                400,
                "static key balance probe failed or returned invalid quota data; the key was not saved",
            );
        }
    };
    // The probe's snapshot is enough: do not issue an unnecessary second request.
    if account.set_credits(credits).is_err() {
        return fail(500, "could not prepare static key balance");
    }
    let saved = match app.pool.upsert(account, Some(&app.args.accounts_dir)) {
        Ok(a) => a,
        Err(_) => return fail(500, "static API key account could not be persisted"),
    };
    let view = saved.public_metadata();
    wire::json_reply(
        200,
        json!({"ok":true,"uid":saved.uid(),"nickname":view["nickname"],"updated":existing.is_some(),"probe":probe,"account":view,"accounts":views(app)}),
    )
}

/// Caller must enforce POST and panel authorization before dispatching here.
pub async fn handle(app: Arc<App>, path: &str, payload: Value) -> Option<Reply> {
    if !matches!(
        path,
        "/accounts/credits"
            | "/accounts/credits/fetch"
            | "/accounts/credits/detail"
            | "/accounts/add-api-key"
    ) {
        return None;
    }
    if !payload.is_object() {
        return Some(fail(400, "billing payload must be an object"));
    }
    if payload.get("uid").is_some_and(|v| !v.is_string()) {
        return Some(fail(400, "uid must be a string"));
    }
    Some(match path {
        "/accounts/credits" | "/accounts/credits/fetch" => sweep(&app, &payload).await,
        "/accounts/credits/detail" => detail(&app, &payload).await,
        "/accounts/add-api-key" => add_api_key(&app, &payload).await,
        _ => unreachable!(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn precise_cycle_capacities_and_inference() {
        let p = parse_package(
            &json!({"CycleCapacitySizePrecise":"100.125","CycleCapacitySize":999,"CapacitySizePrecise":500,
            "CycleCapacityRemainPrecise":"75.115","CycleCapacityUsedPrecise":"25.01"}),
            0.0,
        );
        assert_eq!(p["size"], json!(py_round(100.125, 2)));
        assert_eq!(p["remain"], json!(py_round(75.115, 2)));
        assert_eq!(p["used"], json!(25.01));
        assert_eq!(p["in_usage"], true);
        assert_eq!(
            parse_package(&json!({"CapacitySize":10,"CapacityRemain":6}), 0.0)["used"],
            4.0
        );
        assert_eq!(
            parse_package(&json!({"CapacitySize":10,"CapacityUsed":3}), 0.0)["remain"],
            7.0
        );
        assert_eq!(
            parse_package(
                &json!({"CycleCapacitySizePrecise":"NaN","CycleCapacitySize":8,"CapacityRemain":2}),
                0.0
            )["size"],
            8.0
        );
    }
    #[test]
    fn deduction_deadline_wins_over_cycle_and_far_future_is_not_expiring() {
        let now = timeutil::parse_local_stamp("2026-10-27 12:00:00").unwrap();
        let deduction = timeutil::parse_local_stamp("2034-10-27 12:00:00").unwrap();
        let p = parse_package(
            &json!({"CycleEndTime":"2026-10-28 12:00:00","DeductionEndTime":deduction*1000.0,"CapacitySize":10,"CapacityRemain":10}),
            now,
        );
        assert_eq!(p["no_expiry"], true);
        assert!(p["days_left"].is_null());
        assert_eq!(p["is_expired"], false);
        assert_eq!(p["expire_time"], "2034-10-27 12:00:00");
        let p = parse_package(
            &json!({"CycleEndTime":"2026-10-26T12:00:00","DeductionEndTime":"invalid"}),
            now,
        );
        assert_eq!(p["days_left"], -1.0);
        assert_eq!(p["is_expired"], true);
    }
    #[test]
    fn sentinel_boundary_and_create_time() {
        let now = timeutil::parse_local_stamp("2026-10-27 12:00:00").unwrap();
        let p = parse_package(
            &json!({"DeductionEndTime":(now+730.0*86400.0)*1000.0,"CreateTime":now*1000.0}),
            now,
        );
        assert_eq!(p["no_expiry"], false);
        assert_eq!(p["days_left"], 730.0);
        assert_eq!(p["create_time"], "2026-10-27 12:00:00");
        let p = parse_package(
            &json!({"DeductionEndTime":(now+730.0*86400.0+1.0)*1000.0}),
            now,
        );
        assert_eq!(p["no_expiry"], true);
    }
    #[test]
    fn tencent_envelopes_require_accounts_and_reject_errors() {
        let v = json!({"code":0,"data":{"Response":{"Data":{"Accounts":[],"TotalCount":0}}}});
        assert!(account_rows(envelope_data(&v, true).unwrap())
            .unwrap()
            .is_empty());
        let v = json!({"data":{"Response":{"Data":{"Accounts":[{"CapacitySize":1,"CapacityRemain":1}]}}}});
        assert_eq!(
            account_rows(envelope_data(&v, true).unwrap())
                .unwrap()
                .len(),
            1
        );
        for v in [
            json!({}),
            json!({"code":0}),
            json!({"code":0,"data":{"Response":{"Error":{"Code":"Denied"}}}}),
            json!({"code":401,"data":{"Response":{"Data":{"Accounts":[]}}}}),
        ] {
            assert!(envelope_data(&v, true).is_err());
        }
        assert!(account_rows(&json!({})).is_err());
        assert!(envelope_data(&json!({"code":11140,"data":{}}), false)
            .unwrap_err()
            .terminal());
    }
    #[test]
    fn enterprise_renewal_is_not_expiry_or_negative_balance() {
        let credits = parse_enterprise(
            &json!({"credit":"12.75","limitNum":"10","cycleEndTime":"2020-01-01 00:00:00"}),
            0.0,
        )
        .unwrap();
        assert_eq!(credits["remain"], 0.0);
        assert_eq!(credits["used"], 12.75);
        assert_eq!(credits["packages"][0]["no_expiry"], true);
        assert!(credits["earliest_expiring"].is_null());
        assert!(parse_enterprise(&json!({}), 0.0).is_err());
        assert!(parse_enterprise(&json!({"credit":"NaN","limitNum":10}), 0.0).is_err());
    }
    #[test]
    fn summary_fallback_and_unknown_tier() {
        assert_eq!(summary_totals(&[json!({"CycleTotalCapacity":"10.25","CycleRemainCapacity":6,"CycleUsedCapacity":4.25})]).unwrap(),(6.0,4.25,10.25));
        assert!(summary_totals(&[json!({"PackageCode":"x"})]).is_err());
        assert!(parse_tier(&json!({}), 0.0).is_none());
        assert_eq!(
            parse_tier(&json!({"IsPaidUser":false}), 0.0).unwrap()["paid"],
            false
        );
        assert!(validate_capacities(&json!({"CapacitySize":100})).is_err());
    }
}
