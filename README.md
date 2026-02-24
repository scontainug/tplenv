# tplenv

`tplenv` renders placeholders in template files.

- `{{VARNAME}}`, `$VARNAME`, and `${VARNAME}` read from environment variables (or from `environment.VARNAME` with `--value-file-only`).
- `{{ .Values.key }}` reads from a YAML values file.

## Install

To install `tplenv` on your computer, just run:

```bash
cargo install --path .
```

## Building

```bash
cargo build --release
```

Binary path:

```bash
./target/release/tplenv
```

## Help

Show CLI usage:

```bash
tplenv --help
```

## Usage

```bash
tplenv (--file INPUT.yaml | --file-pattern "<NUM>-*.yaml") [--values-file Values.yaml] [--output OUTPUT.yaml] [--value-file-only]
```

Options:

- `-f, --file <PATH>`: input template file (required)
- `--file-pattern <PATTERN>`: match multiple input files by filename pattern (supports `*` and `<NUM>`, e.g. `<NUM>-*.yaml`)
- `--values-file <PATH>`: values YAML file (default: `Values.yaml`, alias: `--values`)
- `-o, --output <PATH>`: output file (`-` or omitted means stdout)
- `-v, --verbose`: print substitutions to stderr
- `--create-values-file`: ask for missing placeholders and write/update the values file (`$VAR`/`${VAR}` are stored as `environment.VAR`)
  - In this mode, `environment.VAR` from the values file has priority over OS environment variables.
- `--force`: only valid with `--create-values-file`; asks for all `.Values.*` placeholders and uses existing values as prompt defaults
- `--value-file-only`: resolve `{{VARNAME}}` from `environment.VARNAME` in the values file (do not read OS environment variables)
- `--eval`: only with `--create-values-file`; print prompted keys as bash `export` lines (useful with `eval "$( ... )"`)
  - If `--output <FILE>` is also set, the rendered YAML is still written to that file while exports are printed to stdout.
- `--indent`: when a replacement value contains multiple lines, tplenv emits YAML block scalars automatically (`|` or `|+` for trailing empty lines) and keeps indentation valid
- `-h, --help`: print help
- `--version`: print version

Notes:

- Use either `--file` or `--file-pattern`.
- With multiple matched files, output is a YAML multi-document stream (`---` separators), to stdout or to `--output <FILE>`.
- Multi-file mode fails unless all matched input files end with `.yaml`.

## Examples

Render using environment variables and values file:

```bash
tplenv --file deployment.tpl.yaml --values-file Values.yaml --output deployment.yaml
```

Create/update only missing values in `Values.yaml`, then render:

```bash
tplenv --file deployment.tpl.yaml --create-values-file
```

Ask for all values found in the template and prefill from existing file values:

```bash
tplenv --file deployment.tpl.yaml --create-values-file --force
```

Resolve `{{VARNAME}}` from the values file:

```bash
tplenv --file deployment.tpl.yaml --values-file Values.yaml --value-file-only
```

With `--value-file-only`, interactive mode also manages missing `environment.*` keys:

```bash
tplenv --file deployment.tpl.yaml --values-file Values.yaml --value-file-only --create-values-file
```

Process all numbered YAML files in a directory:

```bash
tplenv --file-pattern "examples/06-file-pattern/<NUM>-*.yaml" --values-file examples/06-file-pattern/Values.yaml --value-file-only
```

Generate bash exports from prompted values:

```bash
eval "$(tplenv --file deployment.tpl.yaml --create-values-file --eval)"
```

With `--force`, all prompted keys are exported (for example `image.tag` -> `IMAGE_TAG`, `environment.APP_NAME` -> `APP_NAME`).

Keep multiline values aligned:

```bash
tplenv --file deployment.tpl.yaml --values-file Values.yaml --indent
```

More runnable examples are in `examples/README.md`.
That includes a `--value-file-only --create-values-file --force` interactive example with defaults.

## Tests

Run unit tests:

```bash
cargo test
```

## Nix (Deterministic Build)

This repo now includes a flake-based build:

- `/Users/christoffetzer/Library/Mobile Documents/com~apple~CloudDocs/GIT/scontainug/tplenv/flake.nix`
- `/Users/christoffetzer/Library/Mobile Documents/com~apple~CloudDocs/GIT/scontainug/tplenv/default.nix`

Build with Nix:

```bash
nix build .#tplenv
```

Enter development shell:

```bash
nix develop
```

For deterministic builds across machines, commit `flake.lock` to the repo.
If it does not exist yet, generate it once with:

```bash
nix flake lock
```
