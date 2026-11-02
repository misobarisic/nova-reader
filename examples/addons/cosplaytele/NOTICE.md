# CosplayTele native reader port

Upstream: https://github.com/keiyoushi/extensions-source
Pinned commit: `efa93f5a3537856fe830450bbe33f69d21d28167`
License: Apache License 2.0, retained in full in LICENSE.
Upstream extension versionCode: 1; native addon version: 1.0.0.

Ported files at that exact immutable revision:

- `src/all/cosplaytele/src/eu/kanade/tachiyomi/extension/all/cosplaytele/CosplayTele.kt`
- `src/all/cosplaytele/src/eu/kanade/tachiyomi/extension/all/cosplaytele/Dto.kt`
- `src/all/cosplaytele/src/eu/kanade/tachiyomi/extension/all/cosplaytele/Filters.kt`
- `src/all/cosplaytele/build.gradle.kts`
- `src/all/cosplaytele/res/mipmap-xxxhdpi/ic_launcher.png`

Behavior references: `CosplayTele.kt`, `Dto.kt` and `Filters.kt`: popular-post JSON endpoint and embedded featured media, latest/search HTML listings and pagination, combined category/text search, category/tag discovery from `explore-categories`, completed post metadata, one Gallery chapter, `time.updated` dates, and `.gallery-item img` DOM image order. The native page-1 offset is intentionally corrected from upstream `page * 20` to `(page - 1) * 20`, so the first batch is retained. Popular JSON rendered titles are decoded as HTML text. Upstream direct-URL query routing is not advertised as a separate operation; gallery URLs work as details/chapter/page identities.

These source classes use the upstream KeiSource/HTTP, Jsoup selection, model DTO,
filter, and date helpers. No shared upstream parser or multisrc implementation
is copied or required by these independent JavaScript ports.

Changes for Nova Reader: synchronous host HTTP/HTML/URL adapters; reader format-2
package and `nova-reader1` DTOs; explicit HTTP/challenge/markup errors; canonical
source-local path identities without query, fragment or trailing slash; strict
failure for malformed items/images instead of partial galleries; stable indexed
page IDs; HTTPS image URL checks and Referer/Origin page headers. Both manifest
and capability descriptors mark this source NSFW and language `all`.
Video-only posts report an image-gallery-only error when requesting pages.

The unchanged upstream launcher icon is attributed to Keiyoushi's CosplayTele
contributors at the pinned path/revision above, under Apache-2.0. No other
upstream assets, site gallery images or scraped site HTML are bundled. Tests and
neutral SVG image responses are newly authored synthetic fixtures; Node's
linkedom dependency is development-only and excluded from package inputs.

Domain scope is restricted to the source's declared site hosts (no CDN wildcard).
Metadata listings and one post per source were checked on 2026-11-02 using a
standard browser User-Agent: HTTP 200, gallery selectors present, and all sampled
full-size image URLs on the primary site host. Requests without that User-Agent
returned HTTP 403. No live image bytes were fetched; broader image host coverage
is unverified. Future external image hosts fail explicitly until separately
verified and permissioned. Passing fixtures does not establish live
site availability, challenge bypass, browser/session transfer or video support.
The independent distribution home is `nova-addons/reader-addons/cosplaytele`;
this repository retains reviewable source and synthetic regressions.
