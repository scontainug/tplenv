#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

cp -R examples "$WORK_DIR/"
EX_DIR="$WORK_DIR/examples"

# Keep this example deterministic even if committed sample values changed.
cat > "$EX_DIR/05-value-file-only-force/Values.yaml" <<'EOF'
environment:
  APP_NAME: default-app
  NAMESPACE: default-ns
  APP_MODE: safe
  REGION: eu-central-1
image:
  repository: ghcr.io/example/demo
  tag: "1.0.0"
replicas: 2
EOF

run_tplenv() {
  cargo run --quiet -- "$@"
}

assert_contains() {
  local file="$1"
  local needle="$2"
  if ! grep -Fq -- "$needle" "$file"; then
    echo "Assertion failed: '$needle' not found in $file" >&2
    echo "--- $file ---" >&2
    cat "$file" >&2
    exit 1
  fi
}

assert_prompt_order() {
  local file="$1"
  shift
  local last=0
  local needle line
  for needle in "$@"; do
    line="$(grep -nF -- "$needle" "$file" | head -n1 | cut -d: -f1)"
    if [[ -z "$line" ]]; then
      echo "Assertion failed: prompt '$needle' not found in $file" >&2
      echo "--- $file ---" >&2
      cat "$file" >&2
      exit 1
    fi
    if (( line <= last )); then
      echo "Assertion failed: prompt order incorrect for '$needle' in $file" >&2
      echo "--- $file ---" >&2
      cat "$file" >&2
      exit 1
    fi
    last=$line
  done
}

echo "Running examples in $EX_DIR"

# 1) Basic render (env + values)
APP_NAME=demo-api NAMESPACE=staging run_tplenv \
  --file "$EX_DIR/01-basic-render/template.yaml" \
  --values-file "$EX_DIR/01-basic-render/Values.yaml" \
  --output "$WORK_DIR/out-01.yaml"
assert_contains "$WORK_DIR/out-01.yaml" "name: demo-api"
assert_contains "$WORK_DIR/out-01.yaml" "namespace: staging"
assert_contains "$WORK_DIR/out-01.yaml" "image: nginx:1.27"

# 2) Create missing values interactively (no prompt expected here, env copied to values file)
APP_NAME=demo-config NAMESPACE=dev run_tplenv \
  --file "$EX_DIR/02-create-values/template.yaml" \
  --values-file "$EX_DIR/02-create-values/Values.yaml" \
  --create-values-file \
  --output "$WORK_DIR/out-02.yaml"
assert_contains "$WORK_DIR/out-02.yaml" "name: demo-config-config"
assert_contains "$EX_DIR/02-create-values/Values.yaml" "environment:"
assert_contains "$EX_DIR/02-create-values/Values.yaml" "APP_NAME: demo-config"

# 3) Force prompt for all values
printf 'db.host.local\nforced-db\nforced-pass\nforced-user\nforced-app\nforced-ns\n' | run_tplenv \
  --file "$EX_DIR/03-force-update/template.yaml" \
  --values-file "$EX_DIR/03-force-update/Values.yaml" \
  --create-values-file \
  --force \
  --output "$WORK_DIR/out-03.yaml" >/dev/null
assert_contains "$WORK_DIR/out-03.yaml" "name: forced-app-db"
assert_contains "$WORK_DIR/out-03.yaml" "host: db.host.local"
assert_contains "$WORK_DIR/out-03.yaml" "user: forced-user"

# 4) Value file only mode
run_tplenv \
  --file "$EX_DIR/04-value-file-only/template.yaml" \
  --values-file "$EX_DIR/04-value-file-only/Values.yaml" \
  --value-file-only \
  --output "$WORK_DIR/out-04.yaml"
assert_contains "$WORK_DIR/out-04.yaml" "name: values-driven-app-runtime"
assert_contains "$WORK_DIR/out-04.yaml" "namespace: qa"
assert_contains "$WORK_DIR/out-04.yaml" "app_mode: read-only"

# 5) Value file only + force (accept all defaults)
printf '\n\n\n\n\n\n\n' | run_tplenv \
  --file "$EX_DIR/05-value-file-only-force/template.yaml" \
  --values-file "$EX_DIR/05-value-file-only-force/Values.yaml" \
  --value-file-only \
  --create-values-file \
  --force \
  --output "$WORK_DIR/out-05.yaml" >/dev/null
assert_contains "$WORK_DIR/out-05.yaml" "name: default-app-settings"
assert_contains "$WORK_DIR/out-05.yaml" "region: eu-central-1"
assert_contains "$WORK_DIR/out-05.yaml" "image_repo: ghcr.io/example/demo"

# 6) File pattern + multi-document YAML output
run_tplenv \
  --file-pattern "$EX_DIR/06-file-pattern/<NUM>-*.yaml" \
  --values-file "$EX_DIR/06-file-pattern/Values.yaml" \
  --value-file-only \
  --output "$WORK_DIR/out-06.yaml"
assert_contains "$WORK_DIR/out-06.yaml" "---"
assert_contains "$WORK_DIR/out-06.yaml" "name: multi-demo-config"
assert_contains "$WORK_DIR/out-06.yaml" "name: multi-demo-db"

# 7) Eval exports
eval_stdout="$WORK_DIR/eval-stdout.txt"
eval_stderr="$WORK_DIR/eval-stderr.txt"
printf '\n\n\n\n\n\n\n' | run_tplenv \
  --file "$EX_DIR/05-value-file-only-force/template.yaml" \
  --values-file "$EX_DIR/05-value-file-only-force/Values.yaml" \
  --value-file-only \
  --create-values-file \
  --force \
  --eval >"$eval_stdout" 2>"$eval_stderr"
assert_contains "$eval_stdout" "export APP_NAME='default-app'"
assert_contains "$eval_stdout" "export IMAGE_TAG='1.0.0'"

# 8) Indent multiline values
run_tplenv \
  --file "$EX_DIR/07-indent/template.yaml" \
  --values-file "$EX_DIR/07-indent/Values.yaml" \
  --indent \
  --output "$WORK_DIR/out-07.yaml"
assert_contains "$WORK_DIR/out-07.yaml" "startup.sh: |"
assert_contains "$WORK_DIR/out-07.yaml" "    echo \"boot\""
assert_contains "$WORK_DIR/out-07.yaml" "    echo \"ready\""
assert_contains "$WORK_DIR/out-07.yaml" "    echo \"done\""

# 9) Context mode (shows list-entry context and prompts in file order)
ctx_stdout="$WORK_DIR/context-stdout.txt"
ctx_stderr="$WORK_DIR/context-stderr.txt"
printf 'img-orig\nimg-dest\npull-secret\n5.9.0\ncas-ns\ncas-name\n--cvm\n--scone-enclave\n' | run_tplenv \
  --file "$EX_DIR/08-context/environment-variables.md" \
  --values-file "$EX_DIR/08-context/Values.yaml" \
  --create-values-file \
  --context \
  --output "$WORK_DIR/out-08.md" >"$ctx_stdout" 2>"$ctx_stderr"

assert_contains "$WORK_DIR/out-08.md" "stored: img-orig"
assert_contains "$WORK_DIR/out-08.md" "Destination of the confidential container image: img-dest"
assert_contains "$WORK_DIR/out-08.md" "set to --cvm"
assert_contains "$EX_DIR/08-context/Values.yaml" "IMAGE_NAME: img-orig"
assert_contains "$EX_DIR/08-context/Values.yaml" "SCONE_ENCLAVE: --scone-enclave"
assert_contains "$ctx_stderr" "1. Original native conatainer image is stored: \${IMAGE_NAME}"
assert_contains "$ctx_stderr" "8. In CVM mode, you can run using the nodes or Kata-Pods. Mainly, set to --scone-enclave: \${SCONE_ENCLAVE}"
assert_prompt_order "$ctx_stderr" \
  "Enter value for values file key environment.IMAGE_NAME:" \
  "Enter value for values file key environment.DESTINATION_IMAGE_NAME:" \
  "Enter value for values file key environment.IMAGE_PULL_SECRET_NAME:" \
  "Enter value for values file key environment.SCONE_VERSION:" \
  "Enter value for values file key environment.CAS_NAMESPACE:" \
  "Enter value for values file key environment.CAS_NAME:" \
  "Enter value for values file key environment.CVM_MODE:" \
  "Enter value for values file key environment.SCONE_ENCLAVE:"

echo ""
echo "All examples passed."
