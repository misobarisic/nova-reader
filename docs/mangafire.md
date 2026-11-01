# MangaFire native source

`examples/addons/mangafire` is a separately installable Reader format-2 package,
version 1.0.0, contract `nova-reader1`, API `^1.0`. It is a JavaScript port of
Keiyoushi's implementation, not an APK/JAR loader. Exact upstream revision,
paths, modifications and Apache-2.0 text are retained in the package's
[NOTICE](../examples/addons/mangafire/NOTICE.md) and
[LICENSE](../examples/addons/mangafire/LICENSE).

## Install and select

```sh
python3 tools/package-addon.py examples/addons/mangafire /tmp/mangafire-1.0.0.zip
cargo run --locked -- install-addon /tmp/mangafire-1.0.0.zip
cargo run --locked
```

In Browse, turn on **Show adult sources** and select MangaFire's desired language.
Upstream classifies this catalogue as mixed. Reader's binary content-warning
field is conservatively `nsfw: true` for the entire package and each variant.
The native port defaults to **Safe**, rather than upstream's all-content default.
Source Settings exposes `contentRating` (`safe`, `suggestive`, `erotica`,
`pornographic`, `all`), volume preference, edition merging and official-edition
preference. Existing entries and image bytes are not automatically reclassified
or removed when that setting changes.

Explicit source IDs are `mangafire.en`, `mangafire.es`, `mangafire.es-419`,
`mangafire.fr`, `mangafire.ja`, `mangafire.pt`, and `mangafire.pt-BR`. English
is the API fallback. Latin American Spanish maps to API code `es-la`; Brazilian
Portuguese maps to `pt-br`. All variants share their package's profile settings.

## Behavior and request protocol

Popular and latest query `/api/titles` with `order[views_30d]=desc` and
`order[chapter_updated_at]=desc`, respectively. Listings preserve API order and
use `meta.hasNext`, independently of item count. Text search and filters combine,
matching the pinned source. Author/artist lookup uses `/api/tags`; an unmatched
author yields a legitimate empty result. Type, content rating, genre inclusion/
exclusion, theme, status, AND/OR match mode, year bounds, minimum chapters and
sort definitions retain the upstream values.

Every API request uses the upstream three-stage VRF signature: stable sorting by
query key, indexed repeated `[]` parameters in the signed cleartext, UTF-8 bytes,
three substitution/XOR-feedback stages, and URL-safe unpadded Base64. This is
implemented in `vrf.js` without extra host primitives. It does not solve captchas.
No browser code, WebView asset or OpenCV solver is included. In this pinned
implementation `getPageList` returns `data.pages[].url` directly; `PageDto`
contains no scramble key and there is no image decode/descramble interceptor.
`VrfSigner` changes `/api/` requests only. This port therefore does not assume
the image scrambling used by some older MangaFire implementations.

Series IDs are `/title/<hid>` and omit mutable title slugs. Retrieval URLs retain
the current slug. Details include description, authors, type/genre/theme and
status. Chapter pages are enumerated sequentially with language, descending
number sort and `limit=200`, preserving server order and distinct edition IDs.
Decimal numbers remain strings in Reader DTOs; integer IDs received as JSON
strings remain strings. Optional merging groups by displayed chapter number,
uses the chosen official-edition preference and sorts descending, following
upstream. It is off by default so editions remain distinct. Volume preference
uses matching-language volumes, falling back to chapters only when the API has
no volumes at all, as upstream does.

Chapter/page identities do not contain image tokens. Page descriptors preserve
API order, include dimensions when supplied, and carry a title/chapter Referer.
Rust owns image bytes and source/profile sessions. Permissions cover
`mangafire.to` and the CDN suffixes `mfcdn.nl` and `mfcdn1.xyz`, including child
hosts. These suffixes were observed in safe live metadata on 2026-11-01.
Unexpected image hosts fail with `ParseChanged`; a new suffix requires a
permission-reviewed package update. Redirect and HTTPS/header policy remain
host-enforced. No arbitrary domain or custom-cookie setting is exposed.

HTTP 401, 403/captcha markup, 404, 429 and server failures become
`AuthenticationRequired`, `ChallengeRequired`, `NotFound`, `RateLimited` and
`Network`, respectively. Changed API fields/non-JSON responses become
`ParseChanged`. Interactive captcha solving/session transfer is unavailable;
the port never claims a challenge bypass. Upstream's optional auto-solver and
Android-specific client interceptors are not ported.

The host's per-operation limits still apply: at most 12 metadata requests,
2 MiB aggregate metadata and bounded QuickJS execution. A chapter catalogue over
12 API pages (11 after a volume probe) fails explicitly before fetching a partial
list. This is a current large-series limitation, not a complete-list success.
The upstream client rate-limit interceptor is not replicated in JavaScript;
429 responses are explicit and retry remains a user action.

## Catalogue identity

The live Keiyoushi gzip index fetched on 2026-11-01 had compressed SHA-256
`acaae63ab34af917e45687bbac4eedf160dc1f110c8084d84721b5eaafe17c4f`. Its exact
MangaFire extension record and repository metadata are retained in
`crates/reader-catalogue/fixtures/keiyoushi-mangafire.pb.gz` (deterministic gzip
mtime 0). Package `eu.kanade.tachiyomi.extension.all.mangafire`,
version `1.6.35`/code `106035`, declares mixed content warning `2`. The checked-in
port mappings use the actual decimal-string IDs below; knowing a mapping alone
does not publish an addon or make it available in a trusted native catalogue.

| Language | Upstream source ID | Native source ID |
| --- | --- | --- |
| en | `6084907896154116083` | `mangafire.en` |
| es | `8098539940032331958` | `mangafire.es` |
| es-419 | `3849324770075062403` | `mangafire.es-419` |
| fr | `6228021941803932286` | `mangafire.fr` |
| ja | `524110931382140732` | `mangafire.ja` |
| pt | `758400875161895515` | `mangafire.pt` |
| pt-BR | `7842137233968841729` | `mangafire.pt-BR` |

## Research comparison and verification

Both candidates were inspected at immutable Keiyoushi revision
`0d83f6be79e12b0a8b6c6f20a500c0e9456c222a`, returned by the main-commit API on
2026-11-01. MangaFire's paths are recorded in NOTICE. Kagane's comparable files
are under `src/all/kagane/`: `build.gradle.kts` and
`src/eu/kanade/tachiyomi/extension/all/kagane/{Kagane,Dto,Filters}.kt`.
Kagane currently uses integrity-token POSTs, an `x-integrity-token` header,
dynamic cache hosts, access-token refresh and JPEG XL pages by default.
Those require additional host/decoder design. MangaFire's JSON GET API and pure
request signer fit the existing host, so MangaFire was selected for this port.
No Kagane package is advertised as implemented.

Required tests are network-free and use newly authored neutral JSON only:

```sh
node examples/addons/mangafire/tests/parser.test.mjs
cargo test --locked -p reader-sources --test mangafire
cargo test --locked -p reader-catalogue --test mangafire
```

The Node test checks signer vectors and request/result rules. Actual-host Rust
tests install the real ZIP content and execute it in QuickJS, covering languages,
settings, author lookup, paginated/duplicate chapters, volume fallback, stable
identities, dimensions, CDN/header handling, HTTP/challenge errors, changed
markup, cancellation and the chapter-request budget. Placeholder page transport
returns inert bytes; these fixtures make no claim about live image decoding.
The manual test is opt-in and never fetches image bytes:

```sh
cargo test --locked -p reader-sources --test mangafire live_safe_metadata_smoke -- --ignored --nocapture
```

Safe live probes on 2026-11-01 confirmed HTTP 200 and the expected API schema for
listing, details, chapters and page descriptors. They fetched no image bytes.
The opt-in smoke through the production Reader host was also attempted: it
failed before listing with `Network("DNS deadline")` during host DNS preflight.
Thus the successful proxy-based HTTP probes do not establish a successful native
Reader live session. Live download/decode/device reading and future challenge
availability remain unverified. Native version 1.0.0 is independent of upstream versionCode 35.
Independent future publication belongs in `nova-addons/addons/mangafire`; this
change does not publish or modify that repository.
