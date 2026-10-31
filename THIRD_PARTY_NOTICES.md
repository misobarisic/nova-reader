# Third-party notices and provenance

Nova Reader is derived from [Nova](https://github.com/misobarisic/nova), revision
`b69a1e5765ecc65ac3b00acdfe2f16e2f46a5dda`, by Nova's contributors. Adapted
material includes project/build/release structure, platform conventions,
application artwork, navigation/PageHeader/More presentation patterns, and reader source runtime/HTTP foundations. Reader changes
remove video-specific responsibilities and establish a separate application.
Nova and this project's code are GPL-3.0-or-later; the full GPL text is in LICENSE.

Shared storage and UI utilities are consumed from
[nova-kit](https://github.com/misobarisic/nova-kit), pinned in Cargo.toml/Cargo.lock
to `63141dfc6abd39a247edde1f33ebaeb59dc7229a`, the inherited Nova pin. The plan
also inspected kit revision `2e038b6280466130f6b58d7b3846fd2a68cc198f`; Reader
has not silently switched to it. Kit retains its own GPL-3.0-or-later notices.
Generic addon extraction and the Nova compatibility gate remain pending.

The Manhwa18.cc/YiiMadara JavaScript port derives from Keiyoushi
extensions-source revision `0d83f6be79e12b0a8b6c6f20a500c0e9456c222a` under
Apache-2.0. Exact source paths, modifications and omissions are recorded in
`examples/addons/manhwa18cc/NOTICE.md`; its full applicable license is retained
alongside it. Keiyoushi catalogue schema/data provenance and live-index extraction
are recorded in `docs/catalogues.md` and `crates/reader-catalogue/fixtures`.
Those upstream IDs/version numbers do not imply executable APK/JAR compatibility.
The newly authored neutral source and generated geometric PNGs are CC0-1.0;
see `examples/addons/neutral-fixture/NOTICE.md`.

Slint 1.18 is separately licensed. Its published alternatives include GPL-3.0-only,
Slint Royalty-free License 2.0 and Slint Software License 3.0. The application's
GPL declaration does not replace Slint's terms or other dependencies' licenses.
This open-source build uses the applicable GPL option. Review upstream terms
before distributing under another licensing arrangement.

Release tooling `.github/scripts/dependency-licenses.sh` uses cargo-about with
locked target/features and fails on unresolved license inventory entries. It
produces full dependency license texts/upstream links separately for Linux,
Windows and Android; those inventories accompany packaged artifacts. This notice
is attribution, not a substitute for that target-specific dependency inventory.
The inherited launcher art is not newly commissioned Reader branding.

The UI uses the same Roboto Regular and Bold faces as Nova, derived from
[googlefonts/roboto-2](https://github.com/googlefonts/roboto-2) revision
`38062f4b4a0be4346d07a928408da21602545e9e` under Apache-2.0. Nova's prepared
faces are retained unchanged; see `assets/fonts/README.md` and the full license
in `assets/fonts/LICENSE.txt`. That license is included in every packaged
platform dependency inventory. Navigation and menu artwork is reused from Nova
under the project's existing license and attribution.
