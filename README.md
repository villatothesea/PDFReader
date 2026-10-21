# PDFReader

The PDF reader plugin for [MimiReader](../MimiReader). PDF pages are rasterised with
[hayro](https://crates.io/crates/hayro) (MIT/Apache-2.0).

A plugin is a separate executable that speaks line-delimited JSON over stdio. The
protocol and the serve loop come from `mireader-reader-sdk`. A package is a folder
holding `reader.toml` and the executable, found and hot-swapped by
`mireader-reader-host`. Both crates live in the reader's repository, and this repo
expects it as a sibling folder:

```text
E:\Villa\VibeCoding\MimiReader\
  MimiReader\   the reader (crates\reader-sdk, crates\reader-host)
  PDFReader\    this repository
  Agent\        the agent parts of Zeron
```

## Build and test

```powershell
cargo test -j 2      # unit tests, the host end to end, and plugin mode (install → open → hot swap → uninstall)
.\stage.ps1          # build and stage into ..\MimiReader\target\debug\readers\mireader.pdf
.\stage.ps1 -Release -Out D:\Programs\MimiReader\readers
```

Licence: MIT (see `LICENSE`).
