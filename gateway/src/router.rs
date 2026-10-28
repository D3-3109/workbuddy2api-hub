use crate::{
    dispatch, panel,
    state::App,
    wire::{self, Reply},
};
use hyper::{body::Incoming, Method, Request};
use serde_json::{json, Value};
use std::{net::SocketAddr, sync::Arc};
use wbcore::types::Realm;

pub async fn route(app: Arc<App>, request: Request<Incoming>, peer: SocketAddr) -> Reply {
    let (parts, body) = request.into_parts();
    let mut path = parts.uri.path().to_string();
    let q = wire::query(parts.uri.query());
    let mut pinned = None;
    if let Some(suffix) = path.strip_prefix("/u/") {
        let Some((reference, tail)) = suffix.split_once('/') else {
            return wire::error(404, "pinned account route must be /u/<ref>/v1/...", false);
        };
        let reference = url::form_urlencoded::parse(format!("ref={reference}").as_bytes())
            .next()
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        if reference.is_empty() {
            return wire::error(404, "account reference required", false);
        }
        path = format!("/{tail}");
        pinned = Some(reference);
    }
    let is_api = matches!(
        path.as_str(),
        "/v1/chat/completions"
            | "/chat/completions"
            | "/v1/completions"
            | "/completions"
            | "/v1/responses"
            | "/responses"
            | "/v1/responses/compact"
            | "/responses/compact"
            | "/v1/messages"
            | "/messages"
            | "/v1/messages/count_tokens"
            | "/messages/count_tokens"
            | "/v1/models"
            | "/models"
            | "/v1/balance-summary"
    );
    if pinned.is_some() && !is_api {
        return wire::error(
            404,
            "pinned account routes are supported only for model APIs",
            false,
        );
    }
    let anthropic = path == "/messages"
        || path.starts_with("/messages/")
        || path == "/v1/messages"
        || path.starts_with("/v1/messages/");
    let mut reply = if parts.method == Method::OPTIONS {
        wire::response(204, "text/plain", vec![])
    } else if parts.method == Method::GET && matches!(path.as_str(), "/" | "/dashboard" | "/ui") {
        let lang = app.settings.snapshot()["ui_language"]
            .as_str()
            .unwrap_or("zh")
            .to_owned();
        let html = include_str!("../../assets/dashboard.html").replace(
            "data-ui-language=\"__WB_UI_LANGUAGE__\"",
            &format!("data-ui-language=\"{lang}\""),
        );
        let tag = format!("\"{}\"", wbcore::ids::sha256_hex(&html));
        wire::etag(
            wire::response(200, "text/html; charset=utf-8", html.into_bytes()),
            &parts.headers,
            &tag,
        )
    } else if parts.method == Method::GET && is_api {
        match app.authenticate(&parts.headers, &q) {
            Err(()) => wire::error(401, "missing or invalid API key", anthropic),
            Ok(key) => match app.request_realm(&parts.headers, &q, key.as_ref()) {
                Err(()) => wire::error(400, "realm must be intl or cn", anthropic),
                Ok(realm) => match path.as_str() {
                    "/v1/models" | "/models" => {
                        models(&app, realm, key.as_ref(), pinned.as_deref())
                    }
                    "/v1/balance-summary" => balance(&app, realm, key.as_ref(), pinned.as_deref()),
                    _ => wire::error(405, "this API requires POST", anthropic),
                },
            },
        }
    } else if parts.method == Method::GET {
        if app.panel_valid(&parts.headers) {
            if let Some(reply) = crate::account_net::get(app.clone(), &path, &q).await {
                reply
            } else {
                panel::get(app.clone(), &path, &parts.headers, &q, peer).await
            }
        } else {
            panel::get(app.clone(), &path, &parts.headers, &q, peer).await
        }
    } else if parts.method == Method::POST {
        let key = if is_api {
            match app.authenticate(&parts.headers, &q) {
                Ok(k) => k,
                Err(()) => return api_error(401, "missing or invalid API key", anthropic, &path),
            }
        } else {
            if !matches!(path.as_str(), "/panel/login" | "/panel/logout")
                && !app.panel_valid(&parts.headers)
            {
                return wire::error(401, "panel password required", false);
            }
            None
        };
        let enabled = app.settings.snapshot()["enable_anthropic_messages"] == json!(true)
            || std::env::var("WB_ENABLE_ANTHROPIC_MESSAGES")
                .is_ok_and(|s| matches!(s.as_str(), "1" | "true" | "yes" | "on"));
        if anthropic && !enabled {
            return api_error(
                400,
                "Anthropic Messages is disabled; explicitly enable it in settings",
                true,
                &path,
            );
        }
        match wire::read_json(
            body,
            &parts.headers,
            app.args.max_payload_bytes,
            path == "/accounts/import",
        )
        .await
        {
            Err((status, msg)) => wire::error(status, msg, anthropic),
            Ok(payload) if is_api => {
                dispatch::chat(app.clone(), &path, payload, &parts.headers, &q, key, pinned).await
            }
            Ok(payload) => {
                if let Some(reply) =
                    crate::billing::handle(app.clone(), &path, payload.clone()).await
                {
                    reply
                } else if let Some(reply) =
                    crate::account_net::post(app.clone(), &path, payload.clone()).await
                {
                    reply
                } else {
                    panel::post(app.clone(), &path, payload, &parts.headers, &q, peer).await
                }
            }
        }
    } else {
        wire::error(405, "method not allowed", anthropic)
    };
    wire::cors(&path, &mut reply);
    reply
}

fn api_error(status: u16, message: &str, anthropic: bool, path: &str) -> Reply {
    let mut r = wire::error(status, message, anthropic);
    wire::cors(path, &mut r);
    r
}

fn models(app: &App, realm: Realm, key: Option<&Value>, pin: Option<&str>) -> Reply {
    let settings = app.settings.snapshot();
    let mut response = app.catalog.read().list(realm);
    let account = match pin {
        Some(reference) => match app.pool.find_unique(reference) {
            Ok(a) => Some(a),
            Err(e) => return wire::error(404, e.to_string(), false),
        },
        None => None,
    };
    if let (Some(a), Some(key)) = (&account, key) {
        if !wbsettings::key_allows_account(key, a.uid()) {
            return wire::error(403, "API key does not permit the pinned account", false);
        }
    }
    if let Some(rows) = response["data"].as_array_mut() {
        rows.retain(|row| {
            let id = row["id"].as_str().unwrap_or("");
            if settings["filter_models_by_key"] == json!(true)
                && !dispatch::key_allows_model(key, id)
            {
                return false;
            }
            if let Some(a) = &account {
                if a.realm() != realm {
                    return false;
                }
                if a.public_metadata()["disabledModels"]
                    .as_array()
                    .is_some_and(|m| {
                        m.iter()
                            .any(|v| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case(id)))
                    })
                {
                    return false;
                }
            }
            if settings["models_only_available"] == json!(true) {
                return app.pool.accounts().iter().any(|a| {
                    if a.realm() != realm
                        || !account_allowed(key, a.uid())
                        || account.as_ref().is_some_and(|pin| pin.uid() != a.uid())
                    {
                        return false;
                    }
                    let view = a.public_metadata();
                    let pro = app.catalog.read().requires_pro(realm, id);
                    view["enabled"] != json!(false)
                        && (!pro
                            || view["tier"]["paid"] == json!(true)
                            || view["credits"]["is_paid_user"] == json!(true))
                        && !view["disabledModels"].as_array().is_some_and(|xs| {
                            xs.iter()
                                .any(|x| x.as_str().is_some_and(|s| s.eq_ignore_ascii_case(id)))
                        })
                });
            }
            true
        });
    }
    wire::json_reply(200, response)
}

fn account_allowed(key: Option<&Value>, uid: &str) -> bool {
    key.is_none_or(|k| wbsettings::key_allows_account(k, uid))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn balance_account_filter_uses_exact_account_ids() {
        let key = json!({"accounts":["account-a"]});
        assert!(account_allowed(Some(&key), "account-a"));
        assert!(!account_allowed(Some(&key), "account-b"));
        assert!(!account_allowed(Some(&key), "account-a-extra"));
        assert!(account_allowed(None, "account-b"));
    }
}

fn balance(app: &App, realm: Realm, key: Option<&Value>, pin: Option<&str>) -> Reply {
    let pinned = match pin {
        Some(reference) => match app.pool.find_unique(reference) {
            Ok(a) if a.realm() != realm => {
                return wire::error(409, "pinned account belongs to another realm", false)
            }
            Ok(a) if !account_allowed(key, a.uid()) => {
                return wire::error(403, "API key does not permit the pinned account", false)
            }
            Ok(a) => Some(a.uid().to_owned()),
            Err(_) => return wire::error(404, "pinned account not found", false),
        },
        None => None,
    };
    let mut balance = 0.0;
    let mut count = 0;
    let mut enabled = 0;
    for a in app.pool.accounts() {
        let view = a.public_metadata();
        if a.realm() != realm
            || view["enabled"] == json!(false)
            || !account_allowed(key, a.uid())
            || pinned.as_ref().is_some_and(|uid| uid != a.uid())
        {
            continue;
        }
        enabled += 1;
        if let Some(n) = view["credits"]["remain"].as_f64() {
            balance += n;
            count += 1;
        }
    }
    wire::json_reply(
        200,
        json!({"balance":if count>0{json!(balance)}else{Value::Null},"unit":"credit","realm":realm.as_str(),"accounts_enabled":enabled,"accounts_with_credit_data":count,"updated_at":wbcore::timeutil::now()}),
    )
}
