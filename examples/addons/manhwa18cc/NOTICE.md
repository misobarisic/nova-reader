# Manhwa18.cc native reader port

Upstream: https://github.com/keiyoushi/extensions-source
Pinned commit: `0d83f6be79e12b0a8b6c6f20a500c0e9456c222a`
License: Apache License 2.0, retained in LICENSE.

Ported source paths at that exact revision:

- `src/all/manhwa18cc/src/eu/kanade/tachiyomi/extension/all/manhwa18cc/Manhwa18Cc.kt`
- `src/all/manhwa18cc/build.gradle.kts`
- `lib-multisrc/yiimadara/src/eu/kanade/tachiyomi/multisrc/yiimadara/YiiMadara.kt`
- `lib-multisrc/yiimadara/src/eu/kanade/tachiyomi/multisrc/yiimadara/Filters.kt`

This is a modified JavaScript port for Nova Reader, not a JVM/APK extension.
Changes: native v2 package envelope and reader DTOs; host HTTP, HTML selection and
URL resolution adapters; explicit error mapping and missing-markup detection;
source-local path identities; language variants selected by sourceId; ISO date
strings; page request Referer/Origin descriptors. Related-series and legacy
Madara chapter-memo migration are not implemented. The unchanged route, ordering,
raw-language policy, text-search apostrophe normalization and filter precedence
are retained. No adult images or scraped site HTML are included.
Fixtures and neutral images are newly authored synthetic material.

No live-site availability, challenge bypass or session transfer is implied.
Native addon version 1.0.0 is independent of upstream extension versionCode 65.
The intended independently published home is nova-addons/addons/manhwa18cc;
this reader repository carries the reviewable reference package and fixtures.

Version 1.0.1 bundles the upstream `res/mipmap-xxxhdpi/ic_launcher.png` icon
from Keiyoushi revision `1dfd0bcf25b48cb5bd42d9191918a29072c67a61`, unchanged,
under the repository Apache-2.0 license. Parser behavior is unchanged.
