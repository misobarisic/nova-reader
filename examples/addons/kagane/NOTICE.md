# Kagane native Reader port

Modified JavaScript port of Keiyoushi Kagane, originally extension versionCode 37.
Upstream: https://github.com/keiyoushi/extensions-source
Original immutable revision: `1dfd0bcf25b48cb5bd42d9191918a29072c67a61`.
Version 1.0.1 request compatibility follows versionCode 39 at immutable revision
`efa93f5a3537856fe830450bbe33f69d21d28167`.
License: Apache-2.0, retained in LICENSE.

Source paths: `src/all/kagane/build.gradle.kts` and
`src/all/kagane/src/eu/kanade/tachiyomi/extension/all/kagane/{Kagane,Dto,Filters}.kt`.
`icon.png` is the unchanged `src/all/kagane/res/mipmap-xxxhdpi/ic_launcher.png`
from the original pinned revision.

The port follows the v2 search/series API, compact chapter-index column mapping,
18 language variants, rating flags, genre/source/tag/staff/group filtering,
chapter-title options, public integrity/access-token flow and datasaver paths.
Tokens are fetched per page-list/refresh operation rather than retained as JVM
fields. Reader retries one rejected page through resolvePage on HTTP 401/403/507,
using its normal checked host transport. No captcha solver or interactive browser
session transfer is provided. The canonical host is kagane.moe, the homepage
redirect target observed on 2026-11-01 and reconfirmed on 2026-11-02. The former
kagane.cx and kagane.net hosts redirect API requests to kagane.moe. Their previous
domain grants remain for existing URL compatibility, alongside the observed
kstatic.to image cache; no new domain grants are introduced. Unknown cache hosts
fail explicitly.

Version 1.0.1 sends the site's Origin on metadata, integrity, book-manifest and
page requests, matching current upstream's Fetch.XHR requests. Kagane.kt at the
new pinned revision explicitly documents that the image host returns HTTP 404
without this Origin and changes its image requests to Fetch.XHR. A metadata-only
Safe-content probe on 2026-11-02 received a 44-page JXL book manifest on kstatic.to.
HEAD requests to the same signed panel URL returned HTTP 404 without Origin and
HTTP 200 with `Origin: https://kagane.moe`. No panel response body was downloaded.
Safe search, series details, chapter index, integrity and book-manifest metadata
returned HTTP 200 through the available HTTP proxy. Metadata 404s were not
reproduced from that proxy; the missing-Origin panel failure was reproduced.
Cover HEAD requests on this network returned HTTP 200 with and without Origin;
cover DTOs do not carry headers in the current Reader API.

The Mirror setting now accepts only kagane.moe. On upgrade Reader automatically
migrates stored kagane.cx/kagane.net selections to that default while retaining
other compatible source settings. Requests therefore start on the canonical
origin and do not require unsafe cross-origin POST redirects. No settings reset
or expanded-permission reactivation is needed. Collection/session endpoint 404s
now report a changed/unavailable API endpoint; a missing series or book still
returns NotFound. Genuine 404 image requests are not retried as token failures.

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
probes. The original 1.0.0 ZIP remains unchanged; 1.0.1 is a separate package
version. Synthetic fixtures and geometric codec samples are newly authored.
Host fixture/codec tests do not establish Android device acceptance or perpetual
site availability. Reopen a chapter if its server session is revoked.
