# Installable addons

Real site addons, their tests and release packaging are maintained
in [nova-addons](https://github.com/misobarisic/nova-addons#nova-reader-chapter-and-page-sources).
Use its `reader-addons/` sources and download installable ZIPs from its
[GitHub Releases](https://github.com/misobarisic/nova-addons/releases). Reader contains no real site addon implementations or ZIPs.

Install a ZIP from **Browse → Extensions → Install addon**, or run
`nova-reader install-addon /path/to/addon.zip`. Permission-expanding updates
require re-enabling the addon. Reader format-2 packages use `nova-reader1`;
Nova video addons use a separate contract and cannot run in Reader.

The only in-repository addon is [the neutral host fixture](../examples/addons/neutral-fixture),
with generated geometric images for offline runtime and UI tests. Reader retains
its generic packager at `tools/package-addon.py`; source-specific tests and
published-package checks belong to nova-addons.
