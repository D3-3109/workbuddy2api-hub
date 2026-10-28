mod account_net;
mod billing;
mod cli;
mod dispatch;
mod log_channel;
mod net;
mod panel;
mod router;
mod state;
mod wire;

use clap::Parser;
use parking_lot::{Mutex, RwLock};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};
use tokio::sync::{watch, Semaphore};
use wbcore::types::Realm;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    struct LogShutdown;
    impl Drop for LogShutdown {
        fn drop(&mut self) {
            log_channel::shutdown(std::time::Duration::from_millis(500));
        }
    }
    let _log_shutdown = LogShutdown;
    let mut args = cli::Args::parse();
    args.validate()?;
    for dir in [&args.accounts_dir, &args.usage_dir] {
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
        }
    }
    let settings = wbsettings::Settings::load(args.accounts_dir.join("settings.json"))?;
    if let Some(password) = args.panel_password.take() {
        settings.set_password(&password)?;
    }
    let mut launcher = args
        .api_key
        .take()
        .or_else(|| std::env::var("WB_PROXY_KEY").ok())
        .unwrap_or_default();
    if args.lan && launcher.is_empty() {
        launcher = settings.ensure_launcher_key()?.0;
    }
    let extra_keys = if launcher.is_empty() {
        vec![]
    } else {
        vec![launcher]
    };
    if !args.host.is_loopback() && !settings.auth_required(&extra_keys) {
        anyhow::bail!(
            "refusing unauthenticated network listener; configure an API key or use --lan"
        );
    }
    if !args.host.is_loopback() && settings.password_is_default() {
        anyhow::bail!("refusing network listener with default panel password; set WB_PANEL_PASSWORD or --panel-password");
    }
    let (pool, load_issues) = wbaccounts::AccountPool::load_dir(&args.accounts_dir)?;
    for issue in load_issues {
        log_channel::emit(
            "WARN",
            &format!("skipped account file: {}", issue.path.display()),
        );
    }
    let persisted = std::fs::read(args.accounts_dir.join("active_realm.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    let realm = persisted
        .as_ref()
        .and_then(|v| v["realm"].as_str())
        .and_then(Realm::parse)
        .or_else(|| {
            std::env::var("WB_PROXY_DEFAULT_REALM")
                .ok()
                .and_then(|s| Realm::parse(&s))
        })
        .unwrap_or(Realm::Intl);
    let usage = wbusage::UsageStore::new(args.usage_dir.clone());
    let (shutdown, _) = watch::channel(false);
    let app = Arc::new(state::App {
        slots: Arc::new(Semaphore::new(args.max_concurrent_chat)),
        args,
        settings,
        pool: Arc::new(pool),
        catalog: RwLock::new(wbcatalog::Catalog::bundled()),
        usage,
        http: wbhttp::Client::new()?,
        realm: RwLock::new(realm),
        extra_keys,
        login_limit: wbsettings::LoginRateLimiter::new(),
        refresh_locks: Mutex::new(HashMap::new()),
        restrictions: Mutex::new(HashMap::new()),
        logs: Mutex::new(VecDeque::new()),
        shutdown,
        started: wbcore::timeutil::now(),
    });
    if app.settings.password_is_default() {
        app.log("WARN","panel password is still the baseline default; set WB_PANEL_PASSWORD before exposing the panel");
    }
    let signal = app.shutdown.clone();
    tokio::spawn(async move {
        net::wait_for_signal().await;
        let _ = signal.send(true);
    });
    let maintenance = if !app.args.offline && app.args.test_upstream.is_none() {
        Some(tokio::spawn(dispatch::maintenance(app.clone())))
    } else {
        None
    };
    let result = net::serve(app.clone()).await;
    let _ = app.shutdown.send(true);
    if let Some(task) = maintenance {
        task.abort();
    }
    result
}
