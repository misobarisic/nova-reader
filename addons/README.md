# Installable Reader addons

[MangaFire 1.0.0](mangafire-1.0.0.zip) is a native Nova Reader format-2 addon.
Install the ZIP through Browse → Extensions → Install addon. The source is mixed,
so enable Show adult sources to see it; the addon defaults to Safe content.

Source and complete provenance: [examples/addons/mangafire](../examples/addons/mangafire).
The ZIP includes its Apache-2.0 license and notices. Rebuild it with:

```sh
python3 tools/package-addon.py examples/addons/mangafire addons/mangafire-1.0.0.zip
```

SHA-256: `db2bebbcc3363ac2e7b521e53f8524f4aaaf0b277044b84bd459774838999797`.
