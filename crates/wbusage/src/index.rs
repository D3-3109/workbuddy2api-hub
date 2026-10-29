//! In-memory index of `usage.jsonl` for the aggregate views.
//!
//! Every complete line is parsed once into a fixed-size [`Rec`] holding exactly
//! the fields the views read; later queries only parse appended lines. The views
//! fold these records in file order with [`Num`], which reproduces `bump`'s
//! int/float behaviour step by step, and build their maps in first-insertion
//! order. Outputs are therefore identical to the streaming implementations,
//! including float summation order and JSON key order. Rows a view returns
//! verbatim (recent rows, perf samples, credit events) are re-read by offset.
use crate::pricing::{CostOf, Pricing};
use crate::{analytics, number, outcome, scan, text, truth, views, Context};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::hash::Hash;
use std::io;
use std::path::Path;
use std::sync::Arc;

const ABSENT: u32 = u32::MAX;
const VALID: u8 = 1;
const ERROR: u8 = 2;
const NONEMPTY: u8 = 4;
const TTFT: u8 = 8;
const SPEED: u8 = 16;
const ELAPSED: u8 = 32;
/// `prompt, completion, reasoning, cached, total` (the `TOKENS` order), then credit.
const NUMS: [&str; 6] = [
    "prompt_tokens",
    "completion_tokens",
    "reasoning_tokens",
    "cached_tokens",
    "total_tokens",
    "credit",
];
const CREDIT: usize = 5;

#[derive(Clone, Copy, PartialEq)]
enum Outcome {
    Completed,
    Aborted,
    Other,
}

/// One nonblank complete line. Strings are interned; `""` is id 0.
struct Rec {
    offset: u64,
    at: f64,
    nums: [f64; 6],
    ttft: f64,
    speed: f64,
    elapsed: f64,
    model: u32,
    account: u32,
    realm: u32,
    /// `ABSENT` when the row has no `key` field at all.
    key: u32,
    /// `ABSENT` unless `cost_policy` is a nonempty string.
    policy: u32,
    outcome: Outcome,
    /// Bit i: field `NUMS[i]` exists in the row (even as null).
    present: u8,
    flags: u8,
}

#[derive(Default)]
struct Names {
    ids: HashMap<String, u32>,
    names: Vec<String>,
}
impl Names {
    fn new() -> Self {
        let mut n = Self::default();
        n.id("");
        n
    }
    fn id(&mut self, s: &str) -> u32 {
        if let Some(id) = self.ids.get(s) {
            return *id;
        }
        let id = self.names.len() as u32;
        self.ids.insert(s.to_owned(), id);
        self.names.push(s.to_owned());
        id
    }
    fn name(&self, id: u32) -> &str {
        &self.names[id as usize]
    }
}

pub(crate) struct Index {
    cursor: scan::Cursor,
    recs: Vec<Rec>,
    names: Names,
    costs: Option<(Arc<Pricing>, Vec<CostOf>)>,
}
impl Default for Index {
    fn default() -> Self {
        Self {
            cursor: scan::Cursor::default(),
            recs: Vec::new(),
            names: Names::new(),
            costs: None,
        }
    }
}

impl Index {
    /// Fold lines appended since the last call; rebuild if the log was replaced.
    pub fn refresh(&mut self, path: &Path) -> io::Result<()> {
        if self.cursor.unchanged(path) {
            return Ok(());
        }
        if !self.cursor.matches(path)? {
            *self = Self::default();
        }
        let mut recs = Vec::new();
        let names = &mut self.names;
        let end = scan::scan_range(path, self.cursor.offset, None, |offset, row| {
            recs.push(match row {
                Some(row) => record(names, offset, &row),
                None => Rec {
                    offset,
                    at: 0.0,
                    nums: [0.0; 6],
                    ttft: 0.0,
                    speed: 0.0,
                    elapsed: 0.0,
                    model: 0,
                    account: 0,
                    realm: 0,
                    key: ABSENT,
                    policy: ABSENT,
                    outcome: Outcome::Other,
                    present: 0,
                    flags: 0,
                },
            })
        })?;
        // Capture before publishing, so a failed capture cannot double-count.
        let cursor = scan::Cursor::capture(path, end)?;
        self.recs.append(&mut recs);
        self.cursor = cursor;
        Ok(())
    }

    pub fn rows(&self) -> usize {
        self.recs.len()
    }
    pub fn cursor(&self) -> scan::Cursor {
        self.cursor.clone()
    }

    /// Per-record cost outcomes for this pricing, extended incrementally.
    fn costs(&mut self, pricing: &Arc<Pricing>) -> &[CostOf] {
        if !self
            .costs
            .as_ref()
            .is_some_and(|(p, _)| Arc::ptr_eq(p, pricing))
        {
            self.costs = Some((pricing.clone(), Vec::with_capacity(self.recs.len())));
        }
        let (_, costs) = self.costs.as_mut().unwrap();
        for rec in &self.recs[costs.len()..] {
            costs.push(if rec.flags & VALID == 0 {
                CostOf::Disabled
            } else {
                pricing.cost_of(
                    self.names.name(rec.model),
                    (rec.policy != ABSENT).then(|| self.names.name(rec.policy)),
                    rec.at,
                    rec.nums[0],
                    rec.nums[3],
                    rec.nums[1],
                    rec.flags & NONEMPTY != 0,
                )
            });
        }
        costs
    }
}

fn record(names: &mut Names, offset: u64, row: &Value) -> Rec {
    let mut present = 0;
    let mut nums = [0.0; 6];
    for (i, key) in NUMS.iter().enumerate() {
        if row.get(*key).is_some() {
            present |= 1 << i;
        }
        nums[i] = number(row, key);
    }
    let mut flags = VALID;
    for (bit, set) in [
        (ERROR, truth(&row["error"])),
        (NONEMPTY, truth(row)),
        (TTFT, truth(&row["ttft_ms"])),
        (SPEED, truth(&row["tokens_per_sec"])),
        (ELAPSED, truth(&row["elapsed_ms"])),
    ] {
        if set {
            flags |= bit;
        }
    }
    let result = outcome(row);
    Rec {
        offset,
        at: number(row, "at"),
        nums,
        ttft: number(row, "ttft_ms"),
        speed: number(row, "tokens_per_sec"),
        elapsed: number(row, "elapsed_ms"),
        model: names.id(&text(&row["model"])),
        account: names.id(&text(&row["account"])),
        realm: names.id(&text(&row["realm"])),
        key: if row.get("key").is_none() {
            ABSENT
        } else {
            names.id(&text(&row["key"]))
        },
        policy: row["cost_policy"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map_or(ABSENT, |s| names.id(s)),
        outcome: match result.as_str() {
            "completed" => Outcome::Completed,
            "client_aborted" => Outcome::Aborted,
            _ => Outcome::Other,
        },
        present,
        flags,
    }
}

/// One JSON number slot updated exactly like `crate::bump`.
#[derive(Clone, Copy, Default)]
enum Num {
    #[default]
    Missing,
    Int(f64),
    Float(f64),
    Null,
}
impl Num {
    const I0: Num = Num::Int(0.0);
    const F0: Num = Num::Float(0.0);
    fn get(self) -> f64 {
        match self {
            Num::Int(v) | Num::Float(v) => v,
            _ => 0.0,
        }
    }
    fn add(&mut self, by: f64) {
        let sum = self.get() + by;
        let float = matches!(self, Num::Float(_));
        *self = if !float && sum.fract() == 0.0 && sum.abs() < i64::MAX as f64 {
            Num::Int(sum as i64 as f64)
        } else if sum.is_finite() {
            Num::Float(sum)
        } else {
            Num::Null
        };
    }
    fn value(self) -> Value {
        match self {
            Num::Int(v) => json!(v as i64),
            Num::Float(v) => json!(v),
            _ => Value::Null,
        }
    }
}

/// Insertion-ordered map, like a `serde_json` object with `preserve_order`.
struct Ordered<K, V> {
    index: HashMap<K, usize>,
    items: Vec<(K, V)>,
}
impl<K: Hash + Eq + Copy, V> Ordered<K, V> {
    fn new() -> Self {
        Self {
            index: HashMap::new(),
            items: Vec::new(),
        }
    }
    fn entry(&mut self, key: K, make: impl FnOnce() -> V) -> &mut V {
        let i = *self.index.entry(key).or_insert_with(|| {
            self.items.push((key, make()));
            self.items.len() - 1
        });
        &mut self.items[i].1
    }
}
fn counts<K: Hash + Eq + Copy>(map: &Ordered<K, Num>, name: impl Fn(K) -> String) -> Value {
    let mut out = Map::new();
    for (k, n) in &map.items {
        out.insert(name(*k), n.value());
    }
    Value::Object(out)
}

/// `Context::row_realm` per distinct (realm, account, model) triple.
struct Realms<'a> {
    ctx: &'a Context<'a>,
    memo: HashMap<(u32, u32, u32), usize>,
    names: Vec<String>,
    ids: HashMap<String, usize>,
}
impl<'a> Realms<'a> {
    fn new(ctx: &'a Context<'a>) -> Self {
        Self {
            ctx,
            memo: HashMap::new(),
            names: Vec::new(),
            ids: HashMap::new(),
        }
    }
    fn of(&mut self, names: &Names, rec: &Rec) -> usize {
        let key = (rec.realm, rec.account, rec.model);
        if let Some(id) = self.memo.get(&key) {
            return *id;
        }
        let realm = self.ctx.row_realm(&json!({
            "realm": names.name(rec.realm),
            "account": names.name(rec.account),
            "model": names.name(rec.model),
        }));
        let next = self.names.len();
        let id = *self.ids.entry(realm.clone()).or_insert(next);
        if id == next {
            self.names.push(realm);
        }
        self.memo.insert(key, id);
        id
    }
    fn is(&mut self, names: &Names, rec: &Rec, want: &Option<String>) -> bool {
        match want {
            None => true,
            Some(want) => {
                let id = self.of(names, rec);
                self.names[id] == *want
            }
        }
    }
}
fn within(at: f64, window: (Option<f64>, Option<f64>)) -> bool {
    window.0.is_none_or(|lo| at >= lo) && window.1.is_none_or(|hi| at <= hi)
}
fn or_default(names: &mut Names, id: u32, default: &str) -> u32 {
    if id == 0 {
        names.id(default)
    } else {
        id
    }
}

/// Rows at `offsets`, or None when any of them no longer parses.
fn reread(ix: &Index, path: &Path, offsets: &[u64]) -> io::Result<Option<Vec<Value>>> {
    let end = offsets.last().map_or(0, |last| {
        let next = ix.recs.partition_point(|r| r.offset <= *last);
        ix.recs.get(next).map_or(ix.cursor.offset, |r| r.offset)
    });
    Ok(scan::read_rows(path, offsets, end)?.into_iter().collect())
}

struct Bucket {
    requests: Num,
    accounts: Ordered<u32, Num>,
    cost: Num,
    fields: [Num; 6],
    order: Vec<usize>,
}
impl Bucket {
    fn new() -> Self {
        Self {
            requests: Num::I0,
            accounts: Ordered::new(),
            cost: Num::F0,
            fields: [Num::Missing; 6],
            order: Vec::new(),
        }
    }
    fn feed(&mut self, rec: &Rec, cost: CostOf) {
        self.requests.add(1.0);
        for i in 0..6 {
            if rec.present & (1 << i) != 0 {
                if matches!(self.fields[i], Num::Missing) {
                    self.order.push(i);
                }
                self.fields[i].add(rec.nums[i]);
            }
        }
        if rec.account != 0 {
            self.accounts.entry(rec.account, || Num::Missing).add(1.0);
        }
        if let CostOf::Known(cny) = cost {
            self.cost.add(cny);
        }
    }
    fn value(&self, names: &Names) -> Value {
        let mut out = Map::new();
        out.insert("requests".into(), self.requests.value());
        out.insert(
            "accounts".into(),
            counts(&self.accounts, |id| names.name(id).to_owned()),
        );
        out.insert("cost_cny".into(), self.cost.value());
        for i in &self.order {
            out.insert(NUMS[*i].into(), self.fields[*i].value());
        }
        Value::Object(out)
    }
}

pub(crate) fn totals(
    ix: &mut Index,
    path: &Path,
    ctx: &Context,
    pricing: &Arc<Pricing>,
    started: f64,
) -> io::Result<Value> {
    let realm = ctx.realm(false);
    let window = ctx.window();
    let mut out = views::totals_template(path, &realm, pricing, started);
    ix.costs(pricing);
    let Index {
        recs, names, costs, ..
    } = ix;
    let costs = &costs.as_ref().unwrap().1;
    let mut realms = Realms::new(ctx);
    let (mut requests, mut errors) = (Num::I0, Num::I0);
    let (mut credit, mut cost_cny) = (Num::F0, Num::F0);
    let mut tokens = [Num::I0; 5];
    let mut missing: Ordered<u32, Num> = Ordered::new();
    type Nested<T> = Ordered<u32, Ordered<usize, T>>;
    let mut by_model: Ordered<u32, Bucket> = Ordered::new();
    let mut by_realm: Nested<Bucket> = Ordered::new();
    let mut by_acct: Nested<Ordered<u32, Bucket>> = Ordered::new();
    for (rec, cost) in recs.iter().zip(costs.iter()) {
        if rec.flags & VALID == 0 || !realms.is(names, rec, &realm) || !within(rec.at, window) {
            continue;
        }
        let completed = rec.outcome == Outcome::Completed;
        if completed {
            &mut requests
        } else {
            &mut errors
        }
        .add(1.0);
        if rec.outcome == Outcome::Aborted {
            continue;
        }
        let model = or_default(names, rec.model, "unknown");
        credit.add(rec.nums[CREDIT]);
        match cost {
            CostOf::Known(cny) => cost_cny.add(*cny),
            CostOf::Unknown => missing.entry(model, || Num::Missing).add(1.0),
            CostOf::Disabled => {}
        }
        if !completed {
            continue;
        }
        for (slot, n) in tokens.iter_mut().zip(rec.nums) {
            slot.add(n);
        }
        let r = realms.of(names, rec);
        let account = or_default(names, rec.account, "(unattributed)");
        by_model.entry(model, Bucket::new).feed(rec, *cost);
        by_realm
            .entry(model, Ordered::new)
            .entry(r, Bucket::new)
            .feed(rec, *cost);
        by_acct
            .entry(model, Ordered::new)
            .entry(r, Ordered::new)
            .entry(account, Bucket::new)
            .feed(rec, *cost);
    }
    out["requests"] = requests.value();
    out["errors"] = errors.value();
    for (key, n) in crate::TOKENS.iter().zip(tokens) {
        out[*key] = n.value();
    }
    out["credit"] = credit.value();
    out["cost_cny"] = cost_cny.value();
    out["cost_missing"] = counts(&missing, |id| names.name(id).to_owned());
    let model_name = |id: u32| names.name(id).to_owned();
    let realm_name = |id: usize| realms.names[id].clone();
    let mut m = Map::new();
    for (model, bucket) in &by_model.items {
        m.insert(model_name(*model), bucket.value(names));
    }
    out["by_model"] = Value::Object(m);
    let mut m = Map::new();
    for (model, inner) in &by_realm.items {
        let mut r = Map::new();
        for (realm, bucket) in &inner.items {
            r.insert(realm_name(*realm), bucket.value(names));
        }
        m.insert(model_name(*model), Value::Object(r));
    }
    out["by_model_realm"] = Value::Object(m);
    let mut m = Map::new();
    for (model, inner) in &by_acct.items {
        let mut r = Map::new();
        for (realm, accounts) in &inner.items {
            let mut a = Map::new();
            for (account, bucket) in &accounts.items {
                a.insert(names.name(*account).to_owned(), bucket.value(names));
            }
            r.insert(realm_name(*realm), Value::Object(a));
        }
        m.insert(model_name(*model), Value::Object(r));
    }
    out["by_model_acct"] = Value::Object(m);
    views::totals_tail(&mut out, ctx);
    Ok(out)
}

pub(crate) fn recent(
    ix: &mut Index,
    path: &Path,
    ctx: &Context,
    pricing: &Pricing,
) -> io::Result<Option<Value>> {
    let realm = ctx.realm(false);
    let mut realms = Realms::new(ctx);
    let mut total = 0u64;
    let mut matching = Vec::new();
    for rec in &ix.recs {
        if rec.flags & VALID == 0 {
            if realm.is_none() {
                total += 1;
            }
        } else if realms.is(&ix.names, rec, &realm) {
            total += 1;
            matching.push(rec.offset);
        }
    }
    let (limit, pages, page, start, end) = views::recent_pages(ctx, total, matching.len() as u64);
    let Some(rows) = reread(ix, path, &matching[start as usize..end as usize])? else {
        return Ok(None);
    };
    let mut rows: Vec<Value> = rows
        .into_iter()
        .map(|row| views::decorate_recent(row, ctx, pricing))
        .collect();
    rows.reverse();
    Ok(Some(
        json!({"total":total,"page":page,"limit":limit,"total_pages":pages,"usd_cny":pricing.fx(),"rows":rows}),
    ))
}

pub(crate) fn perf(ix: &mut Index, path: &Path, ctx: &Context) -> io::Result<Option<Value>> {
    let sample = views::perf_sample(ctx);
    let tail = &ix.recs[ix.recs.len().saturating_sub(sample)..];
    let offsets: Vec<u64> = tail
        .iter()
        .filter(|r| r.flags & VALID != 0)
        .map(|r| r.offset)
        .collect();
    let Some(mut rows) = reread(ix, path, &offsets)? else {
        return Ok(None);
    };
    rows.reverse();
    let mut projected = VecDeque::with_capacity(tail.len());
    for rec in tail {
        projected.push_back(if rec.flags & VALID == 0 {
            None
        } else {
            rows.pop().as_ref().map(views::perf_project)
        });
    }
    Ok(Some(views::perf_finish(projected, sample, ctx)))
}

pub(crate) fn timeseries(ix: &mut Index, path: &Path, ctx: &Context) -> io::Result<Option<Value>> {
    let realm = ctx.realm(false);
    let (lo, hi, step, origin) = views::timeseries_grid(ctx);
    struct Slot {
        requests: Num,
        errors: Num,
        tokens: [Num; 5],
        credit: Num,
    }
    let mut series: BTreeMap<i64, Slot> = BTreeMap::new();
    let mut credited: Vec<(f64, u64)> = Vec::new();
    let mut realms = Realms::new(ctx);
    for rec in &ix.recs {
        if rec.flags & VALID == 0
            || !realms.is(&ix.names, rec, &realm)
            || !within(rec.at, (Some(lo), Some(hi)))
        {
            continue;
        }
        let k = ((rec.at - origin) / step as f64).floor() as i64;
        let slot = series.entry(k).or_insert(Slot {
            requests: Num::I0,
            errors: Num::I0,
            tokens: [Num::I0; 5],
            credit: Num::F0,
        });
        if rec.outcome == Outcome::Completed {
            slot.requests.add(1.0);
            for (t, n) in slot.tokens.iter_mut().zip(rec.nums) {
                t.add(n);
            }
        } else {
            slot.errors.add(1.0);
        }
        if rec.outcome != Outcome::Aborted {
            slot.credit.add(rec.nums[CREDIT]);
        }
        if rec.nums[CREDIT] > 0.0 {
            credited.push((rec.at, rec.offset));
        }
    }
    // A stable sort of all candidates then a cut equals the streaming
    // sort-and-truncate-after-each-push.
    credited.sort_by(|a, b| b.0.total_cmp(&a.0));
    credited.truncate(50);
    let offsets: Vec<u64> = credited.iter().map(|c| c.1).collect();
    let Some(rows) = reread(ix, path, &offsets)? else {
        return Ok(None);
    };
    let credits: Vec<Value> = rows.iter().map(views::credit_entry).collect();
    let series: Vec<Value> = series
        .into_iter()
        .map(|(k, slot)| {
            let mut b = views::timeseries_bucket(origin, k, step);
            b["requests"] = slot.requests.value();
            b["errors"] = slot.errors.value();
            for (key, n) in crate::TOKENS.iter().zip(slot.tokens) {
                b[*key] = n.value();
            }
            b["credit"] = slot.credit.value();
            b
        })
        .collect();
    Ok(Some(
        json!({"ok":true,"realm":realm.as_deref().unwrap_or("all"),"bucket_seconds":step,"since":lo,"until":hi,"series":series,"credits":credits}),
    ))
}

pub(crate) fn by_account(ix: &mut Index) -> Value {
    struct Acct {
        requests: Num,
        tokens: [Num; 5],
        models: Ordered<u32, Num>,
    }
    let Index { recs, names, .. } = ix;
    let mut accounts: Ordered<u32, Acct> = Ordered::new();
    for rec in recs.iter() {
        if rec.flags & VALID == 0 || rec.flags & ERROR != 0 {
            continue;
        }
        let uid = or_default(names, rec.account, "(unattributed)");
        let model = or_default(names, rec.model, "?");
        let a = accounts.entry(uid, || Acct {
            requests: Num::I0,
            tokens: [Num::I0; 5],
            models: Ordered::new(),
        });
        a.requests.add(1.0);
        for (t, n) in a.tokens.iter_mut().zip(rec.nums) {
            t.add(n);
        }
        a.models.entry(model, || Num::Missing).add(1.0);
    }
    let mut out = Map::new();
    for (uid, a) in &accounts.items {
        let name = names.name(*uid);
        let mut e = views::by_account_entry(name);
        e["requests"] = a.requests.value();
        for (key, n) in crate::TOKENS.iter().zip(a.tokens) {
            e[*key] = n.value();
        }
        e["models"] = counts(&a.models, |id| names.name(id).to_owned());
        out.insert(name.to_owned(), e);
    }
    views::by_account_rows(Value::Object(out))
}

const STAT_KEYS: [&str; 15] = [
    "requests",
    "errors",
    "prompt_tokens",
    "completion_tokens",
    "reasoning_tokens",
    "cached_tokens",
    "total_tokens",
    "credit",
    "cost_cny",
    "ttft_sum",
    "ttft_n",
    "speed_sum",
    "speed_n",
    "elapsed_sum",
    "elapsed_n",
];
#[derive(Clone, Copy)]
struct Stat([Num; 15]);
impl Stat {
    fn new() -> Self {
        let (i, f) = (Num::I0, Num::F0);
        Self([i, i, i, i, i, i, i, f, f, f, i, f, i, f, i])
    }
    fn feed(&mut self, rec: &Rec, cost: f64) {
        let s = &mut self.0;
        s[if rec.outcome == Outcome::Completed {
            0
        } else {
            1
        }]
        .add(1.0);
        for i in 0..5 {
            s[2 + i].add(rec.nums[i]);
        }
        s[7].add(rec.nums[CREDIT]);
        s[8].add(cost);
        for (bit, value, at) in [
            (TTFT, rec.ttft, 9),
            (SPEED, rec.speed, 11),
            (ELAPSED, rec.elapsed, 13),
        ] {
            if rec.flags & bit != 0 {
                s[at].add(value);
                s[at + 1].add(1.0);
            }
        }
    }
    fn value(&self) -> Value {
        let mut out = Map::new();
        for (key, n) in STAT_KEYS.iter().zip(self.0) {
            out.insert((*key).into(), n.value());
        }
        Value::Object(out)
    }
}
struct ModelStat([Num; 4]);
impl ModelStat {
    fn new() -> Self {
        Self([Num::I0, Num::I0, Num::I0, Num::F0])
    }
    fn value(&self) -> Value {
        let mut out = Map::new();
        for (key, n) in ["requests", "tokens", "reasoning", "cost_cny"]
            .iter()
            .zip(self.0)
        {
            out.insert((*key).into(), n.value());
        }
        Value::Object(out)
    }
}

struct Entry {
    window: Stat,
    all: Stat,
    window_models: Ordered<u32, ModelStat>,
    all_models: Ordered<u32, ModelStat>,
}
impl Entry {
    fn new() -> Self {
        Self {
            window: Stat::new(),
            all: Stat::new(),
            window_models: Ordered::new(),
            all_models: Ordered::new(),
        }
    }
    fn feed(&mut self, rec: &Rec, model: u32, cost: f64, in_window: bool, distribution: bool) {
        let completed = rec.outcome == Outcome::Completed;
        let model_feed = |map: &mut Ordered<u32, ModelStat>| {
            if completed {
                let m = &mut map.entry(model, ModelStat::new).0;
                m[0].add(1.0);
                m[1].add(rec.nums[4]);
                m[2].add(rec.nums[2]);
                m[3].add(cost);
            }
        };
        self.all.feed(rec, cost);
        if distribution {
            model_feed(&mut self.all_models);
        }
        if in_window {
            self.window.feed(rec, cost);
            if distribution {
                model_feed(&mut self.window_models);
            }
        }
    }
    fn value(&self, names: &Names) -> Value {
        let models = |map: &Ordered<u32, ModelStat>| {
            let mut out = Map::new();
            for (id, stat) in &map.items {
                out.insert(names.name(*id).to_owned(), stat.value());
            }
            Value::Object(out)
        };
        json!({"window":self.window.value(),"all_time":self.all.value(),
            "window_models":models(&self.window_models),"all_models":models(&self.all_models)})
    }
}

type KeyEntry = (Entry, Ordered<usize, Num>, f64);

pub(crate) fn analytics(ix: &mut Index, ctx: &Context, pricing: &Arc<Pricing>) -> Value {
    let realm = ctx.realm(true);
    let window = ctx.window();
    ix.costs(pricing);
    let Index {
        recs, names, costs, ..
    } = ix;
    let costs = &costs.as_ref().unwrap().1;
    let mut realms = Realms::new(ctx);
    let mut summary = Entry::new();
    let mut accounts: Ordered<u32, (u32, Entry)> = Ordered::new();
    let mut models: Ordered<u32, Entry> = Ordered::new();
    let mut keys: Ordered<u32, KeyEntry> = Ordered::new();
    let before = names.id("__before_keys__");
    for (rec, cost) in recs.iter().zip(costs.iter()) {
        if rec.flags & VALID == 0
            || rec.outcome == Outcome::Aborted
            || !realms.is(names, rec, &realm)
        {
            continue;
        }
        let in_window = within(rec.at, window);
        let cost = cost.cny();
        let model = or_default(names, rec.model, "(unknown)");
        summary.feed(rec, model, cost, in_window, false);
        let uid = or_default(names, rec.account, "(unattributed)");
        let key = if rec.key == ABSENT {
            before
        } else {
            or_default(names, rec.key, "__no_key__")
        };
        accounts
            .entry(uid, || (rec.realm, Entry::new()))
            .1
            .feed(rec, model, cost, in_window, true);
        models
            .entry(model, Entry::new)
            .feed(rec, model, cost, in_window, false);
        let r = realms.of(names, rec);
        let k = keys.entry(key, || (Entry::new(), Ordered::new(), 0.0));
        k.0.feed(rec, model, cost, in_window, true);
        k.1.entry(r, || Num::Missing).add(1.0);
        k.2 = k.2.max(rec.at);
    }
    let summary = json!({"window":summary.window.value(),"all_time":summary.all.value()});
    let mut account_map = Map::new();
    for (uid, (realm, entry)) in &accounts.items {
        let mut e = entry.value(names);
        e["uid"] = json!(names.name(*uid));
        e["nickname"] = json!(names.name(*uid));
        e["domain"] = json!("");
        e["realm"] = json!(names.name(*realm));
        account_map.insert(names.name(*uid).to_owned(), e);
    }
    let mut model_map = Map::new();
    for (model, entry) in &models.items {
        model_map.insert(
            names.name(*model).to_owned(),
            json!({"model":names.name(*model),"window":entry.window.value(),"all_time":entry.all.value()}),
        );
    }
    let mut key_map = Map::new();
    for (key, (entry, key_realms, last)) in &keys.items {
        let mut e = entry.value(names);
        e["realms"] = counts(key_realms, |id| realms.names[id].clone());
        e["last_at"] = json!(last);
        key_map.insert(names.name(*key).to_owned(), e);
    }
    analytics::assemble(
        ctx,
        pricing,
        summary,
        Value::Object(account_map),
        Value::Object(model_map),
        Value::Object(key_map),
    )
}

/// The daily-guard fold over today's records, in file order.
pub(crate) fn daily(ix: &Index, lo: f64) -> Value {
    let mut tokens: Ordered<u32, Num> = Ordered::new();
    let mut credits: Ordered<u32, Num> = Ordered::new();
    let mut models: Ordered<u32, Ordered<u32, Num>> = Ordered::new();
    for rec in &ix.recs {
        if rec.flags & VALID == 0
            || rec.account == 0
            || rec.outcome == Outcome::Aborted
            || rec.at < lo
        {
            continue;
        }
        let total = rec.nums[4];
        tokens.entry(rec.account, || Num::Missing).add(total);
        if rec.nums[CREDIT] != 0.0 {
            credits.entry(rec.account, || Num::F0).add(rec.nums[CREDIT]);
        }
        if rec.model != 0 {
            models
                .entry(rec.account, Ordered::new)
                .entry(rec.model, || Num::Missing)
                .add(total);
        }
    }
    let name = |id: u32| ix.names.name(id).to_owned();
    let mut m = Map::new();
    for (account, inner) in &models.items {
        m.insert(name(*account), counts(inner, name));
    }
    json!({"tokens":counts(&tokens, name),"credits":counts(&credits, name),"models":Value::Object(m)})
}
