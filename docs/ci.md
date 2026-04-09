## CI behavior

Build artifacts now use the normal in-tree Cargo `target/` directory. Kernel
module build output lives under `build/<kernel-version>/`, and VM state lives
under `vm/`. All three directories are git-ignored.

The GitHub Actions `CI` workflow includes a chore job that may update tracked
submodule SHAs automatically. The updater scans `.gitmodules` for configured
submodule URLs, resolves each submodule's current `HEAD`, and stages the
matching gitlink updates.

If a submodule remote cannot be queried or returns an empty `HEAD` SHA, the
workflow fails immediately instead of warning and continuing with partial
updates.

Before staging each submodule gitlink, the workflow prints the resolved `HEAD`
SHA it is about to write.

When that job creates a bot-authored maintenance commit (`chore: auto-fix lint`
or `chore: update submodules`), it appends `[skip ci]` to the commit message so
the resulting push does not trigger a follow-up CI run.
