# Vendored crates and the patches on them

Two of hayro's crates are vendored here, with small changes the PDF reader
needs. `Cargo.toml` points at them with `[patch.crates-io]`. Everything
else of hayro still comes from crates.io.

| Crate | Upstream version | Upstream commit | Licence |
|---|---|---|---|
| `hayro-interpret` | 0.8.0 | `ea9c81dc70cde40c64205986fdcfcd481fe934d8` | Apache-2.0 OR MIT |
| `hayro-cmap` | 0.1.0 | `a2354aebaf86f5ab13e5fd6b6b0521306488bf9d` | Apache-2.0 OR MIT |

The copies are the crates.io packages as published, apart from the
patches below. They keep their `LICENSE-APACHE`, `LICENSE-MIT`, README and
assets. Among those assets are the standard fonts hayro ships (Foxit's),
which every build already contained.

Each modified file says so on its first line (Apache-2.0 §4(b)):
`// Modified by MimiReader: <what>. See vendor/PATCHES.md.`

## The patches

Every patch is as small as it can be, only reads more of what the file
already says, and has a test that fails without it.

None yet.

### Considered and not made

- **A ToUnicode stream compressed without a /Filter** (issue19802). The fix
  needs hayro-syntax's decoder, which isn't public, so a third crate would
  have to be vendored. That is too much for one page whose text xpdf can't
  read either.

## Upgrading hayro

1. Bump `hayro` in `Cargo.toml`. The copies are `hayro-interpret` 0.8.0 and
   `hayro-cmap` 0.1.0. A new hayro that needs newer versions leaves the
   `[patch.crates-io]` entries unused: cargo only warns ("patch ... was not
   used") and builds against crates.io. **The loud failure is the patches'
   tests**, which fail as soon as the build runs without the patches.
2. For each patch above, check upstream's changelog or source for the same
   fix. If upstream has it, drop the patch: delete its section here, and
   when no patch is left in a crate, delete the copy and its
   `[patch.crates-io]` line.
3. Otherwise copy the new upstream package over the vendored crate (from
   `~/.cargo/registry/src/.../<crate>-<version>`) and re-apply each
   remaining patch with its notice line. Update the table above to the new
   version and commit.
4. Run PDFReader's tests. Each patch's test must pass, and fail again with
   the patch removed. Then run the corpus sweep
   (`MimiReader/crates/reader-host/examples/pdf_sweep.rs`) and compare it
   with the last one: no page that agreed before may disagree now.

Offering these patches upstream is the user's decision; none has been
proposed yet.
