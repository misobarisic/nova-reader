# Toonily native Reader port

Upstream: https://github.com/keiyoushi/extensions-source
Pinned revision: `efa93f5a3537856fe830450bbe33f69d21d28167`
License: Apache License 2.0; complete license retained in LICENSE.

Ported and inspected source paths at that exact revision:

- `src/en/toonily/build.gradle.kts` (versionCode 19, English, MIXED warning).
- `src/en/toonily/src/eu/kanade/tachiyomi/extension/en/toonily/Toonily.kt`.
- `src/en/toonily/src/eu/kanade/tachiyomi/extension/en/toonily/Filters.kt`.
- `lib-multisrc/madara/src/eu/kanade/tachiyomi/multisrc/madara/MadaraBase.kt`.
- `lib-multisrc/madara/src/eu/kanade/tachiyomi/multisrc/madara/MadaraNoAjax.kt`.
- `lib-multisrc/madara/src/eu/kanade/tachiyomi/multisrc/madara/Filters.kt` and `Models.kt`.

The unchanged bundled icon comes from
`src/en/toonily/res/mipmap-xxxhdpi/ic_launcher.png` at that same revision,
under the upstream repository's Apache-2.0 license. It is used as source
identification artwork. No ownership of Toonily branding is claimed.

This is a modified JavaScript port for Nova Reader, not a JVM/APK extension.
Native package ID: `toonily`; native English source ID: `toonily.en`.
The upstream extension identity is exactly `en.toonily`; Nova's source ID follows
its existing `<addon>.<language>` convention. Package version 1.0.0 is independent
of upstream versionCode 19. The upstream MIXED warning maps conservatively to
Reader's boolean `nsfw: true` in both manifest and source descriptor.

Preserved behavior: `/serie/` routes; popular/views and latest pagination;
lowercase ASCII search-slug normalization with punctuation collapsed to hyphens;
tag archive precedence over search text and all filters except Order By;
author/artist/year/status/order/adult/genre-condition/genre filters; site-provided
chapter order and decimal chapter numbers; POST to the series `ajax/chapters/`
endpoint; lazy image attributes and largest srcset candidate; `style=list`
fallback when the chapter advertises the single-page reader. The mature-content
boolean defaults false and writes the host-owned `toonily-mature=0` or `1` cookie
before each network operation. This requires Nova Reader 0.1.26 or newer.

Adaptations: native v2 DTOs and host HTTP/HTML/URL/cookie capabilities; stable
source-local series/chapter paths and chapter-scoped page indices; legacy
`/webtoon/` lookup URLs normalized to `/serie/` while canonical IDs are retained;
explicit HTTP, challenge and changed-markup
errors; protected chapter data fails explicitly instead of returning a partial
gallery; oversized tag directories use an unrestricted slug text field instead
of truncating options. No automatic view-count requests, related-series lookup,
legacy upstream numeric-post-ID/memo migration or encrypted chapter-protector
decryption is implemented. Covers retain the site's supplied URL; upstream's
optional best-effort HD image interceptor is not reproduced. No challenge bypass,
browser-session transfer or live-site availability is promised.

Domain evidence: Toonily.kt's `hdCoverInterceptor` identifies static cover versus
data panel CDN hosts. On 2026-11-02, metadata-only HTTP requests with the mature
cookie set to 0 confirmed `static.tnlycdn.com` cover references in `/serie/` and
`data.tnlycdn.com` panel references in the non-explicit Dungeon Reset chapter
`https://toonily.com/serie/dungeon-reset-f6fe1207/chapter-266/`. The matching series
details and POST chapter endpoint also returned HTTP 200. No cover or panel image
bytes were requested. These exact CDN hosts are permitted; unrelated third-party
analytics and advert hosts are not. This metadata observation establishes hosts
and markup, not successful image delivery or ongoing availability.

Every committed HTML/XML fixture and test is newly authored neutral synthetic
material. No scraped galleries, adult images or live-site HTML are distributed.
Development fixtures and Node dependencies are excluded from installable ZIPs.
Canonical distribution: `misobarisic/nova-addons/reader-addons/toonily`.
