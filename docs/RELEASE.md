# Release Process

gest releases are automated by GitHub Actions when a version tag is pushed.
The goal is to ship a single `gest` binary per platform built from the tagged
source.

## Steps

1. Update version in `Cargo.toml`.
2. Run checks locally:

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

3. Optionally build and verify locally:

```bash
cargo build --release
./target/release/gest --version
```

4. Create a git tag and push it:

```bash
git tag -a vX.Y.Z -m "gest vX.Y.Z"
git push origin vX.Y.Z
```

5. GitHub Actions will build and publish the release with binaries for
   Linux, macOS, and Windows.
