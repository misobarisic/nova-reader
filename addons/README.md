# Installable Reader addons

These native format-2 ZIPs require **Nova Reader 0.1.17 or newer** for bundled
addon icons. Install through **Browse → Extensions → Install addon**. Updating
an installed package retains its enabled state unless permissions expand.
All three sources are mixed/NSFW: enable **Show adult sources** to list them.
MangaFire and Kagane default to Safe content in their source settings.

| Addon | Download | Source/provenance |
| --- | --- | --- |
| Manhwa18cc 1.0.1 | [ZIP](manhwa18cc-1.0.1.zip) | [Source](../examples/addons/manhwa18cc) |
| MangaFire 1.0.1 | [ZIP](mangafire-1.0.1.zip) | [Source](../examples/addons/mangafire) |
| Kagane 1.0.0 | [ZIP](kagane-1.0.0.zip) | [Source](../examples/addons/kagane) |

Each includes the upstream icon, Apache-2.0 license and pinned-source notices.
Kagane has 18 language variants and a configurable upstream mirror. Reader's
JPEG XL decoder supports its current page format; JPEG XL encoding is not added.

Rebuild deterministically, for example:

```sh
python3 tools/package-addon.py examples/addons/kagane addons/kagane-1.0.0.zip
```

SHA-256:

```text
9052373c938281bf99d51adb99fe01d11449da482db8811b182cddd003d22261  manhwa18cc-1.0.1.zip
d649274c7c2891df86eb6fc9abc28eb9ac285740f53dd82ada5a48f2853a62fb  mangafire-1.0.1.zip
28ff94cc3b6c1720c9235f03a1410702fd8c056c341d0365f91dc45f07f17212  kagane-1.0.0.zip
```

The older [MangaFire 1.0.0](mangafire-1.0.0.zip) remains available for older Reader
versions without addon icons (SHA-256
`db2bebbcc3363ac2e7b521e53f8524f4aaaf0b277044b84bd459774838999797`).

The shared [Nova addons repository](https://github.com/misobarisic/nova-addons)
also publishes these packages from `reader-addons/` as `nova-reader-…zip` release
assets. Their manifest contract remains `nova-reader1`; the video addons in that
repository target Nova. These existing downloads remain available for installed
catalogue links.
