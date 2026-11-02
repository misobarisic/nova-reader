# Mitaku native reader port

Upstream: https://github.com/keiyoushi/extensions-source
Pinned commit: `efa93f5a3537856fe830450bbe33f69d21d28167`
License: Apache License 2.0, retained in full in LICENSE.
Upstream extension versionCode: 5; native addon version: 1.0.0.

Ported files at that exact immutable revision:

- `src/all/mitaku/src/eu/kanade/tachiyomi/extension/all/mitaku/Mitaku.kt`
- `src/all/mitaku/src/eu/kanade/tachiyomi/extension/all/mitaku/Filters.kt`
- `src/all/mitaku/build.gradle.kts`
- `src/all/mitaku/res/mipmap-xxxhdpi/ic_launcher.png`

Behavior references: `Mitaku.kt` and `Filters.kt`: popular route, category-before-tag search precedence, whitespace-normalized lowercase tag slugs, final-response post redirects, article title/category/tag metadata, one Gallery chapter, `.entry-date` dates, and full-size `data-mfp-src` with `href` fallback from `a.msacwl-img-link`. The upstream source does not support Latest; this port returns an explicit unsupported-catalogue error. Text search overrides filters.

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

The unchanged upstream launcher icon is attributed to Keiyoushi's Mitaku
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
The independent distribution home is `nova-addons/reader-addons/mitaku`;
this repository retains reviewable source and synthetic regressions.
