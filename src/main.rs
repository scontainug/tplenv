// src/main.rs
use anyhow::{Context, Result, bail};
use clap::Parser;
use regex::Regex;
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Substitute {{VARNAME}} placeholders using environment variables,
/// and {{ .Values.key }} placeholders using a YAML values file (default: Values.yaml).
///
/// Examples:
///   {{NAMESPACE}}              -> env var NAMESPACE
///   {{ .Values.namespace }}    -> Values.yaml: namespace
///   {{ .Values.foo.bar }}      -> Values.yaml: foo: { bar: ... }
#[derive(Parser, Debug)]
#[command(name = "tplenv", version, about, disable_help_flag = false)]
struct Args {
    /// Input file (e.g., a YAML manifest)
    #[arg(short = 'f', long = "file")]
    file: PathBuf,

    /// YAML values file (default: Values.yaml)
    #[arg(long = "values", default_value = "Values.yaml")]
    values: PathBuf,

    /// Output file (defaults to stdout). Use "-" for stdout.
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    /// Verbose logging to stderr
    #[arg(short = 'v', long = "verbose", default_value_t = false)]
    verbose: bool,

    /// Ask for values and create/update the values file before rendering
    #[arg(long = "create-values-file", default_value_t = false)]
    create_values_file: bool,

    /// With --create-values-file, ask for all .Values paths (not only missing ones)
    #[arg(long = "force", default_value_t = false)]
    force: bool,

    /// Resolve {{VAR}} from values file section environment.<VAR> (ignore OS env vars)
    #[arg(long = "value-file-only", default_value_t = false)]
    value_file_only: bool,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();

    let input = fs::read_to_string(&args.file)
        .with_context(|| format!("failed to read file: {}", args.file.display()))?;

    // One regex to match both:
    //   {{NAMESPACE}}                         -> capture group 2
    //   {{ .Values.namespace }}               -> capture group 1 (path)
    //   {{.Values.foo.bar}} / extra spaces OK
    //
    // Values paths are dot-separated identifiers: foo.bar.baz
    let re = placeholder_regex()?;
    let (env_vars, values_paths) = collect_placeholders(&input, &re);

    if args.force && !args.create_values_file {
        bail!("--force can only be used together with --create-values-file");
    }

    let needs_values_prompt =
        !values_paths.is_empty() || (args.value_file_only && !env_vars.is_empty());
    if args.create_values_file && needs_values_prompt {
        prompt_and_update_values_file(
            &args.values,
            &values_paths,
            &env_vars,
            args.value_file_only,
            args.force,
            args.verbose,
        )?;
    }

    // Load values YAML only if we actually need it
    let needs_values_yaml =
        !values_paths.is_empty() || (args.value_file_only && !env_vars.is_empty());
    let values_yaml: Option<YamlValue> = if !needs_values_yaml {
        None
    } else {
        load_values_yaml(&args.values)?
    };

    // Resolve placeholders
    let mut missing_values: Vec<String> = Vec::new();
    let mut missing_env: Vec<String> = Vec::new();
    let mut env_map: HashMap<String, String> = HashMap::new();
    if args.value_file_only {
        if !env_vars.is_empty() {
            let yaml = values_yaml
                .as_ref()
                .expect("values_yaml must be loaded in --value-file-only mode");
            let (resolved, missing_paths) = resolve_env_from_values_file(&env_vars, yaml)?;
            env_map = resolved;

            // Treat missing env substitutions as missing values file keys.
            missing_env.clear();
            for p in missing_paths {
                missing_values.push(p);
            }
        }
    } else {
        for v in &env_vars {
            match env::var_os(v) {
                Some(os) => {
                    let val = os.to_string_lossy().to_string();
                    env_map.insert(v.clone(), val);
                }
                None => missing_env.push(v.clone()),
            }
        }
    }

    // Resolve values paths
    let mut values_map: HashMap<String, String> = HashMap::new();
    for p in &values_paths {
        let yaml = values_yaml
            .as_ref()
            .expect("values_yaml must be loaded if values_paths is non-empty");
        match lookup_yaml_path(yaml, p) {
            Some(v) => {
                let s = yaml_value_to_string(v)?;
                values_map.insert(p.clone(), s);
            }
            None => missing_values.push(p.clone()),
        }
    }

    // If anything missing, print all missing and fail
    if !missing_env.is_empty() || !missing_values.is_empty() {
        if !missing_env.is_empty() {
            eprintln!("Missing/undefined environment variables:");
            for v in &missing_env {
                eprintln!("- {v}");
            }
        }
        if !missing_values.is_empty() {
            eprintln!("Missing keys in values file ({}):", args.values.display());
            for p in &missing_values {
                if p.starts_with("environment.") {
                    eprintln!("- {p}");
                } else {
                    eprintln!("- .Values.{p}");
                }
            }
        }
        bail!("not all placeholders could be resolved");
    }

    // Render with logging (if verbose)
    let rendered = re.replace_all(&input, |caps: &regex::Captures| {
        if let Some(p) = caps.get(1) {
            let key = p.as_str();
            let val = values_map.get(key).cloned().unwrap_or_default();
            if args.verbose {
                eprintln!("set .Values.{key} = {val}");
            }
            val
        } else {
            let key = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let val = env_map.get(key).cloned().unwrap_or_default();
            if args.verbose {
                if args.value_file_only {
                    eprintln!("set environment.{key} = {val}");
                } else {
                    eprintln!("set env {key} = {val}");
                }
            }
            val
        }
    });

    write_output(args.output.as_ref(), rendered.as_bytes())?;
    Ok(())
}

fn placeholder_regex() -> Result<Regex> {
    Ok(Regex::new(
        r"\{\{\s*(?:\.Values\.([A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+)*)|([A-Za-z_][A-Za-z0-9_]*))\s*\}\}",
    )?)
}

fn collect_placeholders(input: &str, re: &Regex) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut env_vars: BTreeSet<String> = BTreeSet::new();
    let mut values_paths: BTreeSet<String> = BTreeSet::new();

    for cap in re.captures_iter(input) {
        if let Some(p) = cap.get(1) {
            values_paths.insert(p.as_str().to_string());
        } else if let Some(v) = cap.get(2) {
            env_vars.insert(v.as_str().to_string());
        }
    }

    (env_vars, values_paths)
}

fn load_values_yaml(path: &Path) -> Result<Option<YamlValue>> {
    // If values placeholders are present, we require the file to exist & parse.
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read values file: {}", path.display()))?;
    let yaml: YamlValue = serde_yaml::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(yaml))
}

fn load_values_yaml_if_exists(path: &Path) -> Result<YamlValue> {
    if !path.exists() {
        return Ok(YamlValue::Mapping(YamlMapping::new()));
    }

    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read values file: {}", path.display()))?;
    let yaml: YamlValue = serde_yaml::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(yaml)
}

fn prompt_and_update_values_file(
    path: &Path,
    values_paths: &BTreeSet<String>,
    env_vars: &BTreeSet<String>,
    include_environment_vars: bool,
    force: bool,
    verbose: bool,
) -> Result<()> {
    let mut root = load_values_yaml_if_exists(path)?;

    let all_prompt_paths = collect_prompt_paths(values_paths, env_vars, include_environment_vars);
    let prompt_paths: Vec<String> = if force {
        all_prompt_paths.iter().cloned().collect()
    } else {
        all_prompt_paths
            .iter()
            .filter(|p| lookup_yaml_path(&root, p).is_none())
            .cloned()
            .collect()
    };

    if prompt_paths.is_empty() {
        if verbose {
            eprintln!("No values to prompt for in {}", path.display());
        }
        return Ok(());
    }

    for p in prompt_paths {
        let default_value = lookup_yaml_path(&root, &p).cloned();
        let chosen = prompt_for_yaml_key(&p, default_value.as_ref())?;
        set_yaml_path(&mut root, &p, chosen);
    }

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory: {}", parent.display()))?;
    }

    let out = serde_yaml::to_string(&root)?;
    fs::write(path, out)
        .with_context(|| format!("failed to write values file: {}", path.display()))?;
    Ok(())
}

fn collect_prompt_paths(
    values_paths: &BTreeSet<String>,
    env_vars: &BTreeSet<String>,
    include_environment_vars: bool,
) -> BTreeSet<String> {
    let mut all: BTreeSet<String> = values_paths.clone();
    if include_environment_vars {
        for var in env_vars {
            all.insert(env_var_values_path(var));
        }
    }
    all
}

fn env_var_values_path(var: &str) -> String {
    format!("environment.{var}")
}

fn resolve_env_from_values_file(
    env_vars: &BTreeSet<String>,
    yaml: &YamlValue,
) -> Result<(HashMap<String, String>, Vec<String>)> {
    let mut env_map = HashMap::new();
    let mut missing_paths = Vec::new();

    for var in env_vars {
        let path = env_var_values_path(var);
        match lookup_yaml_path(yaml, &path) {
            Some(v) => {
                env_map.insert(var.clone(), yaml_value_to_string(v)?);
            }
            None => missing_paths.push(path),
        }
    }

    Ok((env_map, missing_paths))
}

fn prompt_for_yaml_key(path: &str, default: Option<&YamlValue>) -> Result<YamlValue> {
    let mut prompt = format!("Enter value for values file key {path}");
    if let Some(v) = default {
        let default_text = yaml_value_to_string(v)?;
        prompt.push_str(&format!(" [{default_text}]"));
    }
    prompt.push_str(": ");

    print!("{prompt}");
    io::stdout().flush()?;

    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let entered = line.trim_end_matches(['\r', '\n']);

    if entered.is_empty() {
        if let Some(v) = default {
            return Ok(v.clone());
        }
        return Ok(YamlValue::String(String::new()));
    }

    Ok(YamlValue::String(entered.to_string()))
}

fn set_yaml_path(root: &mut YamlValue, path: &str, value: YamlValue) {
    let parts: Vec<&str> = path.split('.').collect();
    if !matches!(root, YamlValue::Mapping(_)) {
        *root = YamlValue::Mapping(YamlMapping::new());
    }

    let mut cur = root;
    let mut value_opt = Some(value);

    for (idx, part) in parts.iter().enumerate() {
        let is_last = idx == parts.len() - 1;
        let key = YamlValue::String((*part).to_string());

        match cur {
            YamlValue::Mapping(map) => {
                if is_last {
                    if let Some(v) = value_opt.take() {
                        map.insert(key, v);
                    }
                    return;
                }

                let entry = map
                    .entry(key)
                    .or_insert_with(|| YamlValue::Mapping(YamlMapping::new()));
                if !matches!(entry, YamlValue::Mapping(_)) {
                    *entry = YamlValue::Mapping(YamlMapping::new());
                }
                cur = entry;
            }
            _ => {
                *cur = YamlValue::Mapping(YamlMapping::new());
            }
        }
    }
}

fn lookup_yaml_path<'a>(root: &'a YamlValue, path: &str) -> Option<&'a YamlValue> {
    // path like "foo.bar.baz"
    let mut cur = root;
    for part in path.split('.') {
        match cur {
            YamlValue::Mapping(map) => {
                let key = YamlValue::String(part.to_string());
                cur = map.get(&key)?;
            }
            _ => return None,
        }
    }
    Some(cur)
}

fn yaml_value_to_string(v: &YamlValue) -> Result<String> {
    Ok(match v {
        YamlValue::Null => "".to_string(),
        YamlValue::Bool(b) => b.to_string(),
        YamlValue::Number(n) => n.to_string(),
        YamlValue::String(s) => s.clone(),
        // For sequences/maps, serialize to YAML (trim trailing newline).
        other => serde_yaml::to_string(other)?.trim_end().to_string(),
    })
}

fn write_output(output: Option<&PathBuf>, bytes: &[u8]) -> Result<()> {
    match output.map(|p| p.as_path()) {
        None => {
            let mut out = io::stdout().lock();
            out.write_all(bytes)?;
        }
        Some(p) if p.to_string_lossy() == "-" => {
            let mut out = io::stdout().lock();
            out.write_all(bytes)?;
        }
        Some(p) => {
            fs::write(p, bytes)
                .with_context(|| format!("failed to write output file: {}", p.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_placeholders_finds_unique_env_and_values() {
        let input = r#"
apiVersion: v1
metadata:
  namespace: {{NAMESPACE}}
  name: {{ APP_NAME }}
spec:
  image: {{ .Values.image.repository }}:{{.Values.image.tag}}
  replicas: {{ .Values.replicas }}
  namespace2: {{NAMESPACE}}
"#;
        let re = placeholder_regex().expect("regex must compile");
        let (env_vars, values_paths) = collect_placeholders(input, &re);

        assert_eq!(
            env_vars,
            BTreeSet::from(["APP_NAME".to_string(), "NAMESPACE".to_string()])
        );
        assert_eq!(
            values_paths,
            BTreeSet::from([
                "image.repository".to_string(),
                "image.tag".to_string(),
                "replicas".to_string()
            ])
        );
    }

    #[test]
    fn set_yaml_path_creates_nested_mappings() {
        let mut root = YamlValue::Mapping(YamlMapping::new());
        set_yaml_path(
            &mut root,
            "service.port",
            YamlValue::Number(serde_yaml::Number::from(8080)),
        );

        let got = lookup_yaml_path(&root, "service.port");
        assert_eq!(
            got,
            Some(&YamlValue::Number(serde_yaml::Number::from(8080)))
        );
    }

    #[test]
    fn set_yaml_path_replaces_non_mapping_intermediate_nodes() {
        let mut root: YamlValue = serde_yaml::from_str("service: api\n").expect("valid yaml");
        set_yaml_path(
            &mut root,
            "service.port",
            YamlValue::Number(serde_yaml::Number::from(80)),
        );

        let got = lookup_yaml_path(&root, "service.port");
        assert_eq!(got, Some(&YamlValue::Number(serde_yaml::Number::from(80))));
    }

    #[test]
    fn yaml_value_to_string_handles_scalars_and_mappings() {
        assert_eq!(
            yaml_value_to_string(&YamlValue::Bool(true)).expect("bool string"),
            "true"
        );
        assert_eq!(
            yaml_value_to_string(&YamlValue::String("abc".to_string())).expect("string value"),
            "abc"
        );

        let mapping: YamlValue = serde_yaml::from_str("foo: bar\n").expect("valid map yaml");
        let rendered = yaml_value_to_string(&mapping).expect("mapping string");
        assert!(rendered.contains("foo: bar"));
    }

    #[test]
    fn env_var_values_path_builds_expected_key() {
        assert_eq!(env_var_values_path("NAMESPACE"), "environment.NAMESPACE");
    }

    #[test]
    fn resolve_env_from_values_file_reads_environment_section() {
        let yaml: YamlValue = serde_yaml::from_str(
            r#"
environment:
  APP_NAME: api
  NAMESPACE: prod
"#,
        )
        .expect("valid yaml");
        let env_vars = BTreeSet::from(["APP_NAME".to_string(), "NAMESPACE".to_string()]);

        let (resolved, missing) =
            resolve_env_from_values_file(&env_vars, &yaml).expect("env values resolve");

        assert_eq!(resolved.get("APP_NAME"), Some(&"api".to_string()));
        assert_eq!(resolved.get("NAMESPACE"), Some(&"prod".to_string()));
        assert!(missing.is_empty());
    }

    #[test]
    fn resolve_env_from_values_file_reports_missing_keys() {
        let yaml: YamlValue = serde_yaml::from_str(
            r#"
environment:
  APP_NAME: api
"#,
        )
        .expect("valid yaml");
        let env_vars = BTreeSet::from(["APP_NAME".to_string(), "NAMESPACE".to_string()]);

        let (resolved, missing) =
            resolve_env_from_values_file(&env_vars, &yaml).expect("env values resolve");

        assert_eq!(resolved.get("APP_NAME"), Some(&"api".to_string()));
        assert!(!resolved.contains_key("NAMESPACE"));
        assert_eq!(missing, vec!["environment.NAMESPACE".to_string()]);
    }
}
