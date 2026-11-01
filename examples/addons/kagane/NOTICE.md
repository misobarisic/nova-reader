# Kagane native Reader port

Modified JavaScript port of Keiyoushi Kagane, extension versionCode 37.
Upstream: https://github.com/keiyoushi/extensions-source
Immutable revision: `1dfd0bcf25b48cb5bd42d9191918a29072c67a61`.
License: Apache-2.0, retained in LICENSE.

Source paths: `src/all/kagane/build.gradle.kts` and
`src/all/kagane/src/eu/kanade/tachiyomi/extension/all/kagane/{Kagane,Dto,Filters}.kt`.
`icon.png` is the unchanged `src/all/kagane/res/mipmap-xxxhdpi/ic_launcher.png`.

The port follows the v2 search/series API, compact chapter-index column mapping,
18 language variants, rating flags, genre/source/tag/staff/group filtering,
chapter-title options, public integrity/access-token flow and datasaver paths.
Tokens are fetched per page-list/refresh operation rather than retained as JVM
fields. Reader retries one rejected page through resolvePage on HTTP 401/403/507,
using its normal checked host transport. No captcha solver or interactive browser
session transfer is provided. The default mirror is kagane.moe, the homepage
redirect target observed on 2026-11-01. All three upstream mirror domains and the
observed kstatic.to image cache are explicitly declared; unknown cache hosts fail.

Reader identities use UUIDs, not slugs or expiring URLs. Decimal chapter_no text
is preserved; upstream's special source formats use sort_no. Chapter indices must
be complete. Metadata and image requests remain under Reader's limits. Search
requests default to Safe and disable pornographic/disturbing results until the
user explicitly changes source settings. The package is marked mixed/NSFW.

Not ported: related-series UI, Android preference migration, title source suffix,
formatted link/staff description sections, excluded-genre/format preferences
(filters offer those choices), or the upstream client rate-limit interceptor.
Source filters expose at most 512 choices per group and 1024 controls total;
large lists fail explicitly. The native addon version is independent of upstream.

Live probes on 2026-11-01 verified Safe search, series details, compact chapter
index, integrity and both original/datasaver page manifests. Both manifests used
JPEG XL on kstatic.to. No live chapter image bytes are included or fetched by the
probes. Synthetic fixtures and geometric codec samples are newly authored.
Host fixture/codec tests do not establish Android device acceptance or perpetual
site availability. Reopen a chapter if its server session is revoked.
