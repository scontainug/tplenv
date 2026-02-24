# Examples

Run commands from the project root.

## 1) Basic render (env + values)

```bash
export APP_NAME=demo-api
export NAMESPACE=staging
cargo run -- --file examples/01-basic-render/template.yaml --values examples/01-basic-render/Values.yaml
```

Write to a file:

```bash
cargo run -- --file examples/01-basic-render/template.yaml --values examples/01-basic-render/Values.yaml --output examples/01-basic-render/rendered.yaml
```

## 2) Create missing values interactively

This example starts with a partial values file. `--create-values-file` asks only for missing `.Values.*` paths and updates the file.

```bash
export APP_NAME=demo-config
export NAMESPACE=dev
cargo run -- --file examples/02-create-values/template.yaml --values examples/02-create-values/Values.yaml --create-values-file
```

## 3) Force prompt for all values (with defaults)

`--force` asks for every `.Values.*` path found in the template. If a value already exists, pressing Enter keeps it.

```bash
export APP_NAME=demo-secret
export NAMESPACE=prod
cargo run -- --file examples/03-force-update/template.yaml --values examples/03-force-update/Values.yaml --create-values-file --force
```

## 4) Value file only mode (no OS environment variables)

`--value-file-only` resolves `{{VARNAME}}` from `environment.VARNAME` inside the values file.

```bash
cargo run -- --file examples/04-value-file-only/template.yaml --values examples/04-value-file-only/Values.yaml --value-file-only
```

Create or update missing values, including `environment.*` keys used by `{{VARNAME}}`:

```bash
cargo run -- --file examples/04-value-file-only/template.yaml --values examples/04-value-file-only/Values.yaml --value-file-only --create-values-file
```

## 5) Value file only + force (interactive with defaults)

This values file already contains defaults. `--force` asks for every referenced key anyway, and pressing Enter keeps the default shown in brackets.

```bash
cargo run -- --file examples/05-value-file-only-force/template.yaml --values examples/05-value-file-only-force/Values.yaml --value-file-only --create-values-file --force
```

Typical prompts look like:

```text
Enter value for values file key environment.APP_NAME [default-app]:
Enter value for values file key environment.NAMESPACE [default-ns]:
Enter value for values file key image.tag [1.0.0]:
```

## 6) File pattern + shared values prompts across files

This processes multiple files in one run. Prompts are deduplicated across all matched files because they share one values file.

```bash
cargo run -- --file-pattern "examples/06-file-pattern/<NUM>-*.yaml" --values examples/06-file-pattern/Values.yaml --value-file-only --create-values-file --force
```

If two files use the same key (for example `{{APP_NAME}}`), that key is only prompted once.
Rendered output is a YAML multi-document stream, so you can also write one combined file:

```bash
cargo run -- --file-pattern "examples/06-file-pattern/<NUM>-*.yaml" --values examples/06-file-pattern/Values.yaml --value-file-only --output examples/06-file-pattern/rendered.yaml
```

## 7) Emit bash exports with --eval

Use this when you want prompted values to become environment variables in your current shell.

```bash
eval "$(cargo run -- --file examples/05-value-file-only-force/template.yaml --values examples/05-value-file-only-force/Values.yaml --value-file-only --create-values-file --force --eval)"
```

This prints lines like:

```text
export APP_NAME='default-app'
export IMAGE_TAG='1.0.0'
```

## 8) Keep multiline values aligned with --indent

This preserves indentation when a value spans multiple lines.

```bash
cargo run -- --file examples/07-indent/template.yaml --values examples/07-indent/Values.yaml --indent
```

You can write the rendered file too:

```bash
cargo run -- --file examples/07-indent/template.yaml --values examples/07-indent/Values.yaml --indent --output examples/07-indent/rendered.yaml
```

Tip: after running, open the values file to verify updates.
