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

Tip: after running, open the values file to verify updates.
