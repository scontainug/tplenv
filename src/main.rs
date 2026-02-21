// src/main.rs
use anyhow::{bail, Context, Result};
use clap::Parser;
use regex::Regex;
use serde_yaml::Value as YamlValue;
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
#[command(name = "tplenv", version, about)]
struct Args {
    /// Input file (e.g., a YAML manifest)
    #[arg(short = 'f', long = "file")]
    file: PathBuf,

    /// YAML values file (default: Values.yaml)
    #[arg(short = 'V', long = "values", default_value = "Values.yaml")]
    values: PathBuf,

    /// Output file (defaults to stdout). Use "-" for stdout.
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    /// Verbose logging to stderr
    #[arg(short = 'v', long = "verbose", default_value_t = false)]
    verbose: bool,
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
    let re = Regex::new(
        r"\{\{\s*(?:\.Values\.([A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+)*)|([A-Za-z_][A-Za-z0-9_]*))\s*\}\}",
    )?;

    // Collect unique placeholders
    let mut env_vars: BTreeSet<String> = BTreeSet::new();
    let mut values_paths: BTreeSet<String> = BTreeSet::new();

    for cap in re.captures_iter(&input) {
        if let Some(p) = cap.get(1) {
            values_paths.insert(p.as_str().to_string());
        } else if let Some(v) = cap.get(2) {
            env_vars.insert(v.as_str().to_string());
        }
    }

    // Load values YAML only if we actually need it
    let values_yaml: Option<YamlValue> = if values_paths.is_empty() {
        None
    } else {
        load_values_yaml(&args.values)?
    };

    // Resolve env vars
    let mut missing_env: Vec<String> = Vec::new();
    let mut env_map: HashMap<String, String> = HashMap::new();
    for v in &env_vars {
        match env::var_os(v) {
            Some(os) => {
                let val = os.to_string_lossy().to_string();
                env_map.insert(v.clone(), val);
            }
            None => missing_env.push(v.clone()),
        }
    }

    // Resolve values paths
    let mut missing_values: Vec<String> = Vec::new();
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
            eprintln!(
                "Missing keys in values file ({}):",
                args.values.display()
            );
            for p in &missing_values {
                eprintln!("- .Values.{p}");
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
                eprintln!("set env {key} = {val}");
            }
            val
        }
    });

    write_output(args.output.as_ref(), rendered.as_bytes())?;
    Ok(())
}

fn load_values_yaml(path: &Path) -> Result<Option<YamlValue>> {
    // If values placeholders are present, we require the file to exist & parse.
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read values file: {}", path.display()))?;
    let yaml: YamlValue =
        serde_yaml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(yaml))
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
            fs::write(p, bytes).with_context(|| format!("failed to write output file: {}", p.display()))?;
        }
    }
    Ok(())
}
