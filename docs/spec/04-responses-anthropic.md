# 04 - Protocol translation: Responses API and Anthropic Messages <-> Chat Completions

Baseline: `py-baseline-1.7.32` (`git describe` = `v1.7.32`). All line numbers refer to
`wb_proxy.py` at that tag unless another file is named. The upstream only speaks
OpenAI Chat Completions (always streamed, see `build_upstream_body`, 13145-13239). This
document specifies the two translation layers that sit in front of it:

* `/v1/responses`, `/responses`, `/v1/responses/compact`, `/responses/compact` (OpenAI
  Responses API, used by Codex CLI/App/Desktop, Kelivo).
* `/v1/messages`, `/messages`, `/v1/messages/count_tokens`, `/messages/count_tokens`
  (Anthropic Messages API, used by Claude Code). **Disabled by default.**

Conventions used below:

* "chat body" = the intermediate Chat Completions dict produced by the translator. It is
  then passed through `build_upstream_body` (spec 03 / chat pipeline) on its way out.
  The translators do NOT produce the final wire body; several of their quirks only make
  sense because `build_upstream_body` normalises afterwards (tools wrapping,
  `tool_choice` string-ification, developer->system, system prompt injection, DeepSeek
  thinking injection).
* `_new_id(prefix)` = `prefix + uuid4().hex` (32 lowercase hex chars, no dashes)
  (15585-15586). Fixtures write these as `<prefix><hex>`.
* JSON emitted by Responses SSE uses Python `json.dumps(..., ensure_ascii=False)` with
  DEFAULT separators (`", "`, `": "`). Anthropic SSE frames use compact separators
  (`","`, `":"`). Non-stream JSON bodies (`Handler._json`, 18090) use default separators.
  The Rust port must reproduce these byte layouts if golden tests compare bytes; clients
  only need valid JSON.
* "truthy" means Python truthiness: `None`, `""`, `0`, `[]`, `{}`, `False` are falsy.
  Many branches below depend on it (e.g. an empty-string `content` delta is ignored).

---

## 0. Routing, auth, gating (do_POST 22031-22229)

Order of checks inside `do_POST` for these routes:

1. Path is taken without query string (`self.path.split("?")[0]`, 22034). Exact match
   only; no trailing-slash tolerance on the route list (22158-22163). (Compaction
   detection inside the handler does `rstrip("/")`, 21486, but the router would already
   have 404'd a trailing slash.)
2. `is_messages_route` = path in the four Messages paths (22147-22150).
3. Auth:
   * Messages routes: `_key_ok()` (18441-18452). On failure ->
     `_anthropic_error(401, "missing or invalid API key", "authentication_error")`.
   * Responses routes: `_authorized()` (OpenAI envelope on failure; see auth spec).
   * Key sources accepted by `_supplied_key` (18400-18440): `Authorization` header
     (`Bearer <k>` case-insensitive scheme, or raw value; surrounding quotes stripped),
     then headers `x-api-key`, `api-key`, `x-auth-token`, then query `key`, `api_key`,
     `api-key`. A logged-in panel session also passes.
4. **Anthropic feature gate** (22170-22180): if a Messages route and
   `_anthropic_messages_enabled()` is false, reply
   `400` Anthropic envelope, `error.type = "invalid_request_error"`, message exactly:

   ```
   Anthropic Messages is disabled by default: this gateway translates requests to Chat Completions and ignores cache_control. This can cause excessive credit consumption. Enable it in the Settings panel or set WB_ENABLE_ANTHROPIC_MESSAGES=1 to opt in at your own risk (see issue #367).
   ```

   This check happens **before** the body is read/parsed (the `_anthropic_error` helper
   drains the body itself, 18320-18324), and before any upstream call or usage row.
   It applies to `count_tokens` too (`tests/_test_anthropic_http.py:239-256`).

   `_anthropic_messages_enabled()` (17936-17940) is true when EITHER
   * settings file key `enable_anthropic_messages` is exactly JSON `true`
     (`wb_settings.enable_anthropic_messages`, wb_settings.py:1696-1698), read per
     request; OR
   * env `WB_ENABLE_ANTHROPIC_MESSAGES`, stripped+lowercased, in
     `{"1","true","yes","on"}`, read per request.
5. Body parse (`_payload_or_error`, 19395-19412): must be a JSON object. Errors on
   Messages routes use the Anthropic envelope (`_anthropic_route()`, 19413-19423):
   * too large (> `MAX_PAYLOAD_BYTES`, env `WB_MAX_PAYLOAD_BYTES`, default 16 MiB, line 40):
     `413`, `"payload too large (%d bytes > %d limit)"`, `invalid_request_error`.
   * bad JSON / not an object: `400 "invalid JSON body"`, `invalid_request_error`.
6. Model aliases (`wb_settings.model_aliases`) and background model alias are applied to
   the raw payload `model` (22186-22191) BEFORE translation.
7. Per-key concurrency (22193-22202). Messages route -> `_anthropic_error(429, msg,
   "rate_limit_error")`; Responses -> OpenAI 429. Message:
   `"Rate limit reached: API key '<name>' reached its concurrency limit (<n> in flight); retry shortly"`.
8. Messages: `count_tokens` is served immediately (no chat slot). `/v1/messages` takes a
   global chat slot (`_chat_slots`, wait `CHAT_SLOT_WAIT_SECONDS`, env `WB_CHAT_SLOT_WAIT`
   default 30s); failure -> Anthropic `503 overloaded_error`
   `"gateway is at its concurrent chat limit (%d in flight); retry shortly"`.
   Responses: same slot; failure -> OpenAI `503` with the same text (22221-22223).
9. Responses only: `_dispatch_chat_post` (22231). If `BLOCK_BACKGROUND_REQUESTS`
   (constant `False` at 12917) the background-request check could 400; at baseline it is
   inert. Then `_handle_responses`.

OpenAI error envelope (`_error`, 18308-18319):
`{"error": {"message": <str>, "type": <err_type default "server_error">, "code": <int status>, ["gateway_hint": ...]}}`.

Anthropic error envelope (`anthropic_error_obj`, 16534-16541):
`{"type": "error", "error": {"type": <type>, "message": <str or "upstream error">}}`.

`anthropic_error_type(status)` (16513-16531): 400/422 -> `invalid_request_error`;
401 -> `authentication_error`; 403 -> `permission_error`; 404 -> `not_found_error`;
429 -> `rate_limit_error`; 503/529 -> `overloaded_error`; anything else (incl. non-int ->
500) -> `api_error`.

---

## 1. Responses request -> chat body

Entry: `Handler._handle_responses` (21482-21606) calls `responses_to_chat(payload,
compaction)` (16443-16507), which calls `_responses_input_to_messages` (16186-16419).

### 1.1 Handler pre-processing (21482-21528)

1. `compaction = path.rstrip("/").endswith("/responses/compact") or
   _has_compaction_trigger(payload)` (21486-21487). See section 3.
2. **`previous_response_id`**: if truthy and NOT compaction -> `400`, OpenAI envelope,
   type `invalid_request_error`, message exactly
   `"previous_response_id is not supported: this gateway does not store response state. Send the full conversation in 'input' instead, or use a stateless client."`
   (21494-21500). **The gateway keeps no server-side response state.** `store` is ignored
   entirely (never read). No GET `/v1/responses/{id}` exists.
3. `session_key = extract_session_key(headers, payload)` (15266-15278): first truthy of
   headers `X-Conversation-Id`, `Conversation-Id`, `X-Session-Id`, `Session-Id`, then
   payload `conversation_id`, `session_id`, `metadata.conversation_id`; stripped string.
   Used for account affinity only.
4. `all_tools = list(payload.tools if list else []) + additional_tools_from_input(payload)`
   (21506-21508). `additional_tools_from_input` (15885-15899) concatenates `.tools` of
   every input item with `type == "additional_tools"` whose `tools` is a list.
5. `custom_names = custom_tool_names(all_tools)` (15682-15706): set of `name` for tools
   with `type` (case-insensitive) `"custom"`, recursing into `type: "namespace"` entries
   via the first list-valued field among `tools`, `children`, `functions`; recursion depth
   limit `NAMESPACE_MAX_DEPTH = 4` (stops when `_depth > 4`).
6. `chat_req = responses_to_chat(payload, compaction=compaction)`;
   `ns_map = chat_req.pop("_namespace_map", None)`.
7. `request_meta = {"tools": all_tools, "tool_choice": payload.get("tool_choice","auto"),
   "parallel_tool_calls": payload.get("parallel_tool_calls", True)}` (21513-21517). Echoed
   verbatim on the response objects (note: raw client values, including `null` if the key
   is present with null).
8. `model = payload.get("model") or "deepseek-v4.1-flash"` (21518) - used as the `model`
   field in Responses output. The chat body's `model` is `payload.get("model")` (may be
   None).
9. Realm / banned-model / per-key-model checks (21530-21540) -> OpenAI 400.
10. `open_upstream_with_fallback(chat_req, ...)` builds the wire body through
    `build_upstream_body`. Upstream exceptions map to OpenAI errors (21545-21592):
    `PinnedAccountError` -> its status; `ContentRejected` -> 403 `"upstream 403: <detail>"`;
    `LocalNetworkError` -> 503 (Chinese text); `TLSVerifyError` -> 502; `RateLimited` ->
    429 with `Retry-After` (`_rate_limited`, 18325-18362); `HTTPError` -> its code
    `"upstream <code>: <detail>"`; other -> 503 if message starts with
    `"no usable account"` (+ `" - add or enable one at the dashboard (/)"`), else 502
    `"upstream unreachable: <exc>"`.
11. Dispatch: compaction -> section 3; `stream` truthy -> 2.2; else 2.1.

### 1.2 `responses_to_chat` top-level fields (16443-16507)

Output dict key order: `model`, `messages`, [`response_format`], [`temperature`,
`top_p`, `seed`], [`max_tokens`], [`reasoning_effort`], [`tools`, `_namespace_map`],
[`_web_tools`], [`tool_choice`], [`parallel_tool_calls`].

| Responses field | Chat body | Rule / line |
|---|---|---|
| `model` | `model` | copied verbatim (may be None) (16453) |
| `instructions` | leading `{"role":"system","content":instructions}` | only if a str with non-whitespace (16189-16191); content is NOT stripped |
| `input` | `messages` | 1.3 |
| `text.format` | `response_format` | ONLY when env `WB_PRESERVE_REQUEST_CONTRACT` in `{1,true,yes,on}` (11806-11809, 16454-16467). `{"type":"text"|"json_object"}` -> `{"type": kind}`; `json_schema` -> `{"type":"json_schema","json_schema":{subset of name, description, schema, strict present in fmt}}`; anything else dropped. Default (flag off): `text` is ignored entirely |
| `temperature`, `top_p`, `seed` | same | copied when not None (16468-16470) |
| `max_output_tokens` | `max_tokens` | copied when not None; else catalog `maxOutputTokens` for model if a positive int (`model_default_max_output_tokens`, 13286-13296), e.g. `deepseek-v4.1-flash` -> 128000; unknown model -> omitted (16471-16476) |
| `reasoning.effort` | `reasoning_effort` | `reasoning` must be dict; falls back to top-level `reasoning_effort`; set only if truthy (16477-16484). `reasoning.summary` etc. ignored |
| `tools` + `additional_tools` | `tools`, `_namespace_map` | only if merged list non-empty AND not compaction: `expand_namespace_tools` then `_tools_for_chat` (16486-16493) - 1.4 |
| (web tools) | `tools`, `_web_tools: true` | only if panel switch `local_web_tools` is true AND not compaction AND client declared search/fetch (16498-16502) - 1.5 |
| `tool_choice` | `tool_choice` | copied verbatim when truthy and not compaction (16503-16504). Normalised later by `normalize_tool_choice` (12185-12225) |
| `parallel_tool_calls` | same | copied when not None and not compaction (16505-16506) |
| `stream` | - | NOT copied; upstream is always streamed (`build_upstream_body` sets `stream: true`, `stream_options.include_usage: true`) |
| `store`, `previous_response_id`, `metadata`, `user`, `include`, `truncation`, `background`, `prompt_cache_key`, `service_tier`, `top_logprobs` | - | ignored (never read by the translator) |

Compaction (`compaction` true): if the last user message does not already contain
`CONTEXT CHECKPOINT COMPACTION` (case-insensitive), append
`{"role":"user","content":COMPACTION_PROMPT}` (16448-16452); tools, `tool_choice`,
`parallel_tool_calls` and web tools are withheld.

`_`-prefixed keys (`_namespace_map`, `_web_tools`) are private markers; the handler pops
`_namespace_map`; `build_upstream_body` strips every key starting with `_` (13179-13180).

Downstream normalisation that matters for Responses (in `build_upstream_body`):
* `normalize_roles` maps `developer` -> `system` (11604+).
* A system prompt (`SYSTEM_PROMPT`, default "You are a helpful assistant.") is prepended
  if the first message is not `system` (13172-13173).
* `normalize_tool_choice` (12185-12225): string -> lowercase-trimmed check, `"none"`
  kept as `"none"`, other strings passed through unchanged; dict `{type:none}` -> `"none"`;
  `{type:auto|required}` -> that string; `{type:function}` -> `function.name` or `name`
  (bare string) or `"auto"` if empty; any other dict type or non-str/non-dict -> key
  removed. Tools are never removed for `"none"` (`tests/_test_tool_choice_none.py`).
* `normalize_tools` (12358-12397): any tool dict with `name` and no `function` is wrapped
  to `{"type":"function","function":{"name","description" (default ""),"parameters"
  (default {}), ["strict"]}}`; every other dict gets `type` forced to `"function"` (so a
  passed-through `{"type":"web_search"}` becomes `{"type":"function"}` on the wire - see
  Ambiguities).
* `map_reasoning_effort` (13332-13391) rewrites unsupported efforts (section 5).

### 1.3 `input` -> messages (`_responses_input_to_messages`, 16186-16419)

`input` string -> one `{"role":"user","content":input}` (even empty string).
`input` list -> processed in order with one piece of state, `pending_reasoning` (a
string, initially ""). Any other `input` type -> no messages.

`_flatten_content(content)` (15587-15619):
* None -> `""`; str -> itself; non-list -> `str(content)`.
* list: for each piece: str -> text; dict with `type` in `input_text`, `output_text`,
  `text`, `summary_text` -> text `piece.text or ""`; dict with `type` in `input_image`,
  `image_url`, `image` OR containing key `image_url` -> url = `piece.image_url or
  piece.url`; if that is a dict use its `.url`; if still empty and `piece.data` truthy ->
  `"data:<mimeType|mime_type|image/png>;base64,<data>"`; add an image part if url truthy.
  Other dicts / non-dicts skipped.
* If any image part exists, return the structured list
  `[{"type":"text","text":t} | {"type":"image_url","image_url":{"url":u}}, ...]`
  (texts included in order, even empty ones). Otherwise return the non-empty texts joined
  with `"\n"`.

Per item (non-str, non-dict items skipped; str items -> user message):

| item `type` | behaviour |
|---|---|
| `None` (absent), `"message"`, `"user"` | `body = _flatten_content(content)`; skipped if falsy (empty string / empty list). `role = item.role or "user"`; `developer` -> `system`. If role is `assistant` and previous message is `assistant`: append to previous (`prev.content = str(prev.content) + "\n" + str(body)` if prev content truthy else `body`), and if `pending_reasoning` and prev has no `reasoning_content`, attach it and clear. Otherwise append `{"role", "content": body}` plus `reasoning_content = pending_reasoning` when role is assistant and pending is set (then cleared). Other roles (`system`, `tool`, custom) are passed through as-is. |
| `reasoning` | text = `"\n".join(p.text for p in summary if dict and p.text)` if `summary` is a list; or `summary` if str. If empty: `content` str, or `_flatten_content(content)` if list (could be a list if images - edge). If text truthy: if previous message is `assistant`, set (overwrite) its `reasoning_content`; else store as `pending_reasoning` (overwriting any earlier pending). |
| `function_call_output` | see 1.3.1 |
| `function_call` | `tc = {"id": call_id or id or "", "type":"function", "function":{"name": name or "", "arguments": arguments or "{}"}}`. If previous message is assistant: append to its `tool_calls` (create list if missing) and attach pending reasoning if prev has none. Else append `{"role":"assistant","content":"","tool_calls":[tc]}` (+ `reasoning_content` if pending). `arguments` is NOT validated; a non-string `arguments` is passed through (sanitised later). |
| `custom_tool_call` | `input` dict/list -> `json.dumps(ensure_ascii=False)` (default separators); non-str -> `""` if None else `str()`. Then same as `function_call` with `arguments = json.dumps({"input": raw_input}, ensure_ascii=False)` -> e.g. `{"input": "*** Begin Patch"}` (with a space after the colon). |
| `custom_tool_call_output` | `output` list -> `_flatten_content`; dict -> `json.dumps`; str -> itself; else `str()`. Append `{"role":"tool","tool_call_id": call_id or "", "content": content}`. No orphan handling. |
| `agent_message` | `content` list -> join with `"\n"` of each part's `text` or `encrypted_content` (dict) or `str(p)`; else `str(content or "")`; strip. If non-empty -> user message `"[Message from another task - treat this as a user instruction]\n\n" + text`. |
| `compaction`, `compaction_summary`, `context_compaction` | `summary = _decode_compaction_summary(encrypted_content)`; if non-blank -> `{"role":"system","content":"Summary of the earlier conversation (context checkpoint):\n\n" + summary.strip()}` at its position. |
| `compaction_trigger` | ignored |
| `additional_tools` | ignored here (tools merged in 1.2) |
| anything else (e.g. `local_shell_call`, `web_search_call`, `item_reference`, `input_image` at top level) | logged `responses: WARNING unhandled input item type=...` and dropped |

Note on `role == "assistant"` merge: merging into the previous assistant message happens
both after a `function_call` (so `[function_call, message(assistant)]` becomes one
assistant message with `tool_calls` and the text) and after another assistant message
(two adjacent assistant texts are joined with `\n`).

`pending_reasoning` that is never consumed (no following assistant message/call) is
silently dropped.

#### 1.3.1 `function_call_output` and orphan outputs (16251-16299)

* **Orphan** = `call_id` falsy (missing/empty). Codex uses this shape for messages from a
  sibling task. Text = `_flatten_content(output)` if list; `json.dumps(output)` if dict;
  else `str(output or "")`.
  * If text is a list (images present): append
    `{"role":"user","content":[{"type":"text","text":"[Message from another task - treat this as a user instruction]"}] + parts}`.
  * Else strip; if non-empty append
    `{"role":"user","content":"[Message from another task - treat this as a user instruction]\n\n" + text}`.
  * If empty after strip -> **falls through** to the normal branch and emits
    `{"role":"tool","tool_call_id":"","content":<content>}` (verified: `output:""` ->
    `{"role":"tool","tool_call_id":"","content":""}`).
* Normal: content = `_flatten_content(list)`; dict that looks like an image
  (`type` in input_image/image_url/image or has `image_url`) -> `_flatten_content([dict])`
  (structured parts); other dict -> `json.dumps(dict, ensure_ascii=False)` (e.g.
  `{"a": 1}`); str -> itself; else `str(output)` (None -> the string `"None"`).
  Append `{"role":"tool","tool_call_id": call_id or "","content": content}`.

Tool-call pairing repair (merge adjacent tool_calls, repack results, drop orphan
tool_calls) is done later by `build_upstream_body` (13189-13192), not here.

### 1.4 Tools (custom / namespace / function / others)

`expand_namespace_tools(tools)` (15755-15823) returns `(flat, mapping)`:

* Walk top-level entries in order. Non-dicts skipped. Depth limit: entries deeper than
  `NAMESPACE_MAX_DEPTH` (4) are dropped.
* `type` (lowercased) `"namespace"`: sub-list = first list-valued of `tools`, `children`,
  `functions` (else empty); child namespace name = `entry.name or <parent ns> or ""`;
  recurse.
* Inside a namespace (`ns_name` truthy) and `type` in `("", "function")`: take
  `entry.function` if a dict, else build `{"name": entry.name, "description":
  entry.description or "", "parameters": entry.parameters or entry.input_schema or
  {"type":"object","properties":{}}}` plus `strict` copied ONLY when
  `WB_PRESERVE_REQUEST_CONTRACT` is on and `strict` in entry. Name stripped; skip if empty
  or already seen. Record `mapping[name] = ns_name`. Emit flat
  `{"type":"function","name","description": fn.description or "","parameters":
  fn.parameters or {"type":"object","properties":{}}, ["strict" if in fn]}`.
* Any other entry (top-level function/custom/web_search, or a namespaced custom tool):
  name = `entry.name or entry.function.name`, stripped. If a name exists and was seen,
  drop the entry; else mark seen and, if inside a namespace, `mapping[name] = ns_name`.
  Append the ORIGINAL entry object unchanged (nameless entries such as
  `{"type":"web_search"}` are never deduplicated).
* First occurrence of a name wins (top-level before namespace children when listed first).
* Input objects are not mutated.

`_tools_for_chat(flat)` (15902-15909): drops non-dicts; replaces each `type=="custom"`
(case-insensitive) with `_downgrade_custom_tool` (15709-15730):

```json
{"type": "function", "name": "<name>",
 "description": "<desc>\n\n<CUSTOM_TOOL_HINT>[\n\nGrammar:\n<format.definition>]",   // .strip()ed
 "parameters": {"type": "object",
   "properties": {"input": {"type": "string", "description": "Complete raw payload for this tool, verbatim."}},
   "required": ["input"]}}
```

`CUSTOM_TOOL_HINT` (15640-15644) = `This is a freeform tool. Put the COMPLETE raw payload into the single 'input' string parameter, verbatim. Do not wrap it in JSON, do not wrap it in markdown code fences, do not add commentary.`
The grammar block is appended only if `format` is a dict with a truthy `definition`.

The chat body therefore holds Responses-style flat function tools
(`{"type":"function","name":...,"parameters":...}`); `build_upstream_body.normalize_tools`
wraps them into `{"type":"function","function":{...}}`.

Namespace rejection: `REJECT_NAMESPACE_TOOLS = False` (15662), `find_namespace_tool`
and `NAMESPACE_TOOL_MESSAGE` (15664-15675) are **dead code at baseline** (no caller). The
port may omit them; namespace tools are always expanded and restored.

### 1.5 Web search tools (`local_web_tools` panel switch)

`local_web_tools_enabled()` (15943-15952) = settings key `local_web_tools` is exactly
`true` (wb_settings.py:2518-2529), default false, read per request.

* Switch OFF (default): `{"type":"web_search"}` etc. are forwarded as-is from
  `expand_namespace_tools` (and become `{"type":"function"}` after `normalize_tools`).
  Client-declared functions named `web_search` are the client's.
* Switch ON and `wb_webtools.client_wants_web(raw_tools)` reports search
  (`type` in `web_search`, `web_search_preview`, `web_search_preview_2025_03_11` or name
  `web_search`) or fetch (`web_fetch`): `chat.tools =
  wb_webtools.install_tool_defs(tools, wants)` (removes all same-named declarations and
  inserts the gateway's own `web_search`/`web_fetch` function definitions) and
  `chat._web_tools = True`. The gateway then executes those calls itself, up to
  `MAX_WEB_ROUNDS` (env `WB_MAX_WEB_ROUNDS`, default 3, max 8) follow-up upstream rounds
  (`follow_up_with_tool_results`, 16044-16096). The web tool executor itself is out of
  scope for this document (see wb_webtools spec); the event-level effect is in 2.2.6.

---

## 2. Chat -> Responses output

### 2.1 Non-stream (`_responses_nonstream_response` 21789-21840, `chat_to_response` 17271-17364)

1. The upstream stream is folded by `aggregate_stream` (15412-15558) into one
   `chat.completion` (content joined, reasoning joined, tool_calls merged by `index`,
   empty-name tool calls dropped, truncated-JSON tool calls dropped when
   `finish == "length"` or no `[DONE]` seen, `finish` forced to `tool_calls` when calls
   remain and finish was `stop`/None, usage estimated if missing/zero). That function is
   specified in the chat-pipeline spec; only its output matters here.
2. If web tools are active and the aggregated message contains `web_search`/`web_fetch`
   calls, run them and re-query (rounds); intermediate rounds are invisible.
3. `result = chat_to_response(chat_obj, model, custom_names, request_meta, ns_map,
   sources)`.
4. If `reasoning_only_response(result)` (17367-17378: `output_text` blank, has a
   `reasoning` item, has no `function_call`/`web_search_call` item, status not
   `incomplete`) -> OpenAI `502 "upstream returned reasoning without output text; retry"`
   (type `server_error`). Note a `custom_tool_call`-only answer with reasoning ALSO
   triggers this (verified: `['reasoning','custom_tool_call']` -> True). See Ambiguities.
5. Aggregation exception -> `502 "upstream stream error: <exc>"`.
6. Else `200` JSON body.

`chat_to_response` output object (key order as emitted):

```
id            "resp_" + hex                (new; NOT the upstream id)
object        "response"
created_at    int(time.time())
status        "completed", or "incomplete" when finish_reason == "length"
model         the request model (handler default "deepseek-v4.1-flash")
output        [ items... ]
output_text   message text (after DSML stripping)
metadata      {}
parallel_tool_calls  request_meta.parallel_tool_calls (default true)
tool_choice   request_meta.tool_choice (default "auto")
tools         request_meta.tools or []   (raw client tool list incl. additional_tools)
usage         _responses_usage(...)      (only if upstream usage truthy)
incomplete_details  {"reason":"max_output_tokens"}   (only when finish == "length")
```

`output` items, in this order:
1. `reasoning` if `message.reasoning_content` truthy:
   `{"id":"rs_<hex>","type":"reasoning","status":"completed","summary":[{"type":"summary_text","text":<reasoning>}]}`.
2. One item per `message.tool_calls` entry: `call_id = tc.id or "call_<hex>"`,
   `name = function.name or ""`.
   * name in `custom_names` -> `{"id":"ctc_<hex>","type":"custom_tool_call","status":"completed","call_id","name","input": _unwrap_custom_input(arguments or "")}`.
   * else -> `{"id":"fc_<hex>","type":"function_call","status":"completed","call_id","name","arguments": arguments or "{}"}`.
3. DSML fallback: only if the message has NO `tool_calls`: `parse_dsml_tool_calls(text)`
   (12432-12454); each recovered call -> `function_call` item (`call_id = dc.id`, which is
   always a fresh `call_<hex>`), and `text` = cleaned text. DSML calls are never custom.
4. `message` item if `text` truthy OR output is still empty:
   `{"id":"msg_<hex>","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":text,"annotations":build_citations(text,sources)}]}`;
   `content` is `[]` when text is empty.
5. If `ns_map` truthy: `apply_namespace_to_calls(output, ns_map)` (15865-15882) - for
   each `function_call`/`custom_tool_call` without a `namespace`, resolve name; if a
   namespace results, set `name` = bare name and add `"namespace": ns`.

`_unwrap_custom_input(args)` (15912-15928): non-str -> `json.dumps(args or "")`; parse
JSON; on failure return raw string; dict with `input` str -> that str; dict with
non-None non-str `input` -> `json.dumps(input)`; parsed str -> it; anything else
(dict without input, list, number) -> the raw `args` string.

`_responses_usage(u)` (17256-17270), returns None for falsy `u`:

```
{"input_tokens": prompt_tokens or 0,
 "input_tokens_details": {"cached_tokens": prompt_cache_hit_tokens or completion_tokens_details.cached_tokens or prompt_tokens_details.cached_tokens or 0},
 "output_tokens": completion_tokens or 0,
 "output_tokens_details": {"reasoning_tokens": completion_tokens_details.reasoning_tokens or 0},
 "total_tokens": total_tokens or 0}
```

`resolve_namespaced_name(name, mapping)` (15826-15847):
1. empty -> `(name, "")`.
2. exact key in mapping -> `(name, mapping[name])`.
3. contains `::` at index > 0: `tail = after first "::"`, `head = before`; return
   `(tail, mapping[tail])` if tail mapped, else `(tail, head)` (so `foo::bar` with empty
   mapping -> `("bar","foo")`; note `apply_namespace_to_calls`/`stamp_namespace`
   short-circuit when mapping is empty, so this only fires with a non-empty mapping).
4. for each `(tool, ns)` in mapping: `name == ns + "__" + tool` -> `(tool, ns)` (exact
   compare, so namespaces containing `__` work).
5. else `(name, "")`.

`build_citations(text, sources)` (15988-16041) produces `url_citation` annotations only
for URLs that appear in web tool sources; with web tools off `sources` is empty and
`annotations` is always `[]`.

### 2.2 Stream (`_responses_stream_response` 21702-21787, `stream_responses_events` 17381-17891)

HTTP: `200`, headers `Content-Type: text/event-stream; charset=utf-8`,
`Cache-Control: no-cache`, `Connection: close`, optional
`Access-Control-Allow-Origin: *` (when `cors_origin_allowed`). Headers are sent BEFORE
the first upstream byte is read (after the upstream connection opened successfully).

Frame format (`ev`, 17426-17432):

```
event: <type>\n
data: {"type": "<type>", "sequence_number": <n>, ...payload}\n
\n
```

`sequence_number` starts at 1 and increments by one per frame within one
`stream_responses_events` call. (With web tool rounds, each round restarts at 1 - see
Ambiguities.) Each frame is passed through `clean_responses_frame` (11578-11603), which
recursively removes `function_call` dicts with falsy `name` and empty `tool_calls`
lists from the JSON; it only re-serialises when it changed something (then joins lines
with `\n` and appends `\n\n`). Normal frames are not affected.

Keepalive: `SSEHeartbeat` (15354-15409) writes the SSE comment `: keepalive\n\n` whenever
nothing was written for `SSE_HEARTBEAT_SECONDS = 15.0` (15351). Written under the same
lock, so never splits a frame. Same for the Anthropic stream.

Per-stream ids, created once: `resp_id = resp_<hex>`, `msg_id = msg_<hex>`,
`rs_id = rs_<hex>`, `created = int(time.time())` (17383-17384).

#### 2.2.1 Response object snapshot (`resp_obj(status)`, 17406-17425)

```
{"id": resp_id, "object": "response", "created_at": created, "status": status,
 "model": model, "output": [non-null outputs in index order], "output_text": "".join(text_parts),
 "parallel_tool_calls": meta.parallel_tool_calls (default true),
 "tool_choice": meta.tool_choice (default "auto"), "tools": meta.tools or [],
 "metadata": {}, ["usage": _responses_usage(usage)]}
```

then namespace stamping on `output` if `ns_map`. Snapshots embedded in
`response.created` / `response.in_progress` are taken at that moment (output `[]`).

#### 2.2.2 Event sequence

Opening (skipped when `holder.suppress_lifecycle`, i.e. web-tool rounds > 0):
1. `response.created` `{"response": resp_obj("in_progress")}`
2. `response.in_progress` `{"response": resp_obj("in_progress")}`

Then for each upstream line (`strip_data_prefix`: trims, drops `:` comments, removes any
number of leading `data:`; `[DONE]` sets `saw_done`; unparsable JSON skipped). If the
chunk has truthy `usage`, keep it when there is no usage yet or its `total_tokens` >=
the stored one. For each `choice` in `choices` (ALL choices, not just index 0), with
`delta = choice.delta or {}`, process in this order:

**(a) reasoning** - `delta.reasoning_content` truthy:
* first time: `reason_index = len(outputs)`, reserve slot; emit
  `response.output_item.added` `{"output_index": i, "item": {"id": rs_id, "type":"reasoning","status":"in_progress","summary":[{"type":"summary_text","text":<joined so far, "">}]}}`
  then `response.reasoning_summary_part.added` `{"item_id": rs_id, "output_index": i, "summary_index": 0, "part": {"type":"summary_text","text":""}}`.
* append piece; emit `response.reasoning_summary_text.delta`
  `{"item_id", "output_index", "summary_index": 0, "delta": piece}`.

Reasoning arriving AFTER text has started still appends to the same reasoning item and
emits deltas against an item that may already be `done` (no new item is opened).

**(b) tool calls** - for each `tc` in `delta.tool_calls`: `idx = tc.index` (default 0),
`fn_name = function.name or ""`, `fn_args = function.arguments or ""`,
`call_id = tc.id or ""`.
* If web tools are active (`base_body._web_tools is True`) and (idx already internal OR
  `fn_name` is `web_search`/`web_fetch`): accumulate into `_internal_calls[idx]`, emit
  nothing.
* New `idx`: `out_idx = len(outputs)`, reserve slot; `c_id = call_id or "call_<hex>"`;
  `is_custom = fn_name truthy and fn_name in custom_names`; `item_id = "ctc_<hex>"` if
  custom else `"fc_<hex>"`; store `arguments = fn_args`. Emit
  `response.output_item.added` with item (key order exactly):
  `{"id": item_id, "status":"in_progress", "call_id": c_id, "name": fn_name, "type":"custom_tool_call", "input": ""}` or
  `{"id": item_id, "status":"in_progress", "call_id": c_id, "name": fn_name, "type":"function_call", "arguments": ""}`,
  then `stamp_namespace(item, ns_map)` BEFORE emitting (adds `"namespace"` and rewrites
  `name` to the bare name). **No delta event is emitted for arguments carried in the same
  first chunk**; they appear only in the `.done` event (fixture F6).
* Existing `idx`: if `fn_name` truthy and stored name empty, set it (and mark custom if
  in `custom_names`). If `fn_args` truthy: append; emit
  `response.custom_tool_call_input.delta` (custom) or
  `response.function_call_arguments.delta` (function) with
  `{"output_index", "item_id", "call_id", "delta": fn_args}`. The custom delta carries
  the RAW JSON fragment (e.g. `{"input":"*** Begin`), not unwrapped text.
* A `tc.id` arriving after the first chunk is ignored.

**(c) text** - `delta.content` truthy:
* first time (`msg_index is None`): if a reasoning item exists, close it now:
  `response.reasoning_summary_text.done` `{"item_id": rs_id, "output_index", "summary_index": 0, "text": full}`,
  `response.reasoning_summary_part.done` `{..., "part": {"type":"summary_text","text": full}}`,
  `response.output_item.done` `{"output_index", "item": reason_item("completed")}`.
  Then `msg_index = len(outputs)`, reserve; emit `response.output_item.added`
  `{"output_index", "item": {"id": msg_id, "type":"message","status":"in_progress","role":"assistant","content":[]}}`
  and `response.content_part.added`
  `{"item_id": msg_id, "output_index", "content_index": 0, "part": {"type":"output_text","text":"","annotations":[...]}}`.
* DSML buffering (17821-17876): append to `text_buffer` and loop:
  * no `<` in buffer: flush whole buffer as one `response.output_text.delta`
    `{"item_id": msg_id, "output_index", "content_index": 0, "delta": text}`.
  * a full DSML calls block (`DSML_CALLS_RE`, 12429) starting at the first `<`: flush the
    lead text, parse the block into `dsml_tool_calls`, drop it from the buffer, continue.
  * `cand = buffer[idx:idx+30]`; it is a DSML candidate if it contains `"DSML"`, or is
    shorter than 10 chars and contains none of space, tab, newline, `>`: flush the lead,
    keep `buffer[idx:]`, wait for more input.
  * otherwise flush up to (not including) the next `<` after idx, or the whole buffer if
    there is none.
  Empty-lead flushes still emit a delta with `"delta": ""` (can happen; harmless).
  `text_parts` accumulates exactly the flushed text.

**(d) finish** - `choice.finish_reason` truthy -> `finish = it` (default `"stop"`).

The loop never stops early on `finish_reason` or `[DONE]`; it reads until the upstream
iterator ends.

An upstream chunk `{"error": {...}}` is NOT recognised by the Responses translator: it
has no `choices`, so it is ignored and the stream finishes normally with an empty message
(verified; see Ambiguities).

**Truncation filter** (17884-17890): if any tool calls exist and (`finish == "length"`
or `[DONE]` never seen), every call whose accumulated arguments are a non-empty,
non-whitespace string that fails `json.loads` (`is_truncated_arguments`, 12091-12109) is
removed from `tool_calls_map`; `dropped_truncated` counts them. Its `output_item.added`
was already sent; no `done` will follow and its reserved slot stays `None` (filtered out
of `output`).

#### 2.2.3 Finalisation (`_finalize`, 17454-17682)

In order:
1. Reasoning still open (`reason_index` set and slot None): emit
   `reasoning_summary_text.done`, `reasoning_summary_part.done`, `output_item.done`.
2. For each remaining tool call, sorted by `idx`:
   * custom: `response.custom_tool_call_input.done`
     `{"output_index","item_id","call_id","input": _unwrap_custom_input(arguments)}`;
     item `{"id","type":"custom_tool_call","status":"completed","call_id","name","input": <unwrapped>}`.
   * function: `response.function_call_arguments.done`
     `{"output_index","item_id","call_id","arguments": <accumulated, may be "">}`;
     item `{"id","type":"function_call","status":"completed","call_id","name","arguments"}`.
   * `stamp_namespace(item)`; store; emit `response.output_item.done`
     `{"output_index", "item"}`.
   Note: the `.done` and `output_item.done` for a function call keep `arguments` exactly
   as accumulated (an empty string stays `""`, unlike the non-stream path which uses
   `"{}"`).
3. Flush remaining `text_buffer`: DSML-parse it; append cleaned remainder to
   `text_parts` and emit an `output_text.delta` if `msg_index` is set.
4. DSML fallback: `dsml_calls = dsml_tool_calls`, or if none, parse the full text again
   (if found, `full_text` becomes the cleaned text - but already-streamed deltas are not
   retracted). Only if no structured tool calls remain (`not tool_calls_map`): for each
   DSML call (except internal web calls, which go to `_internal_calls`) append a new
   output slot and emit `output_item.added` (item with `status:"in_progress"`,
   `arguments:""`), `function_call_arguments.delta` (full arguments),
   `function_call_arguments.done`, `output_item.done`. Item:
   `{"id":"fc_<hex>","type":"function_call","status":"completed","call_id": dc.id or "call_<hex>","name","arguments": dc.arguments or "{}"}`
   (+ namespace).
5. Internal web calls (if any): set `holder.internal_calls`, `holder.suppress_completion
   = True`; for each, emit a synthetic `web_search_call` item: `output_item.added`
   `{"id":"ws_<hex>","type":"web_search_call","status":"in_progress"}`,
   `response.web_search_call.in_progress` `{"output_index","item_id"}`,
   `response.web_search_call.searching`, `output_item.done` with
   `{"id","type":"web_search_call","status":"completed",["action": {"type":"search","query": q} | {"type":"open_page","url": u}]}`
   (action only when query/url non-empty), `response.web_search_call.completed`.
6. Message item - emitted if `msg_index` is set OR `full_text` truthy OR there is no
   other non-null output item. If not yet opened, open it (`output_item.added` +
   `content_part.added`) now. Then `response.output_text.done`
   `{"item_id","output_index","content_index":0,"text": full_text}`,
   `response.content_part.done` `{..., "part": {"type":"output_text","text": full_text,"annotations":[...]}}`,
   `response.output_item.done` `{"output_index","item": msg_item("completed")}` where
   `msg_item.content` is `[]` when `text_parts` is empty, else
   `[{"type":"output_text","text":"".join(text_parts),"annotations":[...]}]`.
   Consequence: a tool-call-only turn emits NO message item; an empty turn emits an
   empty message item (fixtures F6, F9).
7. Usage fallback: if no usage or `total_tokens == 0`, and there is any text or reasoning:
   `comp = estimate_tokens(text) + estimate_tokens(reasoning)`,
   `prompt_est = max(1, estimate_tokens(str(meta.get("input") or "")))` - `meta` is
   `request_meta`, which has no `input` key, so this is always **1** (see Ambiguities);
   usage = `{"prompt_tokens": prompt_est, "completion_tokens": comp, "total_tokens": sum,
   "completion_tokens_details": {"reasoning_tokens": estimate_tokens(reasoning)},
   "prompt_tokens_details": {"cached_tokens": 0}}`; stored in `holder.usage`.
8. Terminal status:
   * `empty_reasoning_only` = text blank AND reasoning non-blank AND no tool calls AND no
     DSML calls AND no internal calls AND `finish != "length"` AND nothing dropped.
   * `status = "completed"` if `finish != "length"` and nothing dropped, else
     `"incomplete"`; overridden to `"failed"` when `empty_reasoning_only`
     (`holder.empty_reasoning = True`).
   * `final = resp_obj(status)`; add `incomplete_details: {"reason":"max_output_tokens"}`
     when `finish == "length"` (NOT when incomplete only because of dropped calls).
   * `empty_reasoning_only` -> `final.error = {"code":"empty_reasoning_only","message":"upstream returned reasoning without output text; retry"}`
     and emit `response.failed {"response": final}`.
   * else, unless `holder.suppress_completion`, emit `response.completed {"response": final}`.

No `[DONE]` sentinel and no `response.output_text.annotation.added` events are ever
emitted on the Responses stream.

#### 2.2.4 Errors and aborts on the Responses stream

* Upstream exception while streaming (after headers): the handler writes ONE frame
  (`responses_stream_error_frame`, 12828-12833), NOT through `ev` and without
  `clean_responses_frame`:

  ```
  event: error
  data: {"type": "error", "code": "upstream_stream_error", "message": "upstream stream aborted: <exc>", "param": null, "sequence_number": 0}
  ```

  then returns (connection closes; `Connection: close`). Usage row outcome
  `upstream_aborted`, HTTP status recorded 502.
* Client disconnect (`BrokenPipeError`/`ConnectionResetError`/`ConnectionAbortedError`):
  stop silently, record usage with `outcome="client_aborted"`.
* Normal end: usage recorded `outcome="failed"` when `empty_reasoning`, else `completed`.

#### 2.2.5 Usage accounting across rounds

`holder.usage` after each round is summed into `total_usage` via `sum_usage`
(15965-15982: recursive numeric addition, first non-numeric value kept); the final
recorded usage is the sum. The `usage` inside `response.completed` is only the LAST
round's usage.

#### 2.2.6 Web tool rounds (stream)

When `_finalize` leaves `holder.internal_calls`, the handler closes the upstream, runs
`follow_up_with_tool_results` (appends an assistant message with synthetic ids
`call_web_<ms%1e6>_<i>`, tool results, and on the final round (`rounds >
MAX_WEB_ROUNDS`) removes web tools and appends a system message "The web tools are no
longer available. ..."), and runs `stream_responses_events` again with
`suppress_lifecycle=True` (no `response.created`/`in_progress`). Each round generates NEW
`resp_id`/`msg_id`/`rs_id` and restarts `sequence_number` at 1; only the last round emits
`response.completed`.

### 2.3 Event type catalogue (all types the Responses translator can emit)

```
response.created                         response.in_progress
response.output_item.added               response.output_item.done
response.reasoning_summary_part.added    response.reasoning_summary_part.done
response.reasoning_summary_text.delta    response.reasoning_summary_text.done
response.content_part.added              response.content_part.done
response.output_text.delta               response.output_text.done
response.function_call_arguments.delta   response.function_call_arguments.done
response.custom_tool_call_input.delta    response.custom_tool_call_input.done
response.web_search_call.in_progress     response.web_search_call.searching
response.web_search_call.completed
response.completed                       response.failed
error   (handler-level abort frame, sequence_number 0)
```

Never emitted: `response.incomplete`, `response.reasoning_text.*`, `response.refusal.*`,
`response.output_text.annotation.added`.

---

## 3. Remote compaction (`/v1/responses/compact` and `compaction_trigger`)

Detection: `compaction = path ends with "/responses/compact"` OR
`_has_compaction_trigger(payload)` (16149-16159): `input` str stripped equals
`"compaction_trigger"`, or `input` list contains a dict with
`type == "compaction_trigger"`.

Effects:
1. `previous_response_id` check is skipped (21494).
2. Request translation: section 1.2 compaction rules - history translated normally
   (replayed `compaction` items become system summaries, the trigger item is ignored);
   `COMPACTION_PROMPT` appended as the last user message unless the last user message
   already contains `CONTEXT CHECKPOINT COMPACTION` (case-insensitive; only the LAST
   user message is checked, `_looks_like_compaction_prompt` 16422-16440); no tools,
   tool_choice, parallel_tool_calls.

   `COMPACTION_PROMPT` (16134-16144), exact text:
   ```
   You are performing a CONTEXT CHECKPOINT COMPACTION. Create a handoff summary for another LLM that will resume the task.

   Include:
   - Current progress and key decisions made
   - Important context, constraints, or user preferences
   - What remains to be done (clear next steps)
   - Any critical data, examples, or references needed to continue

   Be concise, structured, and focused on helping the next LLM seamlessly continue the work. Return only the summary.
   ```
3. Response (`_responses_compaction_response`, 21608-21700): aggregate the upstream
   stream (`aggregate_stream`) regardless of `stream`. Errors:
   * aggregation exception -> `502 "upstream stream error: <exc>"` (OpenAI envelope).
   * `summary = str(message.content or "").strip()` empty ->
     `502 "compaction produced no summary (the model returned no text); retry"`.
   Item: `{"id": "cmp_<hex>", "type": "compaction", "encrypted_content": _encode_compaction_summary(summary)}`.

   `_encode_compaction_summary(text)` (16162-16166) =
   `base64(json.dumps({"v":1,"summary":text}, ensure_ascii=False, separators=(",",":")).utf8)`
   standard alphabet with padding. E.g. `"the summary"` ->
   `eyJ2IjoxLCJzdW1tYXJ5IjoidGhlIHN1bW1hcnkifQ==`.
   `_decode_compaction_summary(blob)` (16169-16183): strip; empty -> ""; add `=` padding,
   base64-decode (standard alphabet, Python's non-strict decoder discards invalid chars),
   JSON-parse; if dict with str `summary` return it; on any failure return the stripped
   input text unchanged.

   `usage = _responses_usage(chat.usage) or {}`.
   * Non-stream: `200` `{"id":"resp_<hex>","object":"response","created_at","status":"completed","model","output":[item],"output_text":"","metadata":{},"usage": usage}`
     (no tools/tool_choice/parallel_tool_calls echo).
   * Stream: same SSE headers; exactly four frames (`sequence_number` 1..4), no
     `output_item.added` (Codex's collector reads only `output_item.done`; an `added` frame
     would hand it an empty summary):
     1. `response.created` `{"response": base + status in_progress, output []}`
     2. `response.in_progress` (same)
     3. `response.output_item.done` `{"output_index":0,"item": item}`
     4. `response.completed` `{"response": base + status completed + usage (or {"input_tokens":0,"output_tokens":0} if empty)}`

     `base` key order: `id, object, created_at, model, output, output_text, metadata`,
     then `status` (and `usage`) appended - i.e. `status` comes after `metadata` here,
     unlike normal responses (fixture F10).

No background/async compaction job is kept; `/responses/compact` is synchronous.

---

## 4. Anthropic Messages API

### 4.1 Request -> chat body (`messages_to_chat`, 16859-16936)

Validation (each raises `ValueError` -> `_anthropic_error(400, str(exc), "invalid_request_error")`):
* body not a dict: `"request body must be a JSON object"` (unreachable via HTTP).
* `model` (after `_anthropic_text`, strip) empty: `"model is required"`.
* `messages` not a list: `"messages must be a list"`; empty: `"messages must not be empty"`.
* a message role (stripped, default `"user"`) not in user/assistant/system/developer:
  `"messages[%d].role %r must be user or assistant; pass a system prompt in the top-level system field"`
  (`%r` = Python repr, e.g. `'tool'`).

`_anthropic_text(v)` (16551-16558): None -> ""; str -> itself; int/float/bool -> `str()`
(Python spelling: `True`, `1.5`); anything else -> "".

Field mapping (chat key order: `model`, `messages` (placeholder, replaced at end),
`temperature`, `top_p`, `max_tokens`, `stream`, `stop`, `user`, `tools`, `tool_choice`,
`parallel_tool_calls`, `reasoning_effort`):

| Anthropic | Chat | Rule |
|---|---|---|
| `model` | `model` | stripped text |
| `temperature`, `top_p` | same | when not None |
| `max_tokens` | `max_tokens` | when not None; else catalog default (same as Responses) |
| `stream` | `stream` | `bool(stream)` when not None (determines handler mode) |
| `stop_sequences` | `stop` | when a non-empty list (copied verbatim) |
| `metadata.user_id` | `user` | when metadata is a dict and user_id not None |
| `tools` | `tools` | `anthropic_tools_to_chat` below |
| `tool_choice` | `tool_choice` | `_anthropic_tool_choice` below |
| `tool_choice.disable_parallel_tool_use == true` | `parallel_tool_calls: false` | only when tool_choice is a dict |
| `output_config.effort` / `thinking` | `reasoning_effort` | `anthropic_effort_from_messages` (5.2) |
| `system` | first system message | `_anthropic_system_text`; plus skipped-tools note |
| `top_k`, `thinking`, `output_config` (rest), `context_management`, `cache_control` (anywhere), `service_tier`, `container`, `mcp_servers` | - | dropped (`cache_control` is silently ignored, hence the opt-in gate) |

`_anthropic_system_text(v)` (16707-16719): str -> itself; list -> `"\n".join` of truthy
`_anthropic_text(item.text)` of dict items; else "".

Skipped tools: if any were skipped, append to system:
`"These Anthropic server tools are not available here: " + ", ".join(names) + ". Do not call them."`
joined to the existing system text with `"\n"` (or used alone). System message emitted
only if the result is non-empty.

`anthropic_tools_to_chat(tools)` (16741-16765): for each dict:
* has a dict `function` -> passed through unchanged (OpenAI-shaped tool).
* name = `_anthropic_text(name or type)`; schema = `input_schema`, else `parameters`;
  if schema is None -> skipped (name added to skipped list if non-empty). This is how
  server tools like `{"type":"web_search_20250305","name":"web_search"}`,
  `bash_20250124`, `text_editor_*` are excluded.
* else `{"type":"function","function":{"name", ["description" if not None], "parameters": schema}}`.
  `cache_control`, `strict`, `input_examples` dropped.
* `tools` key omitted when the result is empty.

`_anthropic_tool_choice(v)` (16722-16738): str: lowercase-trimmed, map
`auto->auto, any->required, none->none, required->required`, other strings passed
through lowercased; dict by `type`: `auto` -> `"auto"`, `any` -> `"required"`, `none` ->
`"none"`, `tool` -> `{"type":"function","function":{"name": name}}`; other -> None
(field omitted). The object form is later flattened by `normalize_tool_choice` to the bare
name string.

Messages loop (16909-16935), per item (non-dicts skipped):
* role `system`/`developer`: text = `_anthropic_system_text(content)` or
  `_anthropic_content_to_text(content)`; append `{"role":"system","content":text}` if
  non-empty, AT ITS POSITION (Claude Code >= 2.1.286 mid-conversation system).
* content str -> `{"role", "content"}` (even empty string).
* content list -> `_anthropic_blocks_to_messages(role, content)`.
* other non-None -> `_anthropic_text`, appended if non-empty. None -> nothing.

`_anthropic_blocks_to_messages(role, blocks)` (16799-16856) - collects then emits
`tool` messages FIRST, then at most one message of `role`:
* `thinking`, `redacted_thinking` -> dropped (never forwarded upstream).
* `tool_use`, `server_tool_use` -> `tool_calls` entry
  `{"id": text(id), "type":"function","function":{"name": text(name), "arguments": _anthropic_json_text(input)}}`;
  `_anthropic_json_text` (16561-16569): None -> `"{}"`, str -> itself, else compact JSON
  (`separators=(",",":")`, ensure_ascii False), failure -> `"{}"`.
* `tool_result` -> `{"role":"tool","tool_call_id": text(tool_use_id), "content": _anthropic_tool_result_content(content, is_error)}`.
* `image` -> `_anthropic_image_part`; skipped if url empty; sets `has_non_text`.
  `_anthropic_image_part` (16668-16681): source `base64` -> `data:<media_type or image/png>;base64,<data>`;
  `url` -> url; `file` -> `file_id or url`; else "". Result `{"type":"image_url","image_url":{"url"}}`.
* `document` -> text part `_anthropic_document_text` (16684-16704: source `text` -> data;
  `content` -> flattened; `base64` -> `"[document <media_type|application/octet-stream> omitted]"`;
  `url` -> url; `file` -> file_id or url; prefixed by `title + "\n"` when title), sets
  `has_non_text`.
* anything else (`text`, `search_result`, unknown) -> `_anthropic_block_text` (16616-16623:
  `text` key -> text; else `content` key -> recursive `_anthropic_content_to_text`;
  else ""); appended to texts AND as a text part (even if empty).
* Emit: if no tool_calls, no texts and no non-text parts -> only the tool messages.
  Else message `{"role": role or "user", ["tool_calls"], "content": ...}` where content
  = the parts list if `has_non_text`, else `"".join(texts)` (texts joined WITHOUT
  separator), else `""`. Key order: `role`, `tool_calls`, `content`.

Consequence: a user turn `[tool_result, text]` becomes `[tool msg, user msg]`; an
assistant turn `[thinking, text, tool_use]` becomes one assistant message with `content`
and `tool_calls`.

`_anthropic_tool_result_content(value, is_error)` (16626-16665):
* not a list -> `_anthropic_content_to_text(value, is_error)`: str as-is; dict ->
  block text; other scalar text; prefixed `"error: "` when `is_error is True`.
* list: str items -> text part; `image` -> image part if url else text part
  `"[image omitted: no data]"`; `document` -> text part; others -> text part if text
  non-empty. If no image: return `"".join(texts)` (prefixed `"error: "` if error).
  If an image is present: return the parts list, with `{"type":"text","text":"error:"}`
  inserted first when error.

`_anthropic_content_to_text` for lists (16593-16606) renders images as their data URL
text (used only for non-list tool_result paths and system/other content).

### 4.2 Handler (`_handle_messages`, 21856-21955)

* Translation error -> 400 as above.
* `model = chat.model or "unknown"`; `want_stream = bool(chat.stream)`;
  `session_key = extract_session_key(headers, chat_req)` (note: chat body, which has no
  `metadata`, so only headers work - `metadata.user_id` became `user`).
* Realm/banned/key-model checks -> Anthropic 400 `invalid_request_error`.
* Upstream open errors -> Anthropic envelope: `PinnedAccountError` -> its status, type
  derived from status; `ContentRejected` -> 403 `permission_error`
  `"upstream 403: <detail>"`; `LocalNetworkError` -> 503 `api_error` (explicit type,
  overriding `overloaded_error`); `TLSVerifyError` -> 502 `api_error`; `RateLimited` ->
  429 `rate_limit_error` with `Retry-After: <wait>` header (`_anthropic_rate_limited`,
  18363-18381; `wait = max(1, int(exc.wait or 60))`; text `exc.message` or
  `"upstream rate limit reached for this model; retry in %ds"` + `" - <detail[:200]>"`);
  `HTTPError` -> its code `"upstream <code>: <detail>"`; other -> 503 if message
  (lowercased) contains `"no account"` or `"no enabled account"` (+ dashboard hint) else
  502, type derived from status.

### 4.3 Non-stream response (`_messages_nonstream_response` 21957-21976, `chat_to_messages` 16995-17039)

`aggregate_stream` then `chat_to_messages`; any exception ->
`_anthropic_error(502, "upstream stream error: <exc>")` (type `api_error`).
**There is no web-tool loop and no reasoning-only rejection on this route.**

```
{"id": _anthropic_message_id(chat.id),   // "msg_" + upstream id unless it already starts with "msg_"; new msg_<hex> if empty
 "type": "message", "role": "assistant",
 "model": text(chat.model),               // the UPSTREAM-reported model (aggregate_stream keeps chunk.model)
 "content": [blocks],
 "stop_reason": _anthropic_stop_reason(finish_reason),
 "stop_sequence": null,
 "usage": _anthropic_usage(chat.usage)}
```

Content blocks in order: `{"type":"thinking","thinking": reasoning_content}` (if
non-empty; **no `signature` field**), `{"type":"text","text": content}` (if non-empty;
list content is joined block texts), then one `{"type":"tool_use","id": id or
"toolu_<hex>","name","input": _anthropic_parse_tool_input(arguments)}` per tool call.
`_anthropic_parse_tool_input` (16572-16587): None/"" -> `{}`; dict -> itself; str ->
JSON-parse, dict -> it, other JSON -> `{"value": parsed}`, parse error -> `{"raw": str}`;
other -> `{"value": v}`. Empty content -> `content: []`.

`_anthropic_stop_reason` (16984-16992): `length` -> `max_tokens`; `tool_calls`,
`function_call` -> `tool_use`; `content_filter`, `refusal` -> `refusal`; everything else
(incl. `stop`, empty, None) -> `end_turn`. `stop_sequence` is always null, and
`stop_sequence` stop reason is never produced.

`_anthropic_usage(u)` (16939-16974), key order
`input_tokens, cache_creation_input_tokens, cache_read_input_tokens, output_tokens,
[service_tier]`:
* `cache_read = prompt_cache_hit_tokens` if that key is present (not None), else
  `_best_cached_tokens(u)` (806+: first positive of `prompt_tokens_details.cached_tokens`,
  `prompt_cache_hit_tokens`, `cache_read_input_tokens`, `cached_tokens`,
  `input_tokens_details.cached_tokens`, `completion_tokens_details.cached_tokens`).
* `cache_creation = prompt_cache_write_tokens or 0`.
* `input_tokens = max(0, prompt_tokens - cache_read - cache_creation)`.
* `output_tokens = completion_tokens`.
* `service_tier` copied when truthy.
* non-dict usage -> all zeros. Values coerced with `int(v or 0)`, failures -> 0.

### 4.4 Stream (`_messages_stream_response` 21978-22029, `stream_messages_events` 17056-17253)

HTTP: same headers as the Responses stream. Heartbeat `: keepalive\n\n` every 15s idle.
**No `ping` events are ever emitted** (the Anthropic `ping` event does not exist in this
implementation; keepalive is an SSE comment).

Frame format (`anthropic_sse_frame`, 16544-16548): payload dict copied, then `type` set
(so `type` is the LAST key), compact JSON:
```
event: <name>\ndata: <json>\n\n
```

Input reading: `iter_sse_lines` + `strip_data_prefix`; `[DONE]` stops reading; bad JSON
skipped. Unlike the Responses path, only `choices[0]` is used.

State: `started`, `thinking_open/idx`, `text_open/idx`, `next` (block index counter
starting 0), `tools` by chat `index`, `tool_order`, `usage`, `stop = "end_turn"`.

Per chunk:
1. `chunk.error` is a dict -> emit
   `event: error` `{"error":{"type":"api_error","message": text(err.message or "upstream error")},"type":"error"}`
   and END the generator (no message_stop, no message_delta). If this is the first chunk
   no `message_start` is sent either (fixture F13).
2. First valid chunk -> `message_start`:
   `{"message":{"id": _anthropic_message_id(chunk.id),"type":"message","role":"assistant","content":[],"model": chunk.model or request model,"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}},"type":"message_start"}`.
   (`message_start.usage` is always zeros; real usage arrives only in `message_delta`.)
3. If `chunk.usage` is a dict, store it (last one wins; no total_tokens comparison).
4. No choices -> next chunk.
5. `delta.reasoning_content` non-empty str: open thinking block if not open
   (`content_block_start {"index": n, "content_block": {"type":"thinking","thinking":""}}`)
   and emit `content_block_delta {"index", "delta": {"type":"thinking_delta","thinking": s}}`.
   Never emits `signature_delta`. Once a thinking block was closed, a later reasoning delta
   opens a NEW thinking block at the next index.
6. `delta.content` non-empty str: if text not open: close thinking if open
   (`content_block_stop {"index"}`), open text
   (`content_block_start {"index","content_block":{"type":"text","text":""}}`); emit
   `content_block_delta {"index","delta":{"type":"text_delta","text": s}}`.
   Reasoning arriving while text is open goes to a thinking block that is opened
   without closing the text block (interleaving possible; see Ambiguities).
7. `delta.tool_calls`: for each dict call, `idx = int(call.index or 0)` (bad -> 0); create
   entry on first sight; update `id` / `name` when truthy; `args = function.arguments`
   (None -> ""). When the entry is not opened and (id or name or args) is truthy: close
   open thinking and text blocks, assign block index, default id `"toolu_" + str(idx)`
   (e.g. `toolu_0`, NOT random), emit
   `content_block_start {"index","content_block":{"type":"tool_use","id","name","input":{}}}`.
   The `name` is whatever is known at that moment (may be "" if the name arrives later).
   If opened and args truthy -> `content_block_delta {"index","delta":{"type":"input_json_delta","partial_json": args}}`
   (the first chunk's args ARE emitted, unlike the Responses path). Multiple tool blocks
   can be open simultaneously; text after a tool call opens a new text block without
   closing tool blocks.
8. `finish_reason` truthy -> `stop = _anthropic_stop_reason(finish)`.

End of input (stream EOF or `[DONE]`):
1. If no chunk ever started the message, emit `message_start` with id `msg_<hex>` and the
   request model.
2. Close thinking (if open), text (if open), then every opened tool block in first-seen
   order (`content_block_stop {"index"}`).
3. `holder.usage = raw upstream usage` (for usage recording).
4. `message_delta {"delta":{"stop_reason": stop,"stop_sequence":null},"usage": _anthropic_usage(usage)}`
   (full 4-key usage object).
5. `message_stop {}` -> `{"type":"message_stop"}`.

No truncated-tool-call filtering, no DSML parsing, no empty-reasoning check, no usage
estimation on this path (a stream without usage reports zeros).

Handler-level errors (21998-22018): client disconnect -> stop, record `client_aborted`;
other exception during streaming -> write
`event: error\ndata: {"error":{"type":"api_error","message":"<str(exc)>"},"type":"error"}\n\n`
(raw exception text, no prefix) and stop.

### 4.5 count_tokens (`_handle_messages_count_tokens` 21842-21854)

* Same auth, gate, body parsing and per-key concurrency as `/v1/messages`; no chat slot;
  never contacts the upstream; no usage row.
* `chat = messages_to_chat(payload)` (validation errors -> 400 Anthropic envelope).
* Response `200 {"input_tokens": _anthropic_estimate_chat_tokens(chat)}`.
* `_anthropic_estimate_chat_tokens` (17042-17053): collect strings: for each message
  `_anthropic_json_text(msg.content)` (string as-is; list -> compact JSON) and
  `_anthropic_json_text(tool_call)` for each tool call; for each tool
  `_anthropic_json_text(tool)`; sum `estimate_tokens` over non-empty strings. The system
  prompt that `build_upstream_body` would inject is NOT counted.
* `estimate_tokens(text)` (15285-15292): `cjk` = count of chars in
  `[\u4e00-\u9fff\u3400-\u4dbf]`; `other = len - cjk`;
  result `cjk + max(1, int(other / 3.6))` (0 for empty). Note `max(1, ...)` means any
  non-empty string counts at least 1 even if all CJK (e.g. "你好" -> 2 + 1 = 3).
  `len` is Python code-point length (not UTF-16, not bytes).
  Examples: `"hello world"` -> 3; `"你好 hello"` -> 2 + int(6/3.6)=1 -> 3.

---

## 5. Reasoning effort mapping

### 5.1 Responses

`reasoning.effort` (or top-level `reasoning_effort`) copied as `reasoning_effort`.
Downstream in `build_upstream_body` (13145-13239):
1. `map_reasoning_effort(body, model)` (13332-13391), only when settings
   `reasoning_effort_map` is not explicitly `false` (wb_settings.py:1748-1756, default ON):
   reads `reasoning_effort` (or `reasoningEffort`) when it is a non-blank str; looks up the
   catalog `reasoning` block; if empty -> unchanged.
   * value is off-ish (`off`, `none`, `disabled`, `disable`, case-insensitive): kept
     unless `canDisableThinking is False`, in which case replaced by `_fallback_effort`.
   * catalog has `supportedEfforts`: kept if it matches case-insensitively (original
     spelling kept, e.g. `HIGH`), else replaced by `_fallback_effort`.
   * else catalog pins `effort`: kept if equal (case-insensitive) else replaced.
   * else unchanged.
   `_fallback_effort` (13310-13329): `defaultEffort`, else (if no supportedEfforts) the
   pinned `effort`, else first `supportedEfforts` entry.
   Fixtures (`tests/_test_reasoning_effort_map.py`): levels `{supportedEfforts:[low,medium,high], defaultEffort:medium, canDisableThinking:false}`:
   `low`->`low`, `HIGH`->`HIGH`, `xhigh`->`medium`, `none`->`medium`, `off`->`medium`;
   `{supportedEfforts:[low,high], defaultEffort:high, canDisableThinking:true}`: `none`->`none`;
   pinned `{effort: high}`: `high`->`high`, `low`->`high`; no catalog: `xhigh`->`xhigh`;
   non-string `3` unchanged; `"  "` unchanged; setting off: `xhigh`->`xhigh`.
2. DeepSeek models (`model.lower().startswith("deepseek")`): unless `thinking.type ==
   "disabled"` or effort is `none`: add `thinking: {"type":"enabled"}` if absent, and if no
   effort, inject per `WB_DEFAULT_REASONING_EFFORT` (13394-13409): `off|none|0|false|no`
   -> none injected; `catalog|model|default` -> catalog `defaultEffort` only; otherwise
   (default) catalog default else `"high"`.

### 5.2 Anthropic (`anthropic_effort_from_messages`, 16768-16796)

1. `output_config.effort` (non-blank text, stripped) wins.
2. else `thinking` must be a dict: `type` lowercased: `disabled` -> `"none"`; not
   `enabled` (e.g. `adaptive`) -> no effort; `enabled` with `budget_tokens`
   (`int(budget or 0)`, unparsable -> None) <= 0 -> no effort;
   `>= 32000` -> `xhigh`; `>= 16000` -> `high`; `>= 8000` -> `medium`; else `low`.

The `thinking` object itself is NOT forwarded; `build_upstream_body` then injects
`thinking:{"type":"enabled"}` for DeepSeek unless effort is `none` (verified:
`thinking:{type:disabled}` + `deepseek-v4.1-flash` -> wire has `reasoning_effort:"none"`
and no `thinking` key).

---

## 6. Golden fixtures

All produced by running baseline functions (ids normalised to `<prefix><hex>`,
`time.time()` pinned to `1700000000`). Responses SSE fixtures show only the `data:`
JSON per frame (each frame is `event: <type>\ndata: <json>\n\n`, with `<type>` equal to
the JSON `type`). Fixture sources: tests named in each heading.

### F1. Responses history -> chat body (`_test_custom_tools.py` [2]/[2.5], `_test_orphan_function_call_output.py`)

Input:
```json
{"model": "m", "instructions": "be terse", "input": [
  {"role": "developer", "content": "dev note"},
  {"role": "user", "content": [{"type": "input_text", "text": "edit"},
                               {"type": "input_image", "image_url": "data:image/png;base64,AA"}]},
  {"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "plan"}]},
  {"type": "function_call", "call_id": "call_1", "name": "shell", "arguments": "{\"cmd\":\"ls\"}"},
  {"type": "custom_tool_call", "call_id": "call_2", "name": "apply_patch", "input": "*** Begin Patch"},
  {"type": "function_call_output", "call_id": "call_1", "output": "a.txt"},
  {"type": "custom_tool_call_output", "call_id": "call_2", "output": "Done!"},
  {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "ok"}]},
  {"type": "agent_message", "content": [{"type": "text", "text": "from sub"}]}],
 "tools": [{"type": "function", "name": "shell", "parameters": {"type": "object"}}],
 "tool_choice": "auto", "parallel_tool_calls": false, "max_output_tokens": 100,
 "reasoning": {"effort": "low"}, "temperature": 0.2, "store": true, "metadata": {"a": 1}}
```
`responses_to_chat` output (before `build_upstream_body`):
```json
{"model": "m",
 "messages": [
  {"role": "system", "content": "be terse"},
  {"role": "system", "content": "dev note"},
  {"role": "user", "content": [{"type": "text", "text": "edit"},
                               {"type": "image_url", "image_url": {"url": "data:image/png;base64,AA"}}]},
  {"role": "assistant", "content": "",
   "tool_calls": [
     {"id": "call_1", "type": "function", "function": {"name": "shell", "arguments": "{\"cmd\":\"ls\"}"}},
     {"id": "call_2", "type": "function", "function": {"name": "apply_patch", "arguments": "{\"input\": \"*** Begin Patch\"}"}}],
   "reasoning_content": "plan"},
  {"role": "tool", "tool_call_id": "call_1", "content": "a.txt"},
  {"role": "tool", "tool_call_id": "call_2", "content": "Done!"},
  {"role": "assistant", "content": "ok"},
  {"role": "user", "content": "[Message from another task - treat this as a user instruction]\n\nfrom sub"}],
 "temperature": 0.2, "max_tokens": 100, "reasoning_effort": "low",
 "tools": [{"type": "function", "name": "shell", "parameters": {"type": "object"}}],
 "_namespace_map": {}, "tool_choice": "auto", "parallel_tool_calls": false}
```
(`store` / `metadata` dropped; reasoning was pending, then attached to the assistant
message created by the following `function_call`.)

Orphan outputs (`_test_orphan_function_call_output.py`):
```
input [{"type":"function_call_output","output":[{"type":"output_text","text":"done"}]}]
  -> [{"role":"user","content":"[Message from another task - treat this as a user instruction]\n\ndone"}]
input [{"type":"function_call_output","output":[{"type":"input_image","image_url":"data:image/png;base64,iVBORw0KGgo="}]}]
  -> [{"role":"user","content":[{"type":"text","text":"[Message from another task - treat this as a user instruction]"},
                                {"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw0KGgo="}}]}]
input [{"type":"function_call","call_id":"call_1","name":"shell","arguments":"{}"},
       {"type":"function_call_output","call_id":"call_1","output":[{"type":"output_text","text":"done"}]}]
  -> roles ["assistant","tool"]
input [{"type":"function_call_output","output":""}]            -> [{"role":"tool","tool_call_id":"","content":""}]
input [{"type":"function_call_output","call_id":"c","output":null}]  -> [{"role":"tool","tool_call_id":"c","content":"None"}]
input [{"type":"function_call_output","call_id":"c","output":{"a":1}}] -> [{"role":"tool","tool_call_id":"c","content":"{\"a\": 1}"}]
```
Reasoning edge cases (`_responses_input_to_messages`):
```
[assistant "a", reasoning{summary:[r1]}, reasoning{content:"r2"}] -> [{"role":"assistant","content":"a","reasoning_content":"r2"}]   (overwrite)
[reasoning{summary:"pre"}, message(assistant,""), function_call{name:"x"}]
   -> [{"role":"assistant","content":"","tool_calls":[{"id":"","type":"function","function":{"name":"x","arguments":"{}"}}],"reasoning_content":"pre"}]
[message(user,[input_text a, input_text b])] -> [{"role":"user","content":"a\nb"}]
[{"type":"input_image","image_url":"x"}]     -> []   (unknown top-level item, logged)
```

### F2. Custom tool downgrade (`_test_custom_tools.py` [1], `_test_namespace_tools.py` [3])

Input tool:
```json
{"type": "custom", "name": "apply_patch", "description": "Use the patch format to edit files",
 "format": {"type": "grammar", "syntax": "lark", "definition": "start: /.*/s"}}
```
Chat body tool:
```json
{"type": "function", "name": "apply_patch",
 "description": "Use the patch format to edit files\n\nThis is a freeform tool. Put the COMPLETE raw payload into the single 'input' string parameter, verbatim. Do not wrap it in JSON, do not wrap it in markdown code fences, do not add commentary.\n\nGrammar:\nstart: /.*/s",
 "parameters": {"type": "object",
   "properties": {"input": {"type": "string", "description": "Complete raw payload for this tool, verbatim."}},
   "required": ["input"]}}
```
An ordinary function tool in the same list passes through as the identical object.

### F3. Namespace expansion and resolution (`_test_namespace_tools.py`)

Input tools (in order): `CUSTOM apply_patch`, `{"type":"web_search"}`,
`function get_weather`, and
```json
[{"type": "namespace", "name": "node_repl", "tools": [{"name": "js", "description": "run javascript", "parameters": {"type": "object", "properties": {"code": {"type": "string"}}}}]},
 {"type": "namespace", "name": "multi_agent_v1", "children": [{"name": "spawn_agent", "parameters": {"type": "object", "properties": {"message": {"type": "string"}}}}]},
 {"type": "namespace", "name": "codex_apps__github", "functions": [{"type": "function", "name": "_get_profile", "description": "github profile", "parameters": {"type": "object", "properties": {}}}]}]
```
`expand_namespace_tools` -> flat names in order
`["apply_patch", None (web_search), "get_weather", "js", "spawn_agent", "_get_profile"]`,
mapping `{"js": "node_repl", "spawn_agent": "multi_agent_v1", "_get_profile": "codex_apps__github"}`.
Flattened `js` = `{"type":"function","name":"js","description":"run javascript","parameters":{...}}`;
`spawn_agent` gets `"description": ""`. A top-level `js` listed before the namespaces
wins and appears once.

`resolve_namespaced_name` with mapping `{"js":"node_repl","line":"codex_apps__github"}`:
```
"js"                        -> ("js", "node_repl")
"node_repl__js"             -> ("js", "node_repl")
"node_repl::js"             -> ("js", "node_repl")
"codex_apps__github__line"  -> ("line", "codex_apps__github")
"codex_apps__github::line"  -> ("line", "codex_apps__github")
"js2"                       -> ("js2", "")
```
`apply_namespace_to_calls` on
`[{function_call js c1}, {custom_tool_call apply_patch c2}, {function_call node_repl__js c3}, {message}]`
with `{"js":"node_repl"}` -> fixed=2; c1 gets `namespace:"node_repl"`; c2 untouched;
c3 renamed to `js` + `namespace:"node_repl"`. An item already carrying `namespace` is not
counted.

Nested custom (`_test_custom_tools.py` [13]-[15]):
`{"type":"namespace","name":"functions","tools":[{"type":"custom","name":"exec",...},{"type":"function","name":"wait",...}]}`
-> `custom_tool_names` = `{"exec"}` (also under `children`/`functions`); `_namespace_map`
= `{"exec":"functions","wait":"functions"}`; `exec` is downgraded to a function in chat
tools. Return trip: chat call `exec` args `{"input": "pwd"}` -> `custom_tool_call`,
`namespace:"functions"`, `input:"pwd"`; chat call `wait` args `{"ms":100}` ->
`function_call`, `namespace:"functions"`, arguments untouched. Same when the namespace
arrives only inside an `{"type":"additional_tools","tools":[...]}` input item (and that
item produces no message).

### F4. Non-stream chat -> Responses (`_test_custom_tools.py` [4]/[5], extended)

Custom: chat `tool_calls:[{"id":"call_7","function":{"name":"apply_patch","arguments":"{\"input\": \"*** Begin Patch\\n+ok\\n*** End Patch\"}"}}]`,
`custom_names={"apply_patch"}` -> `output[0] = {"id":"ctc_<hex>","type":"custom_tool_call","status":"completed","call_id":"call_7","name":"apply_patch","input":"*** Begin Patch\n+ok\n*** End Patch"}`
(only item; no message item because text is empty and output is non-empty).

Function + reasoning + text + length:
```json
{"choices": [{"finish_reason": "length", "message": {"role": "assistant", "content": "partial", "reasoning_content": "r",
  "tool_calls": [{"id": "call_8", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"Beijing\"}"}}]}}],
 "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7, "prompt_cache_hit_tokens": 1}}
```
->
```json
{"id": "resp_<hex>", "object": "response", "created_at": 1700000000, "status": "incomplete", "model": "m",
 "output": [
  {"id": "rs_<hex>", "type": "reasoning", "status": "completed", "summary": [{"type": "summary_text", "text": "r"}]},
  {"id": "fc_<hex>", "type": "function_call", "status": "completed", "call_id": "call_8", "name": "get_weather", "arguments": "{\"city\":\"Beijing\"}"},
  {"id": "msg_<hex>", "type": "message", "status": "completed", "role": "assistant",
   "content": [{"type": "output_text", "text": "partial", "annotations": []}]}],
 "output_text": "partial", "metadata": {}, "parallel_tool_calls": true, "tool_choice": "auto", "tools": [],
 "usage": {"input_tokens": 5, "input_tokens_details": {"cached_tokens": 1}, "output_tokens": 2,
           "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 7},
 "incomplete_details": {"reason": "max_output_tokens"}}
```
Reasoning-only (`_test_upstream_repairs.py`): chat
`{"choices":[{"message":{"role":"assistant","content":"","reasoning_content":"thinking"},"finish_reason":"stop"}],"usage":{}}`
-> output `[reasoning]` only (no message item: output is already non-empty; `usage`
key absent because `{}` is falsy), `reasoning_only_response` = true -> HTTP 502
`"upstream returned reasoning without output text; retry"`. Content `"answer"` -> false.

### F5. Responses stream: reasoning then text (shape of `_test_stream_single_id.py` chunks)

Upstream chunks (followed by `data: [DONE]`):
```
{"id":"c1","model":"m","choices":[{"index":0,"delta":{"reasoning_content":"think"}}]}
{"id":"c1","model":"m","choices":[{"index":0,"delta":{"content":"Hello"}}]}
{"id":"c1","model":"m","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":3,"total_tokens":13,"prompt_tokens_details":{"cached_tokens":4},"completion_tokens_details":{"reasoning_tokens":1}}}
```
holder `request_meta = {"tools":[],"tool_choice":"auto","parallel_tool_calls":true}`. Emitted data lines:
```
{"type": "response.created", "sequence_number": 1, "response": {"id": "resp_<hex>", "object": "response", "created_at": 1700000000, "status": "in_progress", "model": "m", "output": [], "output_text": "", "parallel_tool_calls": true, "tool_choice": "auto", "tools": [], "metadata": {}}}
{"type": "response.in_progress", "sequence_number": 2, "response": {...same as above...}}
{"type": "response.output_item.added", "sequence_number": 3, "output_index": 0, "item": {"id": "rs_<hex>", "type": "reasoning", "status": "in_progress", "summary": [{"type": "summary_text", "text": ""}]}}
{"type": "response.reasoning_summary_part.added", "sequence_number": 4, "item_id": "rs_<hex>", "output_index": 0, "summary_index": 0, "part": {"type": "summary_text", "text": ""}}
{"type": "response.reasoning_summary_text.delta", "sequence_number": 5, "item_id": "rs_<hex>", "output_index": 0, "summary_index": 0, "delta": "think"}
{"type": "response.reasoning_summary_text.done", "sequence_number": 6, "item_id": "rs_<hex>", "output_index": 0, "summary_index": 0, "text": "think"}
{"type": "response.reasoning_summary_part.done", "sequence_number": 7, "item_id": "rs_<hex>", "output_index": 0, "summary_index": 0, "part": {"type": "summary_text", "text": "think"}}
{"type": "response.output_item.done", "sequence_number": 8, "output_index": 0, "item": {"id": "rs_<hex>", "type": "reasoning", "status": "completed", "summary": [{"type": "summary_text", "text": "think"}]}}
{"type": "response.output_item.added", "sequence_number": 9, "output_index": 1, "item": {"id": "msg_<hex>", "type": "message", "status": "in_progress", "role": "assistant", "content": []}}
{"type": "response.content_part.added", "sequence_number": 10, "item_id": "msg_<hex>", "output_index": 1, "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}}
{"type": "response.output_text.delta", "sequence_number": 11, "item_id": "msg_<hex>", "output_index": 1, "content_index": 0, "delta": "Hello"}
{"type": "response.output_text.done", "sequence_number": 12, "item_id": "msg_<hex>", "output_index": 1, "content_index": 0, "text": "Hello"}
{"type": "response.content_part.done", "sequence_number": 13, "item_id": "msg_<hex>", "output_index": 1, "content_index": 0, "part": {"type": "output_text", "text": "Hello", "annotations": []}}
{"type": "response.output_item.done", "sequence_number": 14, "output_index": 1, "item": {"id": "msg_<hex>", "type": "message", "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": "Hello", "annotations": []}]}}
{"type": "response.completed", "sequence_number": 15, "response": {"id": "resp_<hex>", "object": "response", "created_at": 1700000000, "status": "completed", "model": "m", "output": [{"id": "rs_<hex>", "type": "reasoning", "status": "completed", "summary": [{"type": "summary_text", "text": "think"}]}, {"id": "msg_<hex>", "type": "message", "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": "Hello", "annotations": []}]}], "output_text": "Hello", "parallel_tool_calls": true, "tool_choice": "auto", "tools": [], "metadata": {}, "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3, "output_tokens_details": {"reasoning_tokens": 1}, "total_tokens": 13}}}
```
All `resp_`/`rs_`/`msg_` ids are the same value throughout one stream.

### F6. Responses stream: namespaced function call (`_test_namespace_tools.py` semantics)

Chunks (+ `[DONE]`), holder `namespace_map = {"js":"node_repl"}`:
```
{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_A","function":{"name":"js","arguments":""}}]}}]}
{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"code\":1}"}}]}}]}
{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}
```
Frames 3..7 (1-2 are created/in_progress as in F5):
```
{"type": "response.output_item.added", "sequence_number": 3, "output_index": 0, "item": {"id": "fc_<hex>", "status": "in_progress", "call_id": "call_A", "name": "js", "type": "function_call", "arguments": "", "namespace": "node_repl"}}
{"type": "response.function_call_arguments.delta", "sequence_number": 4, "output_index": 0, "item_id": "fc_<hex>", "call_id": "call_A", "delta": "{\"code\":1}"}
{"type": "response.function_call_arguments.done", "sequence_number": 5, "output_index": 0, "item_id": "fc_<hex>", "call_id": "call_A", "arguments": "{\"code\":1}"}
{"type": "response.output_item.done", "sequence_number": 6, "output_index": 0, "item": {"id": "fc_<hex>", "type": "function_call", "status": "completed", "call_id": "call_A", "name": "js", "arguments": "{\"code\":1}", "namespace": "node_repl"}}
{"type": "response.completed", "sequence_number": 7, "response": {"id": "resp_<hex>", "object": "response", "created_at": 1700000000, "status": "completed", "model": "m", "output": [{"id": "fc_<hex>", "type": "function_call", "status": "completed", "call_id": "call_A", "name": "js", "arguments": "{\"code\":1}", "namespace": "node_repl"}], "output_text": "", "parallel_tool_calls": true, "tool_choice": "auto", "tools": [], "metadata": {}}}
```
No message item; no `usage` (none upstream, no text to estimate from).

Arguments in the FIRST tool chunk (`{"index":0,"id":"call_1","function":{"name":"f","arguments":"{\"a\":1}"}}` with `finish_reason:"tool_calls"`, then `[DONE]`) produce
`output_item.added` (arguments `""`) followed directly by
`response.function_call_arguments.done` with `"arguments": "{\"a\":1}"` - no `.delta` frame.

### F7. Responses stream: custom tool (`_test_custom_tools.py` [6])

Chunks (no `[DONE]`), holder `custom_names={"apply_patch"}`:
```
{"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_9", "function": {"name": "apply_patch", "arguments": ""}}]}, "finish_reason": null}]}
{"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"input\":\"*** Begin"}}]}, "finish_reason": null}]}
{"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": " Patch\\n+hi\\n*** End Patch\"}"}}]}, "finish_reason": null}]}
{"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}
```
Frames 3..8:
```
{"type": "response.output_item.added", "sequence_number": 3, "output_index": 0, "item": {"id": "ctc_<hex>", "status": "in_progress", "call_id": "call_9", "name": "apply_patch", "type": "custom_tool_call", "input": ""}}
{"type": "response.custom_tool_call_input.delta", "sequence_number": 4, "output_index": 0, "item_id": "ctc_<hex>", "call_id": "call_9", "delta": "{\"input\":\"*** Begin"}
{"type": "response.custom_tool_call_input.delta", "sequence_number": 5, "output_index": 0, "item_id": "ctc_<hex>", "call_id": "call_9", "delta": " Patch\\n+hi\\n*** End Patch\"}"}
{"type": "response.custom_tool_call_input.done", "sequence_number": 6, "output_index": 0, "item_id": "ctc_<hex>", "call_id": "call_9", "input": "*** Begin Patch\n+hi\n*** End Patch"}
{"type": "response.output_item.done", "sequence_number": 7, "output_index": 0, "item": {"id": "ctc_<hex>", "type": "custom_tool_call", "status": "completed", "call_id": "call_9", "name": "apply_patch", "input": "*** Begin Patch\n+hi\n*** End Patch"}}
{"type": "response.completed", "sequence_number": 8, "response": {... "status": "completed", "output": [<the custom_tool_call item>], "output_text": "", ...}}
```
(In the chunk and delta lines above, `\\n` is a JSON-escaped backslash followed by `n`
inside the JSON-encoded arguments string; after the args string is JSON-parsed the
`input` contains real newlines.) Missing `[DONE]` does not matter because the arguments
parse as JSON. No `response.function_call_arguments.*` events appear. An ordinary
function in the same setup (`get_weather`, `_test_custom_tools.py` [7]) uses
`function_call_arguments.delta/.done`, and every such event carries `item_id`.

### F8. Responses stream: truncated tool call (`_test_upstream_repairs.py`)

Chunk (no `[DONE]`):
`{"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_r", "function": {"name": "f", "arguments": "{\"a\":"}}]}, "finish_reason": "tool_calls"}]}`

Frames after created/in_progress:
```
3 response.output_item.added   {"output_index": 0, "item": {"id": "fc_<hex>", "status": "in_progress", "call_id": "call_r", "name": "f", "type": "function_call", "arguments": ""}}
4 response.output_item.added   {"output_index": 1, "item": {"id": "msg_<hex>", "type": "message", "status": "in_progress", "role": "assistant", "content": []}}
5 response.content_part.added  {"item_id": "msg_<hex>", "output_index": 1, "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}}
6 response.output_text.done    {"item_id": "msg_<hex>", "output_index": 1, "content_index": 0, "text": ""}
7 response.content_part.done   {..., "part": {"type": "output_text", "text": "", "annotations": []}}
8 response.output_item.done    {"output_index": 1, "item": {"id": "msg_<hex>", "type": "message", "status": "completed", "role": "assistant", "content": []}}
9 response.completed           {"response": {..., "status": "incomplete", "output": [<message item only>], "output_text": "", ...}}
```
No `function_call_arguments.done`; no `incomplete_details` (finish was not `length`).

### F9. Responses stream: reasoning only -> `response.failed`; empty; abort (`_test_upstream_repairs.py`)

Chunks (no `[DONE]`): `{"choices":[{"delta":{"reasoning_content":"thinking..."}}]}`,
`{"choices":[{"delta":{},"finish_reason":"stop"}]}`. Frames 3-8 are the reasoning-item
lifecycle (output_item.added, reasoning_summary_part.added, reasoning_summary_text.delta,
reasoning_summary_text.done, reasoning_summary_part.done, output_item.done), then:
```
{"type": "response.failed", "sequence_number": 9, "response": {"id": "resp_<hex>", "object": "response", "created_at": 1700000000, "status": "failed", "model": "m", "output": [{"id": "rs_<hex>", "type": "reasoning", "status": "completed", "summary": [{"type": "summary_text", "text": "thinking..."}]}], "output_text": "", "parallel_tool_calls": true, "tool_choice": "auto", "tools": [], "metadata": {}, "usage": {"input_tokens": 1, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 3, "output_tokens_details": {"reasoning_tokens": 3}, "total_tokens": 4}, "error": {"code": "empty_reasoning_only", "message": "upstream returned reasoning without output text; retry"}}}
```
No `response.completed`; `holder["empty_reasoning"] = True`. Usage is the estimate
(`estimate_tokens("thinking...")` = 3; prompt estimate = 1).

Empty stream (`{"choices":[{"delta":{},"finish_reason":"stop"}]}` + `[DONE]`): 8 frames -
created, in_progress, message `output_item.added` (index 0), `content_part.added`,
`output_text.done` (`""`), `content_part.done`, `output_item.done` (content `[]`),
`response.completed` (status `completed`). An upstream `{"error":{"message":"boom"}}`
chunk produces exactly the same 8 frames (the error is ignored).

Abort frame written by the handler on an upstream exception:
```
event: error
data: {"type": "error", "code": "upstream_stream_error", "message": "upstream stream aborted: x", "param": null, "sequence_number": 0}

```

### F10. Remote compaction (`_test_remote_compaction.py`)

Request:
```json
{"model": "deepseek-v4.1-flash", "input": [
  {"type": "message", "role": "user", "content": "do the thing"},
  {"type": "message", "role": "assistant", "content": "done"},
  {"type": "compaction_trigger"}],
 "tools": [{"type": "function", "name": "exec", "parameters": {"type": "object"}}]}
```
Chat body: `messages = [user "do the thing", assistant "done", user COMPACTION_PROMPT]`
plus `max_tokens: 128000`; no `tools`, `tool_choice`, `parallel_tool_calls`. If the last
user message before the trigger is already `COMPACTION_PROMPT`, it appears exactly once.
Without the trigger item the same request keeps `tools`.

Trigger shapes: `{"input":[{"type":"compaction_trigger"}]}`, `{"input":"compaction_trigger"}`,
`{"input":[{"type":"message"},{"type":"compaction_trigger"}]}` are compaction;
`{"input":[{"type":"message","role":"user"}]}`, `{"input":"hello"}`, `{}` are not.

Upstream text `handoff summary`, usage `{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}`. Stream reply (full bytes):
```
event: response.created
data: {"type": "response.created", "sequence_number": 1, "response": {"id": "resp_<hex>", "object": "response", "created_at": 1700000000, "model": "deepseek-v4.1-flash", "output": [], "output_text": "", "metadata": {}, "status": "in_progress"}}

event: response.in_progress
data: {"type": "response.in_progress", "sequence_number": 2, "response": {"id": "resp_<hex>", "object": "response", "created_at": 1700000000, "model": "deepseek-v4.1-flash", "output": [], "output_text": "", "metadata": {}, "status": "in_progress"}}

event: response.output_item.done
data: {"type": "response.output_item.done", "sequence_number": 3, "output_index": 0, "item": {"id": "cmp_<hex>", "type": "compaction", "encrypted_content": "eyJ2IjoxLCJzdW1tYXJ5IjoiaGFuZG9mZiBzdW1tYXJ5In0="}}

event: response.completed
data: {"type": "response.completed", "sequence_number": 4, "response": {"id": "resp_<hex>", "object": "response", "created_at": 1700000000, "model": "deepseek-v4.1-flash", "output": [{"id": "cmp_<hex>", "type": "compaction", "encrypted_content": "eyJ2IjoxLCJzdW1tYXJ5IjoiaGFuZG9mZiBzdW1tYXJ5In0="}], "output_text": "", "metadata": {}, "status": "completed", "usage": {"input_tokens": 100, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 20, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 120}}}

```
Non-stream reply:
```json
{"id": "resp_<hex>", "object": "response", "created_at": 1700000000, "status": "completed", "model": "deepseek-v4.1-flash", "output": [{"id": "cmp_<hex>", "type": "compaction", "encrypted_content": "eyJ2IjoxLCJzdW1tYXJ5IjoiaGFuZG9mZiBzdW1tYXJ5In0="}], "output_text": "", "metadata": {}, "usage": {"input_tokens": 100, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 20, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 120}}
```
Upstream text `"   "` -> HTTP 502, message contains `no summary`.

Codec: encode `"the summary"` -> `eyJ2IjoxLCJzdW1tYXJ5IjoidGhlIHN1bW1hcnkifQ==`, decode
round-trips; decode `"plain text"` -> `"plain text"`; decode `""` / `None` -> `""`.
Replay: input
`[{"type":"compaction","id":"cmp_1","encrypted_content":<encode("earlier work summary")>}, {"type":"message","role":"user","content":"continue"}]` ->
`[{"role":"system","content":"Summary of the earlier conversation (context checkpoint):\n\nearlier work summary"}, {"role":"user","content":"continue"}]`.

### F11. Anthropic request -> chat body (`_test_anthropic_messages.py` [1])

Input:
```json
{"model": "deepseek-v4.1-flash", "max_tokens": 128, "stream": true,
 "system": [{"type": "text", "text": "sys-a"}, {"type": "text", "text": "sys-b"}],
 "messages": [
  {"role": "user", "content": [{"type": "text", "text": "look"},
     {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aaa"}}]},
  {"role": "assistant", "content": [{"type": "thinking", "thinking": "secret", "signature": "sig"},
     {"type": "text", "text": "ok"},
     {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"cmd": "ls"}}]},
  {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "a.txt"},
     {"type": "text", "text": "next"}]}],
 "tools": [{"name": "Bash", "description": "run", "input_schema": {"type": "object"}},
           {"type": "web_search_20250305", "name": "web_search"}],
 "tool_choice": {"type": "tool", "name": "Bash", "disable_parallel_tool_use": true},
 "stop_sequences": ["END"], "metadata": {"user_id": "u"},
 "thinking": {"type": "adaptive", "display": "summarized"},
 "output_config": {"effort": "high", "format": {"type": "json_schema"}},
 "context_management": {"edits": []}, "top_k": 5}
```
Output (exact, key order preserved):
```json
{"model": "deepseek-v4.1-flash",
 "messages": [
  {"role": "system", "content": "sys-a\nsys-b\nThese Anthropic server tools are not available here: web_search. Do not call them."},
  {"role": "user", "content": [{"type": "text", "text": "look"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,aaa"}}]},
  {"role": "assistant", "tool_calls": [{"id": "toolu_1", "type": "function", "function": {"name": "Bash", "arguments": "{\"cmd\":\"ls\"}"}}], "content": "ok"},
  {"role": "tool", "tool_call_id": "toolu_1", "content": "a.txt"},
  {"role": "user", "content": "next"}],
 "max_tokens": 128, "stream": true, "stop": ["END"], "user": "u",
 "tools": [{"type": "function", "function": {"name": "Bash", "description": "run", "parameters": {"type": "object"}}}],
 "tool_choice": {"type": "function", "function": {"name": "Bash"}},
 "parallel_tool_calls": false, "reasoning_effort": "high"}
```
After `build_upstream_body`, `tool_choice` becomes the string `"Bash"`.

Tool results (`[1b]`): `_anthropic_tool_result_content([text "screenshot read", image base64 bbb, document text "doc"])` ->
`[{"type":"text","text":"screenshot read"},{"type":"image_url","image_url":{"url":"data:image/png;base64,bbb"}},{"type":"text","text":"doc"}]`;
`("a.txt")` -> `"a.txt"`; `("boom", True)` -> `"error: boom"`; `([image], True)` ->
`[{"type":"text","text":"error:"}, {image_url...}]`.

Mid-conversation system (`[8]`): `system:"top"`, messages `[user "hi", system [text "hook context"], assistant "ok"]` ->
roles `["system","user","system","assistant"]`, message 2 content `"hook context"`.
`{"role":"tool","content":"x"}` -> `ValueError` naming `'tool'` -> HTTP 400.

Effort (`[2]`): `thinking {type:disabled}` -> `none`; enabled budget 1000 -> `low`;
9000 -> `medium`; 17000 -> `high`; 40000 -> `xhigh`; `{type:adaptive}` -> none;
budget 0 -> none. Tool choice: `"any"` -> `"required"`, `{type:none}` -> `"none"`,
`{type:weird}` -> omitted.

### F12. Chat -> Anthropic message, non-stream (`_test_anthropic_messages.py` [3], `_test_anthropic_http.py` [3])

Input:
```json
{"id": "chatcmpl_1", "model": "deepseek-v4.1-flash",
 "choices": [{"finish_reason": "tool_calls", "message": {"role": "assistant", "content": "hello", "reasoning_content": "secret",
   "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "Bash", "arguments": "{\"cmd\":\"ls\"}"}}]}}],
 "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15, "prompt_cache_hit_tokens": 4, "prompt_cache_write_tokens": 2}}
```
Output:
```json
{"id": "msg_chatcmpl_1", "type": "message", "role": "assistant", "model": "deepseek-v4.1-flash",
 "content": [{"type": "thinking", "thinking": "secret"},
             {"type": "text", "text": "hello"},
             {"type": "tool_use", "id": "call_1", "name": "Bash", "input": {"cmd": "ls"}}],
 "stop_reason": "tool_use", "stop_sequence": null,
 "usage": {"input_tokens": 4, "cache_creation_input_tokens": 2, "cache_read_input_tokens": 4, "output_tokens": 5}}
```
HTTP round trip (`_test_anthropic_http.py` [3]): upstream chunks `"Hello "`, `"world"`,
finish `stop`, usage `{prompt_tokens:11, completion_tokens:2, prompt_cache_hit_tokens:3,
prompt_cache_write_tokens:1}` -> `content == [{"type":"text","text":"Hello world"}]`,
`stop_reason "end_turn"`, `usage.input_tokens == 7`, `output_tokens == 2`. Finish
`length` -> `stop_reason "max_tokens"`.

### F13. Chat stream -> Anthropic SSE (`_test_anthropic_messages.py` [4]-[6b], `_test_anthropic_http.py` [4]-[5])

Thinking + text. Upstream (+ `[DONE]`):
```
{"id":"chatcmpl_4","model":"m","choices":[{"delta":{"reasoning_content":"hmm"}}]}
{"id":"chatcmpl_4","model":"m","choices":[{"delta":{"content":"42"}}]}
{"id":"chatcmpl_4","model":"m","choices":[{"finish_reason":"stop","delta":{}}],"usage":{"prompt_tokens":11,"completion_tokens":2,"total_tokens":13,"prompt_cache_hit_tokens":3,"prompt_cache_write_tokens":1}}
```
Exact output bytes (each frame followed by a blank line):
```
event: message_start
data: {"message":{"id":"msg_chatcmpl_4","type":"message","role":"assistant","content":[],"model":"m","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}},"type":"message_start"}

event: content_block_start
data: {"index":0,"content_block":{"type":"thinking","thinking":""},"type":"content_block_start"}

event: content_block_delta
data: {"index":0,"delta":{"type":"thinking_delta","thinking":"hmm"},"type":"content_block_delta"}

event: content_block_stop
data: {"index":0,"type":"content_block_stop"}

event: content_block_start
data: {"index":1,"content_block":{"type":"text","text":""},"type":"content_block_start"}

event: content_block_delta
data: {"index":1,"delta":{"type":"text_delta","text":"42"},"type":"content_block_delta"}

event: content_block_stop
data: {"index":1,"type":"content_block_stop"}

event: message_delta
data: {"delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"input_tokens":7,"cache_creation_input_tokens":1,"cache_read_input_tokens":3,"output_tokens":2},"type":"message_delta"}

event: message_stop
data: {"type":"message_stop"}

```
Text then tool. Upstream (+ `[DONE]`):
```
{"id":"chatcmpl_3","model":"m","choices":[{"delta":{"content":"ok "}}]}
{"id":"chatcmpl_3","model":"m","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"Bash","arguments":"{\"cmd\":"}}]}}]}
{"id":"chatcmpl_3","model":"m","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls\"}"}}]},"finish_reason":"tool_calls"}]}
```
Output data lines:
```
message_start        {"message":{"id":"msg_chatcmpl_3",...,"model":"m",...},"type":"message_start"}
content_block_start  {"index":0,"content_block":{"type":"text","text":""},"type":"content_block_start"}
content_block_delta  {"index":0,"delta":{"type":"text_delta","text":"ok "},"type":"content_block_delta"}
content_block_stop   {"index":0,"type":"content_block_stop"}
content_block_start  {"index":1,"content_block":{"type":"tool_use","id":"call_1","name":"Bash","input":{}},"type":"content_block_start"}
content_block_delta  {"index":1,"delta":{"type":"input_json_delta","partial_json":"{\"cmd\":"},"type":"content_block_delta"}
content_block_delta  {"index":1,"delta":{"type":"input_json_delta","partial_json":"\"ls\"}"},"type":"content_block_delta"}
content_block_stop   {"index":1,"type":"content_block_stop"}
message_delta        {"delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"input_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0},"type":"message_delta"}
message_stop         {"type":"message_stop"}
```
Upstream error chunk `data: {"error":{"message":"boom"}}` -> the ONLY frame:
```
event: error
data: {"error":{"type":"api_error","message":"boom"},"type":"error"}

```
Empty upstream (`data: [DONE]` only): `message_start` (id `msg_<hex>`, request model),
`message_delta` (`end_turn`, zero usage), `message_stop`.

### F14. Anthropic HTTP surface (`_test_anthropic_http.py`)

| request | expected |
|---|---|
| any of the 4 Messages paths, gate off, valid key | 400, `{"type":"error","error":{"type":"invalid_request_error","message":"...WB_ENABLE_ANTHROPIC_MESSAGES..."}}`, upstream not contacted |
| gate on, no key | 401 `authentication_error`, `Content-Type` contains `application/json` |
| gate on, `x-api-key: WRONG` | 401 `authentication_error` |
| gate on, `Authorization: Bearer <key>` | authenticates |
| missing model / `messages: []` / `messages: "x"` / role `tool` | 400 `invalid_request_error` |
| body `{not json` | 400 Anthropic envelope (`invalid JSON body`) |
| `/v1/messages/count_tokens` `{"model":..,"messages":[{"role":"user","content":"hello world"}]}` | 200 `{"input_tokens": 3}` (int > 0) |
| streamed request | `Content-Type: text/event-stream`, first event `message_start` with `message.model == "deepseek-v4.1-flash"`, last `message_stop`; usage row recorded with `prompt_tokens 11`, `completion_tokens 2`, `cached_tokens 3`, outcome `completed` |
| tool stream with an `input_schema` tool | upstream body `tools[0].function.name == "Bash"`; first `content_block_start` is `tool_use` named `Bash`; partial_json joins to `{"cmd":"ls"}`; `stop_reason tool_use` |

### F15. `tool_choice` normalisation on the wire (`_test_tool_choice_none.py`)

`normalize_tool_choice` with `tools=[run_shell]`:
```
"none"                                         -> "none", tools kept
{"type":"none"}                                -> "none", tools kept
"auto" / {"type":"auto"}                       -> "auto"
"required"                                     -> "required"
{"type":"function","function":{"name":"run_shell"}} -> "run_shell"
(absent)                                       -> absent
```
Responses example: `tool_choice {"type":"function","name":"f"}` (Responses style, name at
top level) -> wire `"tool_choice": "f"`.

---

## 7. Ambiguities, quirks and port decisions

These are observed baseline behaviours. Unless marked "port may fix", reproduce them,
because clients and tests depend on the current shapes.

1. **Responses upstream error chunks are swallowed.** A chat chunk `{"error":{...}}`
   has no `choices`, so `stream_responses_events` ignores it and finishes with
   `response.completed` and an empty message (F9). The Anthropic path emits an `error`
   event instead. Port may fix (emit `response.failed`), but that changes golden output.
2. **`prompt_est` is always 1** in the Responses stream usage fallback because
   `meta.get("input")` reads `request_meta`, which never contains `input` (17648).
3. **`reasoning_only_response` ignores `custom_tool_call`** (17373-17374): a non-stream
   answer with reasoning plus only a custom tool call is rejected with 502. The stream
   path does not have this issue (it checks `tool_calls_map`). Likely a bug; port may fix.
4. **`sequence_number` restarts per web-tool round** and each round mints new
   `resp_`/`msg_`/`rs_` ids; only round 1 has created/in_progress and only the last has
   `response.completed`. A client keyed on response id sees ids change mid-stream.
5. **Passed-through server tools become `{"type":"function"}`** on the wire when the
   local web tools switch is off: `normalize_tools` forces `type:"function"` on a
   nameless `{"type":"web_search"}` entry, leaving an invalid function declaration. What
   the upstream does with it is not verified here.
6. **Reasoning after text** in the Responses stream appends deltas to an already
   `done` reasoning item, and the final reasoning item is not updated. In the Anthropic
   stream a reasoning delta while text is open opens a new thinking block without
   closing the text block (two open blocks).
7. **Anthropic stream has no `ping` event, no `signature`/`signature_delta`, no usage
   in `message_start`, and no estimation when the upstream omits usage** (zeros).
   Thinking blocks returned to the client are unsigned; the request side always drops
   incoming thinking blocks, so they are never replayed upstream.
8. **Anthropic tool ids**: streaming default is the deterministic `toolu_<chat index>`,
   non-stream default is random `toolu_<hex>`.
9. **`message.model`**: non-stream uses the upstream-reported model; stream uses the
   first chunk's `model`, else the request model. Responses always uses the request
   model.
10. **Function-call arguments `""`**: stream keeps `""` in `.done`/item; non-stream
    substitutes `"{}"`.
11. **First-chunk tool arguments** produce a `.delta` in the Anthropic stream but not in
    the Responses stream (F6).
12. **`text.format` is ignored** unless `WB_PRESERVE_REQUEST_CONTRACT=1`; same flag
    controls `strict` on flattened namespace children. Anthropic `output_config.format`
    is always ignored.
13. **No server-side state**: `store` ignored; `previous_response_id` -> 400 (except on
    compaction turns, where it is silently ignored).
14. **Background-request blocking** exists (`BLOCK_BACKGROUND_REQUESTS`) but is `False`
    at baseline; namespace rejection (`REJECT_NAMESPACE_TOOLS`) is dead code.
15. **`custom_tool_call` history uses default JSON separators** (`{"input": "..."}`,
    with a space) while Anthropic tool_use arguments use compact separators
    (`{"cmd":"ls"}`). Byte-exact tests must keep both.
16. **Orphan `function_call_output` with empty output** falls through to a
    `tool` message with `tool_call_id ""`, which `build_upstream_body`'s orphan cleanup
    may then drop (behaviour of the repair step is specified elsewhere).
17. **Anthropic route has no web-tool interception, no DSML recovery in the stream and
    no truncated-call filtering**; the non-stream path gets DSML/leak repair only
    through `aggregate_stream`.
18. **Python-isms to reproduce**: truthiness checks (empty strings ignored as deltas),
    `str(None) == "None"`, `%r` repr in the role error (`'tool'`), `int(x/3.6)`
    truncation in `estimate_tokens`, and code-point string lengths.
