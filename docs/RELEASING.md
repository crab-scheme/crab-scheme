# Releasing CrabScheme

Release candidates are cut as annotated git tags `1.0-rcN`, which trigger
`.github/workflows/release.yml` to build and attach the per-target tarballs
(native binaries + `libcs_aot_rt.a` + the LSP/MCP servers, and the WASM
build). Each RC bumps the workspace version and adds a `CHANGELOG.md` entry.

## Cutting `1.0-rcN`

1. **Land the content.** Merge the PRs for the RC into `main` and ensure CI
   is green.

2. **Update the version.** Bump `[workspace.package].version` in the root
   `Cargo.toml` to `1.0.0-rcN` (Cargo SemVer; the tag drops the patch `.0`).
   Run a build so `Cargo.lock` picks up the new version:

   ```bash
   cargo build --workspace
   ```

3. **Write the changelog.** Add a `## [1.0-rcN] — YYYY-MM-DD` section at the
   top of `CHANGELOG.md` (under the intro), grouped into
   Added / Changed / Fixed, with PR references. A quick way to see the delta:

   ```bash
   git log --merges --format='%s' 1.0-rc<N-1>..HEAD   # PRs since last RC
   git log --no-merges --oneline 1.0-rc<N-1>..HEAD    # individual commits
   ```

   Add the `[1.0-rcN]: …/releases/tag/1.0-rcN` link reference at the bottom.

4. **Commit the prep** (version + changelog) on `main`:

   ```bash
   git commit -am "release: 1.0-rcN — version bump + changelog"
   ```

5. **Tag and push.** Annotate the tag and push it; the release workflow does
   the rest (it derives the version from the tag and auto-generates GitHub
   release notes alongside the curated changelog):

   ```bash
   git tag -a 1.0-rcN -m "Crab Scheme 1.0-rcN"
   git push origin main 1.0-rcN
   ```

6. **Verify.** Watch the `release` workflow; confirm each tarball contains
   the expected artifacts and (on a native target) smoke-test toolchain-free
   AOT from the packaged binary:

   ```bash
   tar xzf crabscheme-1.0-rcN-<target>.tar.gz && cd crabscheme-1.0-rcN-<target>
   ./crabscheme aot-doctor          # both AOT back-ends self-test green
   echo '(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))' > fib.scm
   ./crabscheme aot fib.scm --build -o fib && ./fib 25   # → 75025
   ```

## Version mapping

| Git tag    | Cargo version |
|------------|---------------|
| `1.0-rcN`  | `1.0.0-rcN`   |
| `1.0`      | `1.0.0`       |

The release workflow triggers on any tag matching `1.*`.
