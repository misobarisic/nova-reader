# Installable Reader addons

These native format-2 ZIPs target Nova Reader. Minimum versions are listed below;
Toonily needs **Reader 0.1.26** for host-managed cookie settings. Install through **Browse → Extensions → Install addon**. Updating
an installed package retains its enabled state unless permissions expand.
These sources are mixed/NSFW: enable **Show adult sources** to list them.
MangaFire and Kagane default to Safe content in their source settings.

| Addon | Download | Minimum Reader | Source/provenance |
| --- | --- | --- | --- |
| Manhwa18cc 1.0.1 | [ZIP](manhwa18cc-1.0.1.zip) | 0.1.17 | [Source](../examples/addons/manhwa18cc) |
| MangaFire 1.0.1 | [ZIP](mangafire-1.0.1.zip) | 0.1.17 | [Source](../examples/addons/mangafire) |
| Kagane 1.0.0 | [ZIP](kagane-1.0.0.zip) | 0.1.17 | [Source](../examples/addons/kagane) |
| Toonily (English) 1.0.0 | [ZIP](toonily-1.0.0.zip) | 0.1.26 | [Source](../examples/addons/toonily) |
| Mitaku 1.0.0 | [ZIP](mitaku-1.0.0.zip) | 0.1.26 | [Source](../examples/addons/mitaku) |
| CosplayTele 1.0.0 | [ZIP](cosplaytele-1.0.0.zip) | 0.1.26 | [Source](../examples/addons/cosplaytele) |

Each includes the upstream icon, Apache-2.0 license and pinned-source notices.
Kagane has 18 language variants and a configurable upstream mirror. Reader's
JPEG XL decoder supports its current page format.

Toonily defaults **Show mature content** to off. It sets only the explicitly
granted `toonily-mature` cookie on `https://toonily.com` through Reader's isolated
host jar; scripts cannot read cookies or supply raw Cookie headers. Cookies are
in memory and never synced or included in backups. Permission-expanding updates
require explicit re-enabling on each device.

Mitaku and CosplayTele expose each image gallery as one chapter. Mitaku supports
Popular browsing; CosplayTele offers Popular and Latest. Video-only posts return
an explicit error. These three ports reference Keiyoushi revision
`efa93f5a3537856fe830450bbe33f69d21d28167`; each notice records exact source,
dependencies, icon attribution and port differences. Neutral Node fixtures live
in each source's `tests/` folder; native QuickJS tests live under
`crates/reader-sources/tests/`. Both remain outside install ZIPs.
Metadata samples were checked live, including Toonily's chapter POST and its
exact cover/panel CDN domains. No image bytes were fetched. Availability,
challenges, encrypted markup and unverified external image hosts can still
prevent live use; unknown hosts fail until permissioned. New ports are tested
with Reader 0.1.26; existing downloads retain their previous compatibility.

Rebuild deterministically, for example:

```sh
python3 tools/package-addon.py examples/addons/kagane addons/kagane-1.0.0.zip
```

SHA-256:

```text
9052373c938281bf99d51adb99fe01d11449da482db8811b182cddd003d22261  manhwa18cc-1.0.1.zip
d649274c7c2891df86eb6fc9abc28eb9ac285740f53dd82ada5a48f2853a62fb  mangafire-1.0.1.zip
28ff94cc3b6c1720c9235f03a1410702fd8c056c341d0365f91dc45f07f17212  kagane-1.0.0.zip
5de8447e16860d409d2e2b6d440314c1c3afbb246bb1525f1585ad9f9aad5d8d  toonily-1.0.0.zip
9f2b1069db96dfe840e76b64494f4a16d49ef102f706fba314183909df9a8674  mitaku-1.0.0.zip
fca6428f39d2671eadcaf84a342a39b82efa1a568a996cb0b6d6e26f251f6375  cosplaytele-1.0.0.zip
```

The older [MangaFire 1.0.0](mangafire-1.0.0.zip) remains available for older Reader
versions without addon icons (SHA-256
`db2bebbcc3363ac2e7b521e53f8524f4aaaf0b277044b84bd459774838999797`).

The shared [Nova addons repository](https://github.com/misobarisic/nova-addons)
also publishes these packages from `reader-addons/` as `nova-reader-…zip` release
assets. Their manifest contract remains `nova-reader1`; the video addons in that
repository target Nova. These existing downloads remain available for installed
catalogue links.
