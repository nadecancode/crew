# Output-view regression inputs

These five files are exact decoded entries from
`notes/token-savers/corpus.json.gz`, archive SHA-256
`be068c08663e1f254e389b3ab5fbb3b798858db59dd337e53a78a8987312b36b`.
The successful and failed logs are real captured crew test output; the
zero-test, unknown-runner and JSON entries are synthetic negative controls.
The warning fixture is reconstructed in `tests/output_view.rs` by prepending
the corpus's exact warning line to the successful log, avoiding a duplicate.

The decoded inputs keep crate tests independent of Node/Python, gzip tools
and new decompressor dependencies. Original output, including its printed
paths and line endings, is retained for exact-recovery checks.

| File | SHA-256 |
| --- | --- |
| `cargo-success.txt` | `2cdc945ccee2ddf127911941fd632e012a160f457a81682b642751abb8a9b1a9` |
| `cargo-failure.txt` | `9ce3bf2ab065f9b14ffb1f68611a0b04d6d0245c8579b923b6c087f5712e0e7a` |
| `cargo-zero.txt` | `4432340a47af682d6cd96ee3554ff0f0a11af8c7a4da23192d65af1d514d7d14` |
| `cargo-unknown.txt` | `72caeab04b69f15e696ecded7d1e7598c5217f792f0f7ef0e636c0477cf986ca` |
| `cargo-json.txt` | `bc75ed2b68b4d808428f42c83642b8f577bb9897ab55ccd7e21f11820738b29d` |
