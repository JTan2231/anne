# Build Contract

Anne's default build is a local-only `git2`/vendored-`libgit2` build. The checked-in contract is the Unix-like development and CI path exercised by `./ci.sh`.

## Required

- Rust `1.85+` with `cargo`
- A native C toolchain and linker so Cargo can compile the vendored `libgit2`

## Not Required By Default

- The system `git` executable
- OpenSSL headers or development packages
- libssh2 headers or development packages
- A system-installed `libgit2`
- The system `pkg-config` executable

## Dependency Scope

Anne uses `git2` only for local repository work: repository discovery/open, ref resolution, merge-base lookup, diff generation, and local repository setup inside tests. The default Cargo feature set therefore disables `git2`'s `https` and `ssh` defaults and keeps only `vendored-libgit2`.

`Cargo.lock` still includes the Rust `pkg-config` crate through `libgit2-sys`'s build dependencies. That crate supports non-default system-library probe paths; the default vendored Anne build does not require the host `pkg-config` command.

## Verification

Run:

```sh
./ci.sh
```

That entrypoint verifies the default `git2` feature surface, fails if `openssl-sys` or `libssh2-sys` reappear in the resolved graph, then runs `cargo build --locked` and `cargo test --locked`.

Windows is not part of this checked-in verified build contract yet.
