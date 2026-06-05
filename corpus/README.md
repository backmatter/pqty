# Acceptance corpus

The Linux corpus covers five project shapes:

| Project | Engine/tooling | Boundary |
| --- | --- | --- |
| `pdflatex-bibtex` | pdfLaTeX + BibTeX | Classic bibliography |
| `lualatex-fonts` | LuaLaTeX + fontspec | Unicode engine and fonts |
| `xelatex-tikz` | XeLaTeX + TikZ | Graphics and TikZ |
| `pdflatex-biber` | pdfLaTeX + Biber | biblatex and Biber |
| `runtime-local` | pdfLaTeX + `pqty-fls` | Local files and convergence |

Run every case:

```sh
cargo xtask tex corpus
```

Pass project names for a focused run, for example:

```sh
cargo xtask tex corpus runtime-local
```

The xtask writes
`target/corpus-metrics.json` with provider counts, static-scan misses,
convergence rounds, unwanted providers, lock bytes, and store bytes.

Required TeX Live packages and tools are listed in the installation step of
`.github/workflows/ci.yml`. With no arguments, all five cases and their tools
are mandatory.

The corpus checks pqty's package-layer boundary. Engine binaries,
configuration, bibliography-tool invocation, system-font discovery, and
compilation-pass scheduling remain renderer responsibilities.
