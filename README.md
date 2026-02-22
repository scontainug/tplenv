# tplenv

`tplenv` renders `{{...}}` placeholders in a template file.

- `{{VARNAME}}` reads from environment variables (or from `environment.VARNAME` with `--value-file-only`).
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
tplenv (--file INPUT.yaml | --file-pattern "<NUM>-*.yaml") [--values Values.yaml] [--output OUTPUT.yaml] [--value-file-only]
```

Options:

- `-f, --file <PATH>`: input template file (required)
- `--file-pattern <PATTERN>`: match multiple input files by filename pattern (supports `*` and `<NUM>`, e.g. `<NUM>-*.yaml`)
- `--values <PATH>`: values YAML file (default: `Values.yaml`)
- `-o, --output <PATH>`: output file (`-` or omitted means stdout)
- `-v, --verbose`: print substitutions to stderr
- `--create-values-file`: ask for missing `.Values.*` placeholders and write/update the values file
- `--force`: only valid with `--create-values-file`; asks for all `.Values.*` placeholders and uses existing values as prompt defaults
- `--value-file-only`: resolve `{{VARNAME}}` from `environment.VARNAME` in the values file (do not read OS environment variables)
- `-h, --help`: print help
- `--version`: print version

Notes:

- Use either `--file` or `--file-pattern`.
- With multiple matched files, output is a YAML multi-document stream (`---` separators), to stdout or to `--output <FILE>`.
- Multi-file mode fails unless all matched input files end with `.yaml`.

## Examples

Render using environment variables and values file:

```bash
tplenv --file deployment.tpl.yaml --values Values.yaml --output deployment.yaml
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
tplenv --file deployment.tpl.yaml --values Values.yaml --value-file-only
```

With `--value-file-only`, interactive mode also manages missing `environment.*` keys:

```bash
tplenv --file deployment.tpl.yaml --values Values.yaml --value-file-only --create-values-file
```

Process all numbered YAML files in a directory:

```bash
tplenv --file-pattern "examples/06-file-pattern/<NUM>-*.yaml" --values examples/06-file-pattern/Values.yaml --value-file-only
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
