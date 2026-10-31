use crate::{CancelToken, HttpRequest, Result, Source, SourceError, network::checked_url};
use rquickjs::{Context, Function, Object, Runtime};
use scraper::{Html, Selector};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
const MAX: usize = 2 * 1024 * 1024;
pub(crate) fn invoke(
    source: &Source,
    op: &str,
    request: &Value,
    cancel: &CancelToken,
) -> Result<Value> {
    let settings = source.settings.lock().unwrap().clone();
    let secrets = source
        .manifest
        .configuration
        .iter()
        .filter(|f| f.kind == crate::SettingKind::Secret)
        .filter_map(|f| settings.get(&f.key).and_then(Value::as_str))
        .flat_map(|s| [s, s.trim()])
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    invoke_with_settings(source, op, request, cancel, settings)
        .map_err(|error| redact(error, &secrets))
}
fn redact(error: SourceError, secrets: &[String]) -> SourceError {
    let replace = |mut message: String| {
        for secret in secrets {
            message = message.replace(secret, "[redacted]");
        }
        message
    };
    match error {
        SourceError::Script(message) => SourceError::Script(replace(message)),
        SourceError::Network(message) => SourceError::Network(replace(message)),
        SourceError::ParseChanged(message) => SourceError::ParseChanged(replace(message)),
        other => other,
    }
}
fn invoke_with_settings(
    source: &Source,
    op: &str,
    request: &Value,
    cancel: &CancelToken,
    settings: Value,
) -> Result<Value> {
    cancel.check()?;
    if op != "capabilities" && !source.manifest.capabilities.iter().any(|c| c == op) {
        return Err(SourceError::ParseChanged("unsupported operation".into()));
    }
    let mut request = request
        .as_object()
        .cloned()
        .ok_or_else(|| SourceError::ParseChanged("request must be object".into()))?;
    request.insert("op".into(), json!(op));
    request.insert("apiVersion".into(), json!(1));
    request.insert("settings".into(), settings.clone());
    let input_request = Value::Object(request.clone());
    let request = serde_json::to_string(&request).unwrap();
    if request.len() > MAX {
        return Err(SourceError::Script("input byte limit".into()));
    }
    let runtime = Runtime::new().map_err(script)?;
    runtime.set_memory_limit(32 * 1024 * 1024);
    runtime.set_max_stack_size(512 * 1024);
    let started = Instant::now();
    let deadline = Duration::from_secs(20);
    let token = cancel.clone();
    let active = source.active.clone();
    let io_micros = Arc::new(AtomicU64::new(0));
    let io_budget = io_micros.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        started.elapsed() > deadline
            || started
                .elapsed()
                .saturating_sub(Duration::from_micros(io_budget.load(Ordering::Relaxed)))
                > Duration::from_secs(5)
            || token.is_cancelled()
            || !active.load(Ordering::Acquire)
    })));
    let context = Context::full(&runtime).map_err(script)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let bytes = Arc::new(AtomicUsize::new(0));
    let failure = Arc::new(Mutex::new(None));
    let raw = context.with(|ctx| -> Result<String> {
        let caught = |e| SourceError::Script(rquickjs::CaughtError::from_error(&ctx,e).to_string());
        let nova = Object::new(ctx.clone()).map_err(script)?;
        let http = Object::new(ctx.clone()).map_err(script)?;
        let host = source.transport.clone();
        let permissions = source.manifest.permissions.clone();
        let token = cancel.clone();
        let failure_ref = failure.clone();
        let active = source.active.clone();
        let http_request = Function::new(ctx.clone(), move |raw: String| -> String {
            let operation = (|| {
                token.check()?;
                if !active.load(Ordering::Acquire) { return Err(SourceError::Cancelled); }
                if started.elapsed() > deadline { return Err(SourceError::Script("operation deadline".into())); }
                if raw.len() > MAX || calls.fetch_add(1, Ordering::Relaxed) >= 12 {
                    return Err(SourceError::Network("per-operation request limit".into()));
                }
                let input: HttpRequest = serde_json::from_str(&raw)
                    .map_err(|_| SourceError::Network("invalid HTTP input".into()))?;
                checked_url(&input.url, &permissions)?;
                crate::network::validate_headers(&input.headers, &permissions)?;
                let method = if input.method.is_empty() { "GET" } else { input.method.as_str() };
                if method != "GET" && !(method == "POST" && permissions.http_post) {
                    return Err(SourceError::Network("HTTP method not permitted".into()));
                }
                let remaining = MAX.saturating_sub(bytes.load(Ordering::Relaxed));
                let io_started = Instant::now();
                let response = host.request(&input, remaining, &token);
                io_micros.fetch_add(io_started.elapsed().as_micros().min(u64::MAX as u128) as u64, Ordering::Relaxed);
                let response = response?;
                if response.body.len() > remaining {
                    return Err(SourceError::Network("aggregate response byte limit".into()));
                }
                bytes.fetch_add(response.body.len(), Ordering::Relaxed);
                let body = String::from_utf8(response.body)
                    .map_err(|_| SourceError::ParseChanged("metadata is not UTF-8".into()))?;
                Ok(json!({"status":response.status,"url":response.url,"body":body,"headers":response.headers}).to_string())
            })();
            match operation {
                Ok(value) => value,
                Err(e) => {
                    let result = json!({"error":{"code":code(&e),"message":e.to_string()}}).to_string();
                    *failure_ref.lock().unwrap() = Some(e);
                    result
                }
            }
        }).map_err(script)?;
        http.set("request", http_request).map_err(script)?;
        nova.set("http", http).map_err(script)?;
        let html = Object::new(ctx.clone()).map_err(script)?;
        html.set("select", Function::new(ctx.clone(), select_html).map_err(script)?).map_err(script)?;
        nova.set("html", html).map_err(script)?;
        let url = Object::new(ctx.clone()).map_err(script)?;
        let resolve = Function::new(ctx.clone(), |relative: String, base: String| -> String {
            if relative.len()>8192 || base.len()>8192 { return String::new(); }
            url::Url::parse(&base).ok().and_then(|b| b.join(&relative).ok())
                .map(String::from).unwrap_or_default()
        }).map_err(script)?;
        url.set("resolve", resolve).map_err(script)?;
        nova.set("url", url).map_err(script)?;
        let settings_api=Object::new(ctx.clone()).map_err(script)?;
        settings_api.set("get",Function::new(ctx.clone(),move|key:String|->String {settings.get(&key).unwrap_or(&Value::Null).to_string()}).map_err(script)?).map_err(script)?;
        nova.set("settings",settings_api).map_err(script)?;
        ctx.globals().set("nova", nova).map_err(script)?;
        ctx.eval::<(),_>("{const read=nova.settings.get; const parse=JSON.parse;nova.settings.get=key=>parse(read(key));}").map_err(script)?;
        ctx.eval::<(), _>("nova.http.get=(url,headers='{}')=>nova.http.request(JSON.stringify({url,headers:JSON.parse(headers)}));").map_err(script)?;
        // Keep captured intrinsics in a Rust-held function, away from source global mutation.
        let encode: Function = ctx.eval(BOUNDED_ENCODER).map_err(script)?;
        ctx.eval::<(), _>(source.script.as_str()).map_err(caught)?;
        ctx.globals().set("__nova_request", request).map_err(script)?;
        let result: rquickjs::Value = ctx.eval("globalThis.novaProvider.handle(JSON.parse(__nova_request))").map_err(caught)?;
        encode.call((result,)).map_err(caught)
    });
    cancel.check()?;
    if !source.active.load(Ordering::Acquire) {
        return Err(SourceError::Cancelled);
    }
    if let Some(error) = failure.lock().unwrap().take() {
        return Err(error);
    }
    if started.elapsed() > deadline {
        return Err(SourceError::Script("operation deadline exceeded".into()));
    }
    let raw = raw?;
    if raw.len() > MAX {
        return Err(SourceError::Script("output exceeds 2 MiB".into()));
    }
    let result: Value = serde_json::from_str(&raw)
        .map_err(|_| SourceError::ParseChanged("non-JSON result".into()))?;
    if let Some(error) = result.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("source error")
            .chars()
            .take(512)
            .collect::<String>();
        return Err(match error.get("code").and_then(Value::as_str) {
            Some("ChallengeRequired") => SourceError::ChallengeRequired,
            Some("AuthenticationRequired") => SourceError::AuthenticationRequired,
            Some("RateLimited") => SourceError::RateLimited,
            Some("NotFound") => SourceError::NotFound,
            Some("Cancelled") => SourceError::Cancelled,
            Some("Network") => SourceError::Network(message),
            _ => SourceError::ParseChanged(message),
        });
    }
    validate_result(source, op, &result, &input_request)?;
    Ok(result)
}
fn script(e: rquickjs::Error) -> SourceError {
    SourceError::Script(e.to_string())
}
fn code(e: &SourceError) -> &str {
    match e {
        SourceError::Cancelled => "Cancelled",
        SourceError::ChallengeRequired => "ChallengeRequired",
        SourceError::AuthenticationRequired => "AuthenticationRequired",
        SourceError::RateLimited => "RateLimited",
        SourceError::NotFound => "NotFound",
        _ => "Network",
    }
}
#[derive(Deserialize)]
struct Field {
    #[serde(default)]
    selector: String,
    #[serde(default = "text")]
    value: String,
}
fn text() -> String {
    "text".into()
}
fn select_html(raw: String, query: String, fields: String) -> String {
    if raw.len() > MAX || query.len() > 4096 || fields.len() > 16384 {
        return "[]".into();
    }
    let fields: BTreeMap<String, Field> = match serde_json::from_str(&fields) {
        Ok(v) => v,
        Err(_) => return "[]".into(),
    };
    if fields.len() > 64 {
        return "[]".into();
    }
    let Ok(query) = Selector::parse(&query) else {
        return "[]".into();
    };
    let document = Html::parse_document(&raw);
    let mut rows = Vec::new();
    let mut total = 0;
    for element in document.select(&query).take(4096) {
        let mut row = BTreeMap::new();
        for (name, spec) in &fields {
            let found = if spec.selector.is_empty() {
                Some(element)
            } else {
                Selector::parse(&spec.selector)
                    .ok()
                    .and_then(|s| element.select(&s).next())
            };
            let value = found
                .map(|e| match spec.value.as_str() {
                    "text" => e.text().collect::<String>().trim().into(),
                    "ownText" => e
                        .children()
                        .filter_map(|n| n.value().as_text())
                        .map(|t| t.text.as_ref())
                        .collect::<String>()
                        .trim()
                        .into(),
                    "html" => e.inner_html(),
                    attribute => e.value().attr(attribute).unwrap_or_default().into(),
                })
                .unwrap_or_default();
            total += name.len() + value.len() + 8;
            if total > MAX {
                return "[]".into();
            }
            row.insert(name, value);
        }
        rows.push(row)
    }
    serde_json::to_string(&rows)
        .ok()
        .filter(|s| s.len() <= MAX)
        .unwrap_or_else(|| "[]".into())
}

pub(crate) fn validate_script(source: &str) -> Result<()> {
    let runtime = Runtime::new().map_err(script)?;
    runtime.set_memory_limit(32 * 1024 * 1024);
    runtime.set_max_stack_size(512 * 1024);
    let started = Instant::now();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        started.elapsed() > Duration::from_secs(2)
    })));
    let context = Context::full(&runtime).map_err(script)?;
    context.with(|ctx| {
        ctx.globals().set("__code", source).map_err(script)?;
        ctx.eval::<(), _>("new Function(__code); void 0;")
            .map_err(|_| SourceError::Package("invalid JavaScript syntax".into()))
    })
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 8192 && !id.chars().any(char::is_control)
}
fn dto_error() -> SourceError {
    SourceError::ParseChanged("invalid source response identity or shape".into())
}
fn validate_result(source: &Source, op: &str, value: &Value, request: &Value) -> Result<()> {
    use std::collections::BTreeSet;
    match op {
        "browse" | "search" => {
            let list: crate::Listing =
                serde_json::from_value(value.clone()).map_err(|_| dto_error())?;
            let mut ids = BTreeSet::new();
            if list.items.len() > 4096
                || list.items.iter().any(|s| {
                    !valid_id(&s.id) || s.title.trim().is_empty() || !ids.insert(s.id.clone())
                })
            {
                return Err(dto_error());
            }
        }
        "details" => {
            let item: crate::Series =
                serde_json::from_value(value.clone()).map_err(|_| dto_error())?;
            if !valid_id(&item.id) || item.title.trim().is_empty() {
                return Err(dto_error());
            }
            if let Some(id) = request
                .get("seriesId")
                .or_else(|| request.get("id"))
                .and_then(Value::as_str)
                && id != item.id
            {
                return Err(dto_error());
            }
        }
        "chapters" => {
            let list: crate::ChapterList =
                serde_json::from_value(value.clone()).map_err(|_| dto_error())?;
            let mut ids = BTreeSet::new();
            let expected = request
                .get("seriesId")
                .or_else(|| request.get("id"))
                .and_then(Value::as_str);
            for c in &list.chapters {
                if !valid_id(&c.id)
                    || !valid_id(&c.series_id)
                    || c.title.trim().is_empty()
                    || !ids.insert(&c.id)
                    || expected.is_some_and(|id| id != c.series_id)
                {
                    return Err(dto_error());
                }
                if let Some(number) = &c.number {
                    let n = number.strip_prefix('-').unwrap_or(number);
                    let parts: Vec<_> = n.split('.').collect();
                    if parts.len() > 2
                        || parts
                            .iter()
                            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
                    {
                        return Err(dto_error());
                    }
                }
            }
        }
        "pages" | "resolvePage" => {
            let list: crate::PageList =
                serde_json::from_value(value.clone()).map_err(|_| dto_error())?;
            if list.pages.is_empty() {
                return Err(dto_error());
            }
            let mut ids = BTreeSet::new();
            for p in &list.pages {
                if !valid_id(&p.id)
                    || !ids.insert(&p.id)
                    || p.width == Some(0)
                    || p.height == Some(0)
                {
                    return Err(dto_error());
                }
                if let Some(path) = p
                    .url
                    .strip_prefix(&format!("addon://{}/", source.manifest.id))
                {
                    if !source.manifest.assets.iter().any(|a| a == path) || !p.headers.is_empty() {
                        return Err(dto_error());
                    }
                } else {
                    checked_url(&p.url, &source.manifest.permissions)?;
                    crate::network::validate_headers(&p.headers, &source.manifest.permissions)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

// JSON.stringify invokes the replacer before appending strings to its result. Counting
// escaped UTF-8 there rejects oversized output while it is still inside the heap cap.
const BOUNDED_ENCODER: &str = r#"(() => {
 const stringify=JSON.stringify, charCodeAt=String.prototype.charCodeAt, apply=Reflect.apply, toString=String;
 return value => {
  let bytes=0;
  function count(s) {
   for(let i=0;i<s.length;i++) {
    const n=apply(charCodeAt,s,[i]);
    bytes += n<32?6:n===34||n===92?2:n<128?1:n<2048?2:n>=55296&&n<=57343?6:3;
    if(bytes>2097152)throw Error('output exceeds 2 MiB');
   }
  }
  return stringify(value,(key,value)=>{
   bytes+=8; count(key);
   if(typeof value==='string'||typeof value==='number')count(toString(value));
   return value;
  });
 };
})()"#;
