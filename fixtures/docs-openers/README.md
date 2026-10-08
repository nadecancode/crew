# Docs opener platform fixture

This small crate compiles the production `os.rs`, Docs policy, safe source reader
and their real unit tests. It avoids unrelated daemon C/PTY dependencies when
checking a macOS target from Linux:

```sh
cargo check --locked --manifest-path fixtures/docs-openers/Cargo.toml \
  --tests --target aarch64-apple-darwin
cargo check --locked --manifest-path fixtures/docs-openers/Cargo.toml \
  --tests --target x86_64-apple-darwin
```

Cross-compilation does **not** execute macOS's `O_NOFOLLOW_ANY` opener. On a
macOS runner, execute `cargo test --locked --manifest-path
fixtures/docs-openers/Cargo.toml` as the separate K04 platform gate. Linux and
macOS use the same symlink, hard-link, FIFO, traversal and replaced-root tests.
