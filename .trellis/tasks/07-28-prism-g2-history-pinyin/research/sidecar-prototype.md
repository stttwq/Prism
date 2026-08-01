# Pinyin Sidecar Prototype

## Decision

Select the versioned compact record table with a read-only mapped source file,
a decoded compact hot-query table, and a bounded in-memory rename/delete
overlay. `NodeSlot` remains 12 bytes. Postcard does not provide a zero-copy
`Vec` view, so search reads the decoded heap payload while the mapping remains
open for file lifetime/accounting. This is deliberately not claimed as an
mmap-primary representation. The file contains the schema, dictionary version,
index generation and identity, record offsets, compact token/span payload, and
checksum. A delta is rebuilt internally at 4,096 records.

No generated sidecar is checked into the repository. `pinyin-v1.bin` is a
machine-local derived file written with flush plus atomic replacement and is
covered by `{commonappdata}\Prism` uninstall cleanup.

## Measurement

Command, Release `opt-level="z"`, 2026-08-01:

```text
cargo run --release --manifest-path src/prism-core/Cargo.toml \
  --bin pinyin-sidecar-prototype --offline -- C:\ E:\
```

The harness walked the same machine's current eligible filesystem names,
skipped the indexer's high-noise directory names, encoded the exact production
record/payload shape, modeled both the mapped bytes and decoded compact tables,
and ran 30 warm full scans.

| Metric | Measured |
| --- | ---: |
| Names scanned | 368,685 |
| Walk errors skipped | 226 |
| Names with pinyin records | 272 |
| Encoded mapped bytes | 24,572 B |
| Mapped + decoded resident model | 51,394 B |
| Build time | 901.085 ms |
| ASCII no-hit sidecar scan P95 | 0.0890 ms |
| `weixin` sidecar scan P95 | 0.0846 ms |

The 51,394-byte result includes both encoded mapped bytes and decoded compact
table capacity. It is 0.49% of the 10 MiB sidecar gate, so the source-level
representation gate passes with substantial headroom on the current machine
corpus. The archived G0 run reports 660,751 name candidates, but its exact name
stream was not retained; this report does not present a scaled estimate as a
measurement.

## Remaining Machine Gate

The final acceptance still requires an installed three-process Release sample:

- pinyin enabled and warmed;
- pinyin disabled after preference propagation, proving the mmap/overlay is
  released;
- synchronized `WorkingSetPrivate` totals at or below 100 MiB and a measured
  pinyin delta at or below 10 MiB;
- end-to-end warm `max=8` and `max=1000` P95 results.

That installed-service evidence is intentionally separate from this storage
prototype. It cannot be replaced by the resident model above.
