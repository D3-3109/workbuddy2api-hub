//! The embedded dashboard (`assets/web`, listed by `build.rs`).
//!
//! Every file is hashed and gzip-compressed once, on first use. Responses carry
//! an ETag and `Cache-Control: no-cache`, so a reload costs one 304 per file.

use crate::wire::{self, Reply};
use bytes::Bytes;
use hyper::{header, HeaderMap};
use std::{
    collections::HashMap,
    io::Write,
    sync::{Arc, LazyLock},
};

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

/// Concatenated in this order into `/ui/app.css`; any other css file follows alphabetically.
const CSS_ORDER: &[&str] = &[
    "css/tokens.css",
    "css/base.css",
    "css/layout.css",
    "css/components.css",
    "css/pages.css",
];

struct Asset {
    mime: &'static str,
    raw: Bytes,
    gzip: Bytes,
    tag: String,
}

fn asset(mime: &'static str, body: Vec<u8>) -> Asset {
    use sha2::Digest;
    let tag = format!("\"{}\"", &hex::encode(sha2::Sha256::digest(&body))[..24]);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let gzip = gz
        .write_all(&body)
        .and_then(|()| gz.finish())
        .unwrap_or_default();
    Asset {
        mime,
        raw: body.into(),
        gzip: gzip.into(),
        tag,
    }
}

fn mime_of(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

static FILES: LazyLock<HashMap<String, Arc<Asset>>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    for (name, body) in WEB_ASSETS {
        if *name == "index.html" || name.starts_with("css/") {
            continue;
        }
        map.insert(
            (*name).to_owned(),
            Arc::new(asset(mime_of(name), body.as_bytes().to_vec())),
        );
    }
    let find = |wanted: &str| {
        WEB_ASSETS
            .iter()
            .find(|(n, _)| *n == wanted)
            .map(|(_, b)| *b)
    };
    let mut css = String::new();
    for name in CSS_ORDER {
        if let Some(body) = find(name) {
            css.push_str(body);
            css.push('\n');
        }
    }
    for (name, body) in WEB_ASSETS {
        if name.starts_with("css/") && !CSS_ORDER.contains(name) {
            css.push_str(body);
            css.push('\n');
        }
    }
    map.insert(
        "app.css".to_owned(),
        Arc::new(asset("text/css; charset=utf-8", css.into_bytes())),
    );
    map
});

static INDEX: LazyLock<parking_lot::Mutex<HashMap<&'static str, Arc<Asset>>>> =
    LazyLock::new(Default::default);

fn accepts_gzip(headers: &HeaderMap) -> bool {
    wire::header_text(headers, "accept-encoding").is_some_and(|v| {
        v.split(',').any(|e| {
            e.split(';')
                .next()
                .is_some_and(|e| e.trim().eq_ignore_ascii_case("gzip"))
        })
    })
}

fn serve(asset: &Asset, headers: &HeaderMap) -> Reply {
    let gz = accepts_gzip(headers) && !asset.gzip.is_empty();
    let body = if gz {
        asset.gzip.clone()
    } else {
        asset.raw.clone()
    };
    let mut reply = wire::response_bytes(200, asset.mime, body);
    if gz {
        reply.headers_mut().insert(
            header::CONTENT_ENCODING,
            header::HeaderValue::from_static("gzip"),
        );
    }
    let mut reply = wire::etag(reply, headers, &asset.tag);
    // `etag` answers 304 with a JSON content type; keep the real one for revalidation.
    if let Ok(v) = header::HeaderValue::from_str(asset.mime) {
        reply.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    reply.headers_mut().insert(
        header::VARY,
        header::HeaderValue::from_static("Accept-Encoding"),
    );
    reply
}

/// `/`, `/dashboard`, `/ui`: the shell page, stamped with the instance's default UI language.
pub fn index(lang: &str, headers: &HeaderMap) -> Reply {
    let lang: &'static str = match lang {
        "en" => "en",
        "zh-Hant" => "zh-Hant",
        _ => "zh",
    };
    let page = INDEX
        .lock()
        .entry(lang)
        .or_insert_with(|| {
            let html = WEB_ASSETS
                .iter()
                .find(|(n, _)| *n == "index.html")
                .map(|(_, b)| *b)
                .unwrap_or("<!doctype html><title>dashboard missing</title>")
                .replace(
                    "data-ui-language=\"__WB_UI_LANGUAGE__\"",
                    &format!("data-ui-language=\"{lang}\""),
                );
            Arc::new(asset("text/html; charset=utf-8", html.into_bytes()))
        })
        .clone();
    serve(&page, headers)
}

/// `/ui/<path>`: static module, stylesheet or data file.
pub fn file(path: &str, headers: &HeaderMap) -> Reply {
    let rel = path.trim_start_matches("/ui/");
    match FILES.get(rel) {
        Some(asset) if !rel.contains("..") => serve(asset, headers),
        _ => wire::error(404, "not found", false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use std::collections::HashSet;

    fn names() -> HashSet<&'static str> {
        WEB_ASSETS.iter().map(|(n, _)| *n).collect()
    }

    /// Resolve an import specifier found in the module at `from` to an asset name.
    fn resolve(from: &str, spec: &str) -> Option<String> {
        if let Some(rest) = spec.strip_prefix("/ui/") {
            return Some(rest.to_owned());
        }
        if !spec.starts_with('.') {
            return None;
        }
        let mut parts: Vec<&str> = from.split('/').collect();
        parts.pop();
        for seg in spec.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                s => parts.push(s),
            }
        }
        Some(parts.join("/"))
    }

    #[test]
    fn every_import_resolves_to_an_embedded_file() {
        let have = names();
        let import = Regex::new(r#"(?:\bfrom\s*|\bimport\s*\(\s*|\bimport\s+)['"]([^'"]+)['"]"#)
            .expect("valid regex");
        let mut checked = 0;
        for (name, body) in WEB_ASSETS.iter().filter(|(n, _)| n.ends_with(".js")) {
            for cap in import.captures_iter(body) {
                let spec = &cap[1];
                // Only app-relative imports are resolvable; bare names such as "openai"
                // appear inside the code samples shown on the Agents page.
                let Some(target) = resolve(name, spec) else {
                    continue;
                };
                assert!(
                    have.contains(target.as_str()),
                    "{name} imports {spec:?} -> {target}, which is not under assets/web"
                );
                checked += 1;
            }
        }
        assert!(checked > 50, "suspiciously few imports found: {checked}");
    }

    #[test]
    fn shell_references_and_pages_exist() {
        let have = names();
        let index = WEB_ASSETS
            .iter()
            .find(|(n, _)| *n == "index.html")
            .expect("index.html is embedded")
            .1;
        assert!(index.contains("__WB_UI_LANGUAGE__"));
        assert!(index.contains("href=\"/ui/app.css\""));
        assert!(index.contains("src=\"/ui/js/main.js\""));
        assert!(have.contains("js/main.js"));
        for css in CSS_ORDER.iter().take(4) {
            assert!(have.contains(css), "missing stylesheet {css}");
        }
        // Pages the router can open (js/main.js NAV list).
        for page in [
            "gateway",
            "accounts",
            "analytics",
            "tasks",
            "agents",
            "logs",
            "settings",
        ] {
            let path = format!("js/pages/{page}.js");
            let body = WEB_ASSETS.iter().find(|(n, _)| *n == path).map(|(_, b)| *b);
            let body = body.unwrap_or_else(|| panic!("page module {path} is missing"));
            assert!(
                body.contains("export async function mount")
                    || body.contains("export function mount"),
                "{path} does not export mount()"
            );
        }
    }

    #[test]
    fn index_carries_language_and_revalidates() {
        let headers = HeaderMap::new();
        let reply = index("en", &headers);
        assert_eq!(reply.status().as_u16(), 200);
        let tag = reply
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .expect("etag")
            .to_owned();
        assert_eq!(
            reply
                .headers()
                .get(header::CACHE_CONTROL)
                .map(|v| v.as_bytes()),
            Some(&b"no-cache"[..])
        );

        let mut cond = HeaderMap::new();
        cond.insert(header::IF_NONE_MATCH, tag.parse().expect("header value"));
        assert_eq!(index("en", &cond).status().as_u16(), 304);
        // A different instance language is a different document.
        let other = index("zh-Hant", &headers);
        assert_ne!(
            other
                .headers()
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok()),
            Some(tag.as_str())
        );
        // Unknown languages fall back instead of reaching the HTML.
        let fallback = index("\"><script>", &headers);
        assert_eq!(fallback.status().as_u16(), 200);
    }

    #[test]
    fn files_are_served_gzipped_with_the_right_type_or_404() {
        let mut gz = HeaderMap::new();
        gz.insert(
            header::ACCEPT_ENCODING,
            "gzip".parse().expect("header value"),
        );

        let js = file("/ui/js/main.js", &gz);
        assert_eq!(js.status().as_u16(), 200);
        assert_eq!(
            js.headers().get(header::CONTENT_TYPE).map(|v| v.as_bytes()),
            Some(&b"text/javascript; charset=utf-8"[..])
        );
        assert_eq!(
            js.headers()
                .get(header::CONTENT_ENCODING)
                .map(|v| v.as_bytes()),
            Some(&b"gzip"[..])
        );

        let css = file("/ui/app.css", &HeaderMap::new());
        assert_eq!(css.status().as_u16(), 200);
        assert!(css.headers().get(header::CONTENT_ENCODING).is_none());

        assert_eq!(file("/ui/js/nope.js", &gz).status().as_u16(), 404);
        assert_eq!(file("/ui/js/../index.html", &gz).status().as_u16(), 404);
        // The shell page is served by `index`, never as a raw file.
        assert_eq!(file("/ui/index.html", &gz).status().as_u16(), 404);
    }
}
