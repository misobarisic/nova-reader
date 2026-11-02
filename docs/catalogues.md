# Catalogue discovery

`reader-catalogue` separates native JavaScript ZIP distribution from upstream
Keiyoushi discovery. It never downloads or executes APKs/JARs, installs ZIPs,
activates packages, or runs unattended updates.

The pinned schema is `.github/scripts/index.proto` from Keiyoushi
extensions-source revision `0d83f6be79e12b0a8b6c6f20a500c0e9456c222a`,
preserved in `crates/reader-catalogue/fixtures/index.proto`. Hand-authored prost
0.14 declarations match its tags, including JAR URL tag 501, source message tag
7, and embedded/list-URL oneof tags 101/102. No protoc is needed. Unknown fields
are ignored and unknown content-warning values remain visible as integers.

## Verified live index

On 2026-10-31 the actual live gzip protobuf was downloaded through the managed
HTTP proxy from
`https://raw.githubusercontent.com/keiyoushi/extensions/repo/index.pb`.
Its compressed SHA-256 was
`acaae63ab34af917e45687bbac4eedf160dc1f110c8084d84721b5eaafe17c4f`.
The immutable repository revision
`e3dd05c459154d7a32659305f48ae49853c60291` was subsequently downloaded
separately and its index.pb matched the same SHA-256. The Rust decoder
successfully read all 1,443 extensions from that full index.

The byte-exact Manhwa18.cc extension record and repository metadata were
extracted into the small, reproducible fixture
`crates/reader-catalogue/fixtures/keiyoushi-manhwa18.pb.gz`, using gzip mtime 0.
The package is `eu.kanade.tachiyomi.extension.all.manhwa18cc`, version
`1.6.66`, version code `106066`, extension library `1.6`, content warning
`3` (NSFW), home `https://manhwa18.cc`.

| Language | Actual source ID | Native mapping |
| --- | --- | --- |
| en | `4841602236575491202` | addon `manhwa18cc`, source `manhwa18cc.en` |
| ko | `6864643444164576223` | addon `manhwa18cc`, source `manhwa18cc.ko` |
| all | `993336618407081332` | addon `manhwa18cc`, source `manhwa18cc.all` |

The MangaFire port adds seven exact language mappings and its byte-preserved
`keiyoushi-mangafire.pb.gz` record. Source IDs, index hash and the native port's
capabilities and live-test limits are recorded in [nova-addons MangaFire documentation](https://github.com/misobarisic/nova-addons/blob/main/docs/reader/mangafire.md).

IDs and upstream version codes are decimal strings at JSON boundaries, including
negative/extreme signed int64 values. The checked-in mapping requires the exact
upstream repository, package and source ID. Upstream and native versions are
independent. The mapping alone does not mark a port available: the verified
native catalogue must actually advertise that addon/source pair. Upstream
signing metadata authenticates neither a Nova ZIP nor its catalogue.

## Fetch and decode contract

`decode_keiyoushi_gzip(repository, bytes)` validates embedded indexes.
`CatalogueClient::new()?.fetch_keiyoushi(url)` additionally resolves linked
indexes and bare ExtensionLists. All repository URLs use HTTPS without embedded
credentials; relative list URLs resolve against the current document. HTTP
redirects have a five-hop limit and cannot downgrade to HTTP. Total compressed
bytes across the document graph are bounded to 8 MiB, total decompressed bytes
to 32 MiB, records/sources to 20,000 and linked documents to four. Repeated URLs
and cycles fail. Concatenated gzip members share the decompression limit.

The transport sends `If-None-Match` for cached ETags and accepts 304 only for a
cached response. Cache replacement occurs only after the complete graph decodes
successfully; a failed refresh retains previous cached bytes. The in-memory
cache contains only the latest successful graph. The application should persist
trusted keys and any longer-lived cache according to its own storage policy.

## Native catalogue v1

The signed payload is ordinary UTF-8 JSON with this structure:

```json
{
  "format_version": 1,
  "repository": "publisher-chosen-stable-identity",
  "sequence": 1,
  "packages": [{
    "id": "manhwa18cc",
    "name": "Manhwa18.cc",
    "version": "0.1.0",
    "api": "^1.0",
    "zip_url": "https://example.org/manhwa18cc-0.1.0.zip",
    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "sources": [{
      "id": "manhwa18cc.en",
      "name": "Manhwa18.cc",
      "language": "en",
      "nsfw": true,
      "capabilities": ["browse", "search", "details", "chapters", "pages"]
    }]
  }]
}
```

The example checksum and URL are illustrative, not a published package.
`verify_native_catalogue(bytes, key_id, detached_signature_hex, &trust)` requires
an explicitly configured Ed25519 public key bound to its repository identity
in `TrustStore::trust_key(repository, key_id, public_key_bytes)`. It verifies the
exact original bytes before parsing JSON, rejects invalid signatures and unknown
keys, enforces format/URL/hash/duplicate-record limits, and applies the configured
minimum repository sequence. Native catalogue bytes are limited to 32 MiB.

Key IDs cannot overwrite existing trust. Key rotation requires an explicit
application/user decision to add/remove keys; a signed catalogue cannot grant
itself trust. Sequence floors only increase through `advance_sequence`; callers
persist the floor after accepting a catalogue. This discovery API does not
implement time-based freshness or automatic update policy.

`native_state` reports available, installed, update available or incompatible
based on the installed package version, reader API and known capabilities.
`upstream_state` reports port available, installed native port or not yet ported.
A source which lacks a native port gets an actionable explanation, never an
APK/JAR install action. Future ZIP delivery must verify the advertised SHA-256,
validate the existing reader package format, show permission changes, atomically
activate only validated packages, and retain the prior installation on failure.
Those installation transitions remain owned by `reader-sources`; discovery
performs no filesystem activation.

The fixture is publisher metadata derived from Keiyoushi's Apache-2.0 source
schema; its upstream provenance remains recorded here and in `fixtures/index.proto`,
with the upstream license retained as `fixtures/LICENSE-APACHE-2.0`.
The Reader implementation is GPL-3.0-or-later.
