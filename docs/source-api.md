# Reader source API 1.0

`reader-sources` is a reader-only leaf crate, adapted from Nova's GPL runtime and
HTTP policy at b69a1e5765ecc65ac3b00acdfe2f16e2f46a5dda. Generic extraction to
nova-kit is deferred until both applications have consumer compatibility tests.
Reader does not execute Nova v1/Stremio, Android APK or JVM JAR sources.

A package ZIP contains `manifest.json` and exactly its declared files:

```json
{"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"example","name":"Example","version":"1.0.0","language":"en","nsfw":false,"entry":"main.js","scripts":[],"assets":[],"permissions":{"domains":["example.com"],"httpPost":false},"capabilities":["browse","search","filters","details","chapters","pages"]}
```

Package format, contract/API compatibility and addon semantic version are independent.
Manifest keys are case sensitive. Unknown fields fail validation. Scripts execute in
listed order followed by entry. Paths must be relative, without empty, `.` or `..`
components, backslashes or colons. Duplicate, unlisted, symlink and non-file ZIP
entries are rejected. ZIP input/expanded totals are limited to 8 MiB, executable
UTF-8 JavaScript to 2 MiB. The deterministic builder is
`python3 tools/package-addon.py examples/addons/neutral-fixture output.zip`.
Native installation is the authoritative compatibility validator.

Reader 0.1.17 adds optional `"icon":"icon.png"`. It must name an entry in
`assets`, use PNG, fit 256 KiB and declare nonzero dimensions at most 512×512.
The UI fully decodes with bounded allocation on a worker and retains a 64px
thumbnail shared across language variants. Disabled addons may still display
bundled decoration without running scripts or fetching the network. Invalid
pixel streams use the Kit extension fallback; invalid declarations fail install.
Omitting `icon` preserves the fallback and compatibility with older packages.
Packages using the new manifest field require Reader 0.1.17 or newer.

`SourceRegistry::open(directory)` validates durable packages and digests.
`install_zip(bytes)` fully validates before publishing registry state atomically.
New installs start disabled; `set_enabled(id, true)` explicitly activates them.
Updates preserve enabled state unless permissions expand, in which case explicit re-enabling is required. A source ID/version can only ever refer to identical
archive bytes, including after removal. Failed installs retain the previous active
package. `list`, `source(id)`, `disable` and `remove` manage sources without touching
library records or downloads. Old Source handles are invalidated by update/removal.
A process-held file lock prevents independent registry instances or processes from racing writes; clone the registry handle for shared use. Content-addressed archives are retained for
immutable version history; automatic package garbage collection is not implemented.

`Source::call(op, &request)` invokes synchronous `globalThis.novaProvider.handle`.
Requests must be objects; the host sets `op` and `apiVersion: 1`. Optional sourceId,
page, query, filters, seriesId, chapterId, id and url are passed through. Operations:

| Operation | Result |
|---|---|
| capabilities | Source identities, languages, content warnings, supported operations |
| browse/search | `{items: [Series], hasNextPage: boolean}` |
| filters | Source-defined typed filter definitions |
| details | Series |
| chapters | `{chapters: [Chapter]}` |
| pages/resolvePage | `{pages: [Page]}` (resolvePage optional) |

Series: `id,title,url,coverUrl?,description?,authors[],genres[],status?`.
Chapter: `id,seriesId,title,number?:string,sourceOrder,url,language?,scanlator?,uploadedAt?`.
Page: `id,url,headers:{},width?,height?`. If `resolvePage` is declared, a rejected
image request (401/403/507) calls it once with `{url}` and retries its single
returned Page under the same permission, header and cancellation checks. This
supports short-lived image tokens; a second rejection is returned normally. Chapter numbers are decimal strings and
must never define identity. Page IDs remain stable when signed URLs expire.
Sources may implement multiple language variants using the request sourceId.
Capabilities/filters use generic JSON; typed Rust convenience methods are provided
for browse/search/details/chapters/pages. Domain model conversion belongs to the app.

Errors return `{error:{code,message}}`; recognized codes include ChallengeRequired,
AuthenticationRequired, RateLimited, NotFound, ParseChanged and Cancelled. Network
and script errors have Rust variants. Interactive challenge/login browser handoff
and persistent cookie import are not implemented or promised.

Host capabilities (all synchronous):

- `nova.http.get(url, headersJson = "{}")` and
  `nova.http.request(JSON.stringify({url,method:"GET",headers:{},body:null}))`
  return a JSON **string** `{status,url,body,headers}` or `{error:{code,message}}`.
  POST requires explicit permission. Bodies are text only. Sources map 401/403/429
  and missing markup to actionable source errors. Rust does this for image fetches.
- `nova.html.select(html, css, fieldsJson)` returns JSON-string rows. Field specs
  use `{selector:"a",value:"href"}`. Empty selector means the matched element;
  value defaults to text, with `ownText`, `html` and attribute names supported.
- `nova.url.resolve(relative, base)` resolves against the final response URL.

Each call creates a new 32 MiB QuickJS heap with a 512 KiB stack a 5-second script execution budget and a 20-second
wall deadline. Scripts cannot import modules or access filesystem/process APIs.
Inputs, output JSON and aggregate metadata HTTP bodies are bounded to 2 MiB;
12 HTTP requests and 5 redirects are permitted. Host-side HTML expansion is also
bounded. Images stay out of JavaScript; `fetch_page(url,headers,cancel)` streams
with cancellation checks into a separate 32 MiB encoded-byte limit. Manifest-listed
assets can be read using `addon://<source-id>/<asset-path>` (neutral fixtures).

Metadata and images use the same explicit domain allowlist, HTTPS-only/default-port
policy, safe header whitelist, manual redirect checks and source/profile cookie jar.
Cookie/Host/hop-by-hop headers are host-owned. The narrow `x-integrity-token`
header supports Kagane's public integrity endpoint; like Authorization and POST,
it forbids cross-origin redirects. Allowed Referer and Origin must also
match source domain permissions. Cookie-authenticated, explicitly authenticated and POST cross-origin redirects are
rejected. Each network hop resolves all addresses, rejects private/reserved IPs,
and pins the validated DNS snapshot for direct connections. Inherited HTTP proxy
configuration is honored; a configured proxy controls its own remote DNS resolution.
Only content-type, etag, last-modified and retry-after response headers reach JS.
Tokens/cookies are neither exported nor persisted.

`registry.with_profile(profile)` isolates in-memory source sessions. Callers must
cancel old profile/navigation work using cloned `CancelToken`s; cancellation is
checked during scripts and between body chunks. A blocking DNS lookup/connect/read
can finish before observing cancellation, bounded by a 5-second DNS wait and the HTTP timeout. A timed-out system resolver thread may finish later. The app must not deliver stale results across profiles.
`Source::with_transport` is an explicit testing seam; production registry sources
always use the checked HTTP transport. Tests use sanitized HTML and neutral images;
passing fixture tests does not establish live website support.

## Source configuration

The optional manifest `configuration` array declares at most 64 typed fields:

```json
[{"key":"language","name":"Language","kind":"select","default":"en","options":["en","ko"]},
 {"key":"showCompleted","name":"Show completed","kind":"bool","default":false},
 {"key":"label","name":"Label","kind":"text","default":""},
 {"key":"token","name":"Token","kind":"secret","default":""}]
```

Keys are unique ASCII identifiers up to 64 bytes; names are bounded to 128 bytes.
Text and secret values are at most 4096 bytes. Select options are unique strings
(up to 128 options, 256 bytes each); defaults must match the declared type/options.
Secret defaults must be empty. The schema and persisted settings each have a 64 KiB
limit. Settings files use Unix mode 0600 inside a profile directory with mode 0700;
on Windows they inherit the application directory ACL. Values are plaintext local
configuration, not an OS credential vault.

`settings_schema(id)` returns typed `SettingField` definitions, including disabled
sources. `settings(id)` returns a JSON object of ordinary values and **omits all
secret keys**. `settings_secrets(id)` returns secret-key/boolean presence flags.
`set_settings(id, partial_object)` validates and atomically merges updates;
omitted values, including secrets, remain unchanged. An explicit empty secret
string clears that secret. Unknown keys, invalid types and unsupported select
values fail without changing the old settings.

Configuration is stored separately from the registry/library in
`addons/settings/<SHA-256-of-profile>/<package-id>.json`. It is profile and package
scoped; language variants in one package share configuration. Cookie jars remain
in memory. Metadata backups exclude these files. On schema updates, only values
whose field kind and constraints still match are retained; removed/changed fields
fall back to defaults. Stored kind metadata prevents a former secret from becoming
visible when a field is reclassified as ordinary text. Corrupt settings JSON fails
explicitly rather than being silently overwritten.

Each operation receives one validated snapshot in `request.settings`; the host
overrides a caller-supplied settings object. `nova.settings.get(key)` returns its
native JSON value (or null for an unknown key). Existing Source handles see updates
on their next invocation; running operations retain their original snapshot.
Configured secret values are redacted from script/source error text and never
appear in descriptors. As executable addons can read their own settings, only
install trusted sources. Callers still cancel work when switching profiles.


Source filters support select, text, bool, multiselect and tristate. Multiple
choices produce arrays of provider values; tristate produces
`{include:[...],exclude:[...]}`. Existing Nova switches/dropdowns render option
rows, preserving independent include/exclude selection. Limits are 32 filter
groups, 200 choices per single-select, 512 per multiple group and 1024 rendered
controls total. Oversized definitions fail explicitly rather than truncating.
