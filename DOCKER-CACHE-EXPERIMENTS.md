# Faber Docker Cache Experiments

This fork contains controlled experiments for the slow Rust API image build.
Run the `Docker cache experiments` workflow manually with `variant=both`.

The workflow compares two otherwise equivalent one-build API Dockerfiles:

- `Dockerfile.experiment-layer`: Cargo writes compiled output into the normal
  BuildKit layer. `type=gha,mode=max` must restore this intermediate layer.
- `Dockerfile.experiment-target-mount`: Cargo writes compiled output to a
  BuildKit `type=cache` mount. The mount is mutable state and is not part of an
  image layer.

Each variant has its own GitHub Actions cache scope. Run the same workflow again
without changing the commit and compare whether the build `RUN` instruction is
`CACHED` and whether Cargo emits any `Compiling` lines.

## Interpretation

| Observation | Meaning |
| --- | --- |
| Both variants are `CACHED` on an identical rerun | The layer cache works for unchanged Docker inputs. |
| Layer variant is `CACHED`, target-mount variant recompiles | The GHA layer cache works, but the mutable target mount is not restored as a compiled artifact. |
| Both variants recompile on an identical rerun | Investigate cache scope, cache quota/eviction, workflow permissions, or cache exporter errors. |
| Only application crates compile after a source change | Cargo successfully reused dependency artifacts. |
| Registry downloads are absent but third-party crates compile | Download cache works; compiled target cache does not. |

The experiment intentionally does not build or push the production agent image.
Faber's static musl `faber-agent` build is a separate target and should be
measured separately from API dependency caching.
