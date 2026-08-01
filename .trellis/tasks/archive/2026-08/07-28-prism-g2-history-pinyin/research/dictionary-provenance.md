# Pinyin Dictionary Provenance

## Selected Source

- Rust package: `pinyin` 0.10.0
- Upstream: <https://github.com/mozillazg/rust-pinyin>
- Package license: MIT, copyright 2016 mozillazg
- Embedded character data: `pinyin-data/pinyin.txt` version 0.13.0
- Character-data upstream: <https://github.com/mozillazg/pinyin-data>
- Cargo feature set: `default-features = false`, `features = ["plain"]`

The crate package declares MIT for the distributed package and includes both
`LICENSE` and `pinyin-data/pinyin.txt`. The embedded data file identifies its
version and upstream in its first two lines. `Cargo.lock` records the exact
crate checksum used by the build.

## Phrase Rules

The crate provides character readings, including heteronym data when that
feature is selected, but it does not resolve readings from word context. Prism
therefore owns a small, reviewed longest-match phrase table in
`src/prism-core/src/pinyin.rs`. Its version is part of
`PINYIN_DICTIONARY_VERSION`; changing a phrase or its reading requires bumping
that version so an old sidecar is rejected and rebuilt.

The phrase entries are project-authored pronunciation facts rather than copied
dictionary prose. They cover the fixed regression corpus and common filename,
application, and place-name cases. Unmatched characters use the crate's
versioned default reading.

## Fixed Corpus

The table-driven Rust tests cover:

- simplified and traditional forms (`微信`, `軟體`);
- phrase polyphones (`重庆`/`重慶`, `音乐`/`音樂`, `银行`/`銀行`);
- default character readings (`中国`);
- full pinyin, initials, syllable-boundary substrings and partial suffixes;
- mixed Latin/digit suffixes, case, spaces, apostrophes, and `v`/`ü`;
- UTF-16 highlight spans.

## Generated-Asset Gate

No generated dictionary or sidecar is committed at this step. Before a
generated sidecar can be retained, the prototype report must record measured
bytes, build time, literal-query P95, pinyin-query P95, and release-process
Private Working Set deltas against the G0 corpus. The selected representation
must keep the three-process pinyin delta at or below 10 MiB and satisfy the
task's `max=8`/`max=1000` latency limits.
