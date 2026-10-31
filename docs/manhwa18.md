# Manhwa18.cc reference source

The independently installable package is in `examples/addons/manhwa18cc`.
It is marked NSFW. Select `sourceId` explicitly as `manhwa18cc.en`,
`manhwa18cc.ko` or `manhwa18cc.all`; English is the API fallback. The application enables a newly selected ZIP explicitly; raw registry installation
starts disabled. Permission-expanding updates require re-enabling in Browse. No adult images or scraped live
HTML are bundled. The neutral fixture package supports the same reader contract
without network access.

From the repository root:

```sh
python tools/package-addon.py examples/addons/manhwa18cc /tmp/manhwa18cc.zip
python tools/package-addon.py examples/addons/neutral-fixture /tmp/neutral-fixture.zip
node examples/addons/neutral-fixture/tests/fixture.test.mjs
cd examples/addons/manhwa18cc/tests
npm ci --ignore-scripts
npm test
```

Package syntax is defined by the host tooling. Both packages declare format 2,
`nova-reader1`, API `^1.0`, and independent semantic versions. They are source
packages for Nova Reader, not executable Keiyoushi APK/JAR files. The eventual
publication location is `nova-addons/addons/manhwa18cc`; no repository catalogue,
signing or automatic update support is claimed here.

`yiimadara.js` contains the reusable parser and request policy; `main.js` only
sets the Manhwa18 routes. Popular uses `/webtoons/<page>?orderby=trending`, latest
uses `/webtoons/<page>`, and Korean uses `/raw/<page>`. Text search uses
`/search?q=…&page=…`, replaces ASCII apostrophes with typographic apostrophes,
and ignores filters. Without text search genre takes precedence over category.
The newest-series sort is separate from latest updates. Pagination comes from
the next link before filtering. English hides raw entries; Korean includes only
raw entries outside the raw directory; all includes both, matching upstream.

Series identities are `/webtoon/<slug>`. Chapters retain source order and full
source-local paths, including specials and fractional chapter numbers. Dates
accept English `dd MMM yyyy` and `dd MMM yy` and become ISO date strings. Pages
retain document order, prefer `data-src`, resolve relative to the final response
URL, and carry chapter Referer and source Origin headers. The host owns image
bytes and the source/profile session. Cross-host image domains require a reviewed
manifest permission update; this package currently permits `manhwa18.cc` only.

The parser maps 403/challenge markup to `ChallengeRequired`, 429 to `RateLimited`,
401 to `AuthenticationRequired`, and 404 to `NotFound`. Missing expected markup
becomes `ParseChanged`; only explicit empty-result markup or a present empty
chapter container is treated as empty. Interactive browser challenge resolution
and persistent session transfer are not implemented by this package. Cancellation
and request limits belong to the actual QuickJS host.

Deterministic fixtures cover languages, routes, apostrophes, filter precedence,
pagination after filtering, optional metadata, badges, dates, chapter order,
relative/lazy image URLs, headers, errors and markup changes. The Node shim uses
pinned linkedom; actual QuickJS host integration is tested in reader-sources.
No live-site smoke test has been run and live-site support is unverified.
A manual live probe, when chosen, should inspect safe metadata/status only and
remain separate from CI.

`neutral-fixture` bundles three geometric PNGs: portrait 600x900, wide 1800x600,
and tall 128x12000. Its mixed chapter alternates the shapes; its 320-page chapter
reuses them with distinct stable page IDs. `addon://` assets exercise install,
browse, details, pages and offline reading without network access. This fixture
helps functional checks; it does not establish device memory or frame-time gates.

Exact upstream commit, paths, modifications, omissions and Apache-2.0 license
are retained in the package NOTICE.md and LICENSE. Native source version 1.0.0
is independent of upstream versionCode 65. Upstream catalogue IDs are not guessed
or mapped by this reference package.
