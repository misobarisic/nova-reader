# MangaFire native reader port

Upstream: https://github.com/keiyoushi/extensions-source
Immutable revision: `0d83f6be79e12b0a8b6c6f20a500c0e9456c222a`.
The repository's main API resolved to that revision on 2026-11-01.
License: Apache License 2.0, retained in LICENSE.

Ported source paths at that exact revision:

- `src/all/mangafire/build.gradle.kts`
- `src/all/mangafire/src/eu/kanade/tachiyomi/extension/all/mangafire/MangaFire.kt`
- `src/all/mangafire/src/eu/kanade/tachiyomi/extension/all/mangafire/Dto.kt`
- `src/all/mangafire/src/eu/kanade/tachiyomi/extension/all/mangafire/Filters.kt`
- `src/all/mangafire/src/eu/kanade/tachiyomi/extension/all/mangafire/VrfSigner.kt`

This modified JavaScript port implements the Reader format-2/API-1 contract.
The request signer's three tables, keys, IVs, UTF-8 conversion, sorted query
canonicalization and URL-safe unpadded Base64 follow the pinned VrfSigner.
The signer is an API request protocol adapter, not a captcha solver.

Changes: stable series IDs omit mutable slugs; all seven upstream languages are
explicit source variants; JSON metadata and image descriptors use the Reader
host and scoped permissions; missing/changed API fields and HTTP errors become
explicit Reader errors; chapter enumeration is sequential and constrained by the
host's 12-request/2-MiB operation budget. The pinned getPageList returns direct URL descriptors and defines no image
descrambling transform; no legacy scramble algorithm is guessed or applied.
No partial list is returned when it
exceeds that request budget. The native default content rating is Safe rather
than upstream's All; the addon is still marked NSFW because the source is mixed.
Optional chapter merging, official-edition preference and volume preference are
ported through typed source settings. Native version 1.0.0 is independent of
upstream extension versionCode 35.

Not ported: `ChallengeSolverInterceptor.kt`, `assets/solver.html`, Android WebView
captcha solving, its OpenCV dependency, related-series-by-search integration and
upstream client rateLimit interceptor. Requests use the Reader host's limits;
403/captcha pages report ChallengeRequired with no bypass claim. The exact HTTP
allowlist currently covers mangafire.to and subdomains of the observed CDN
suffixes mfcdn.nl and mfcdn1.xyz. A new CDN suffix needs a permission-reviewed
package update; it is not automatically trusted from API output.

Synthetic fixtures contain newly authored neutral metadata and placeholder image
URLs only. No scraped chapter/image bytes or solver asset is included. Safe live metadata probes on 2026-11-01 validated API signing, listing,
details, chapter-list and page-descriptor endpoints. These probes fetched no
image bytes and do not establish image decoding, downloading or device reading.
The production Reader-host live smoke was attempted separately and failed before
listing with Network("DNS deadline"); successful proxy HTTP probes are not claimed
as a successful native Reader live session.
Independent publication belongs in nova-addons/addons/mangafire when requested;
this package is carried as a reference in the reader repository.

Version 1.0.1 bundles the upstream `res/mipmap-xxxhdpi/ic_launcher.png` icon
from Keiyoushi revision `1dfd0bcf25b48cb5bd42d9191918a29072c67a61`, unchanged,
under the repository Apache-2.0 license. Parser behavior is unchanged.
