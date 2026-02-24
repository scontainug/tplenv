// src/main.rs
use anyhow::{Context, Result, bail};
use clap::{ArgGroup, Parser};
use regex::Regex;
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Substitute env placeholders using environment variables (`{{VARNAME}}`, `$VARNAME`, `${VARNAME}`),
/// and {{ .Values.key }} placeholders using a YAML values file (default: Values.yaml).
///
/// Examples:
///   {{NAMESPACE}}              -> env var NAMESPACE
///   {{ .Values.namespace }}    -> Values.yaml: namespace
///   {{ .Values.foo.bar }}      -> Values.yaml: foo: { bar: ... }
#[derive(Parser, Debug)]
#[command(
    name = "tplenv",
    version,
    about = "Fill placeholders in YAML templates using env vars and/or a values file",
    long_about = "tplenv reads one or more template files and replaces placeholders:\n- {{VARNAME}}, $VARNAME, ${VARNAME} from environment variables\n- {{ .Values.key }} from a YAML values file\n\nYou can also run in values-only mode so env placeholders are read from environment.VARNAME in the values file.\n\nFile patterns:\n- --file-pattern matches files in one directory using * and <NUM>\n- matched files are processed in sorted filename order\n- output is one YAML multi-document stream (documents separated by ---)\n\nEval mode:\n- --eval prints prompted values as bash export statements\n- designed for: eval \"$(tplenv ... --create-values-file --eval)\"",
    after_help = "Quick examples:\n  tplenv --file app.yaml --values Values.yaml\n  tplenv --file app.yaml --indent\n  tplenv --file app.yaml --create-values-file\n  tplenv --file app.yaml --value-file-only --create-values-file --force\n  tplenv --file-pattern \"configs/<NUM>-*.yaml\" --values Values.yaml\n  tplenv --file-pattern \"configs/<NUM>-*.yaml\" --output rendered.yaml\n  eval \"$(tplenv --file app.yaml --create-values-file --eval)\"\n",
    disable_help_flag = false,
    next_line_help = true,
    group(
        ArgGroup::new("input")
            .required(true)
            .args(["file", "file_pattern"])
    )
)]
struct Args {
    /// Single template file to render
    #[arg(short = 'f', long = "file")]
    file: Option<PathBuf>,

    /// Render all files matching this pattern (supports * and <NUM>)
    /// Output becomes one YAML multi-document stream.
    #[arg(long = "file-pattern")]
    file_pattern: Option<String>,

    /// Values YAML file used for {{ .Values.* }} lookups and environment.* in --value-file-only mode
    #[arg(long = "values", default_value = "Values.yaml")]
    values: PathBuf,

    /// Output file path (default: stdout). Use "-" to force stdout.
    /// With multiple input files, output becomes one YAML multi-document stream.
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    /// Show each placeholder replacement while rendering
    #[arg(short = 'v', long = "verbose", default_value_t = false)]
    verbose: bool,

    /// Ask questions for missing values, then write/update the values file first
    #[arg(long = "create-values-file", default_value_t = false)]
    create_values_file: bool,

    /// With --create-values-file: ask for all keys, even if already set
    #[arg(long = "force", default_value_t = false)]
    force: bool,

    /// Do not read OS environment variables; use values file key environment.<VAR> for env placeholders
    #[arg(long = "value-file-only", default_value_t = false)]
    value_file_only: bool,

    /// Print prompted values as bash export statements (for use with eval "$( ... )")
    #[arg(long = "eval", default_value_t = false)]
    eval: bool,

    /// Preserve indentation for multiline replacement values
    #[arg(long = "indent", default_value_t = false)]
    indent: bool,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();

    let input_files = discover_input_files(args.file.as_ref(), args.file_pattern.as_deref())?;
    if input_files.len() > 1 {
        ensure_all_yaml_files(&input_files)?;
    }

    let mut templates: Vec<(PathBuf, String)> = Vec::new();
    for file in &input_files {
        let input = fs::read_to_string(file)
            .with_context(|| format!("failed to read file: {}", file.display()))?;
        templates.push((file.clone(), input));
    }

    // One regex to match all supported placeholders:
    //   {{ .Values.namespace }}               -> capture group 1 (path)
    //   {{NAMESPACE}}                         -> capture group 2
    //   ${NAMESPACE}                          -> capture group 3
    //   $NAMESPACE                            -> capture group 4
    //
    // Values paths are dot-separated identifiers: foo.bar.baz
    let re = placeholder_regex()?;
    let (env_vars, values_paths) = collect_placeholders_all(&templates, &re);

    if args.force && !args.create_values_file {
        bail!("--force can only be used together with --create-values-file");
    }
    if args.eval && !args.create_values_file {
        bail!("--eval can only be used together with --create-values-file");
    }

    let include_environment_vars_in_prompts = args.value_file_only || args.eval;
    let needs_values_prompt =
        !values_paths.is_empty() || (include_environment_vars_in_prompts && !env_vars.is_empty());
    let mut prompted_values: Vec<(String, String)> = Vec::new();
    if args.create_values_file && needs_values_prompt {
        prompted_values = prompt_and_update_values_file(
            &args.values,
            &values_paths,
            &env_vars,
            include_environment_vars_in_prompts,
            args.force,
            args.verbose,
        )?;
    }
    let prompted_env_map = prompted_environment_values(&prompted_values);

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
            if let Some(val) = prompted_env_map.get(v) {
                env_map.insert(v.clone(), val.clone());
                continue;
            }
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
    let mut rendered_outputs: Vec<(PathBuf, String)> = Vec::new();
    for (path, input) in &templates {
        let rendered = re.replace_all(input, |caps: &regex::Captures| {
            let raw = if let Some(p) = caps.get(1) {
                let key = p.as_str();
                let val = values_map.get(key).cloned().unwrap_or_default();
                if args.verbose {
                    eprintln!("set .Values.{key} = {val}");
                }
                val
            } else {
                let key = extract_env_key(caps).unwrap_or("");
                let val = env_map.get(key).cloned().unwrap_or_default();
                if args.verbose {
                    if args.value_file_only {
                        eprintln!("set environment.{key} = {val}");
                    } else {
                        eprintln!("set env {key} = {val}");
                    }
                }
                val
            };

            if args.indent {
                if let Some(m) = caps.get(0) {
                    format_replacement_with_indent(&raw, input, m.start(), m.end())
                } else {
                    raw
                }
            } else {
                raw
            }
        });
        rendered_outputs.push((path.clone(), rendered.to_string()));
    }

    if args.eval {
        // In eval mode, stdout should stay parseable as shell exports.
        if args.output.is_some()
            && args
                .output
                .as_ref()
                .map(|p| p.to_string_lossy() == "-")
                .unwrap_or(false)
        {
            bail!("with --eval, --output - is not supported");
        }
        if args.output.is_some() {
            write_outputs(args.output.as_ref(), &rendered_outputs)?;
        }
        let script = render_eval_exports(&prompted_values);
        let mut out = io::stdout().lock();
        out.write_all(script.as_bytes())?;
    } else {
        write_outputs(args.output.as_ref(), &rendered_outputs)?;
    }
    Ok(())
}

fn placeholder_regex() -> Result<Regex> {
    Ok(Regex::new(
        r"\{\{\s*(?:\.Values\.([A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+)*)|([A-Za-z_][A-Za-z0-9_]*))\s*\}\}|\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)",
    )?)
}

fn collect_placeholders(input: &str, re: &Regex) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut env_vars: BTreeSet<String> = BTreeSet::new();
    let mut values_paths: BTreeSet<String> = BTreeSet::new();

    for cap in re.captures_iter(input) {
        if let Some(p) = cap.get(1) {
            values_paths.insert(p.as_str().to_string());
        } else if let Some(v) = extract_env_key(&cap) {
            env_vars.insert(v.to_string());
        }
    }

    (env_vars, values_paths)
}

fn extract_env_key<'a>(caps: &'a regex::Captures<'a>) -> Option<&'a str> {
    caps.get(2)
        .or_else(|| caps.get(3))
        .or_else(|| caps.get(4))
        .map(|m| m.as_str())
}

fn collect_placeholders_all(
    templates: &[(PathBuf, String)],
    re: &Regex,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut env_vars: BTreeSet<String> = BTreeSet::new();
    let mut values_paths: BTreeSet<String> = BTreeSet::new();

    for (_, input) in templates {
        let (env, values) = collect_placeholders(input, re);
        env_vars.extend(env);
        values_paths.extend(values);
    }

    (env_vars, values_paths)
}

fn discover_input_files(
    file: Option<&PathBuf>,
    file_pattern: Option<&str>,
) -> Result<Vec<PathBuf>> {
    match (file, file_pattern) {
        (Some(path), None) => Ok(vec![path.clone()]),
        (None, Some(pattern)) => find_files_by_pattern(pattern),
        (Some(_), Some(_)) => bail!("use only one of --file or --file-pattern"),
        (None, None) => bail!("one of --file or --file-pattern is required"),
    }
}

fn find_files_by_pattern(pattern: &str) -> Result<Vec<PathBuf>> {
    let pattern_path = Path::new(pattern);
    let dir = match pattern_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let filename_pattern = pattern_path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid --file-pattern: {pattern}"))?;
    let re = file_pattern_regex(filename_pattern)?;

    let mut files = Vec::new();
    for entry in
        fs::read_dir(&dir).with_context(|| format!("failed to read dir: {}", dir.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() {
            continue;
        }

        let name = entry.file_name();
        let name = name.to_string_lossy();
        if re.is_match(&name) {
            files.push(dir.join(name.as_ref()));
        }
    }

    files.sort();
    if files.is_empty() {
        bail!("no files matched --file-pattern {pattern}");
    }
    Ok(files)
}

fn file_pattern_regex(pattern: &str) -> Result<Regex> {
    let escaped = regex::escape(pattern);
    let with_num = escaped.replace("<NUM>", "[0-9]+");
    let final_pattern = with_num.replace(r"\*", ".*");
    Ok(Regex::new(&format!("^{final_pattern}$"))?)
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
) -> Result<Vec<(String, String)>> {
    let mut root = load_values_yaml_if_exists(path)?;
    let mut prompted_values: Vec<(String, String)> = Vec::new();

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
        return Ok(prompted_values);
    }

    for p in prompt_paths {
        let default_value = lookup_yaml_path(&root, &p).cloned();
        let chosen = prompt_for_yaml_key(&p, default_value.as_ref())?;
        let chosen_text = yaml_value_to_string(&chosen)?;
        prompted_values.push((p.clone(), chosen_text));
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
    Ok(prompted_values)
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

fn values_key_to_env_var(values_key: &str) -> String {
    let no_prefix = values_key
        .strip_prefix("environment.")
        .unwrap_or(values_key);
    no_prefix.replace('.', "_").to_uppercase()
}

fn shell_escape_single_quoted(value: &str) -> String {
    value.replace('\'', "'\"'\"'")
}

fn render_eval_exports(prompted_values: &[(String, String)]) -> String {
    let mut env_map: HashMap<String, String> = HashMap::new();
    for (key, value) in prompted_values {
        let env_name = values_key_to_env_var(key);
        env_map.insert(env_name, value.clone());
    }

    let mut names: Vec<String> = env_map.keys().cloned().collect();
    names.sort();

    let mut out = String::new();
    for name in names {
        if let Some(value) = env_map.get(&name) {
            out.push_str(&format!(
                "export {}='{}'\n",
                name,
                shell_escape_single_quoted(value)
            ));
        }
    }
    out
}

fn prompted_environment_values(prompted_values: &[(String, String)]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (key, value) in prompted_values {
        if let Some(env_name) = key.strip_prefix("environment.") {
            out.insert(env_name.to_string(), value.clone());
        }
    }
    out
}

fn indent_multiline_value(value: &str, input: &str, match_start: usize) -> String {
    if !value.contains('\n') {
        return value.to_string();
    }

    let line_start = input[..match_start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let before_match = &input[line_start..match_start];
    let indent: String = before_match
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();

    let mut out = String::with_capacity(value.len() + indent.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        out.push(ch);
        if ch == '\n' && chars.peek().is_some() {
            out.push_str(&indent);
        }
    }
    out
}

fn format_replacement_with_indent(
    value: &str,
    input: &str,
    match_start: usize,
    match_end: usize,
) -> String {
    if !value.contains('\n') {
        return value.to_string();
    }

    if should_use_yaml_block_scalar(input, match_start, match_end) {
        format_as_yaml_block_scalar(value, input, match_start)
    } else {
        indent_multiline_value(value, input, match_start)
    }
}

fn should_use_yaml_block_scalar(input: &str, match_start: usize, match_end: usize) -> bool {
    let line_start = input[..match_start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = input[match_end..]
        .find('\n')
        .map(|i| match_end + i)
        .unwrap_or(input.len());

    let prefix = &input[line_start..match_start];
    let suffix = &input[match_end..line_end];
    let prefix_trimmed = prefix.trim_end();
    let suffix_trimmed = suffix.trim();

    (prefix_trimmed.ends_with(':') || prefix_trimmed.ends_with('-')) && suffix_trimmed.is_empty()
}

fn format_as_yaml_block_scalar(value: &str, input: &str, match_start: usize) -> String {
    let line_start = input[..match_start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_prefix = &input[line_start..match_start];
    let line_indent: String = line_prefix
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();
    let content_indent = format!("{line_indent}  ");

    let indicator = if has_trailing_empty_lines(value) {
        "|+"
    } else {
        "|"
    };
    let content = indent_every_line(value, &content_indent);
    format!("{indicator}\n{content}")
}

fn has_trailing_empty_lines(value: &str) -> bool {
    let mut trailing_newlines = 0usize;
    for ch in value.chars().rev() {
        if ch == '\n' {
            trailing_newlines += 1;
        } else {
            break;
        }
    }
    trailing_newlines > 1
}

fn indent_every_line(value: &str, indent: &str) -> String {
    let mut out = String::new();
    for part in value.split_inclusive('\n') {
        if let Some(line) = part.strip_suffix('\n') {
            out.push_str(indent);
            out.push_str(line);
            out.push('\n');
        } else {
            out.push_str(indent);
            out.push_str(part);
        }
    }
    out
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

    let mut err = io::stderr().lock();
    err.write_all(prompt.as_bytes())?;
    err.flush()?;

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

fn write_outputs(output: Option<&PathBuf>, rendered: &[(PathBuf, String)]) -> Result<()> {
    if rendered.len() == 1 {
        return write_output(output, rendered[0].1.as_bytes());
    }

    let merged = render_multi_document_yaml(rendered);
    write_output(output, merged.as_bytes())
}

fn render_multi_document_yaml(rendered: &[(PathBuf, String)]) -> String {
    let mut out = String::new();
    for (idx, (_, content)) in rendered.iter().enumerate() {
        if idx > 0 {
            out.push_str("\n---\n");
        }
        out.push_str(content);
        if !content.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

fn ensure_all_yaml_files(input_files: &[PathBuf]) -> Result<()> {
    for path in input_files {
        if !is_yaml_file(path) {
            bail!(
                "all input files must be *.yaml for multi-file output, but found {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn is_yaml_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.ends_with(".yaml"))
        .unwrap_or(false)
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
  short_env: $SHORT_ENV
  brace_env: ${BRACE_ENV}
spec:
  image: {{ .Values.image.repository }}:{{.Values.image.tag}}
  replicas: {{ .Values.replicas }}
  namespace2: {{NAMESPACE}}
"#;
        let re = placeholder_regex().expect("regex must compile");
        let (env_vars, values_paths) = collect_placeholders(input, &re);

        assert_eq!(
            env_vars,
            BTreeSet::from([
                "APP_NAME".to_string(),
                "BRACE_ENV".to_string(),
                "NAMESPACE".to_string(),
                "SHORT_ENV".to_string()
            ])
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

    #[test]
    fn file_pattern_regex_supports_num_token_and_wildcard() {
        let re = file_pattern_regex("<NUM>-*.yaml").expect("pattern compiles");
        assert!(re.is_match("1-demo.yaml"));
        assert!(re.is_match("42-x.yaml"));
        assert!(!re.is_match("demo.yaml"));
        assert!(!re.is_match("a-demo.yaml"));
    }

    #[test]
    fn collect_prompt_paths_deduplicates_shared_keys() {
        let values_paths = BTreeSet::from(["db.user".to_string()]);
        let env_vars = BTreeSet::from(["APP_NAME".to_string(), "APP_NAME".to_string()]);

        let all = collect_prompt_paths(&values_paths, &env_vars, true);

        assert!(all.contains("db.user"));
        assert!(all.contains("environment.APP_NAME"));
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn is_yaml_file_only_accepts_yaml_suffix() {
        assert!(is_yaml_file(Path::new("1-a.yaml")));
        assert!(!is_yaml_file(Path::new("1-a.yml")));
        assert!(!is_yaml_file(Path::new("1-a.txt")));
    }

    #[test]
    fn render_multi_document_yaml_uses_doc_separator() {
        let rendered = vec![
            (PathBuf::from("1-a.yaml"), "a: 1\n".to_string()),
            (PathBuf::from("2-b.yaml"), "b: 2\n".to_string()),
        ];
        let out = render_multi_document_yaml(&rendered);
        assert_eq!(out, "a: 1\n\n---\nb: 2\n");
    }

    #[test]
    fn extract_env_key_supports_three_env_styles() {
        let re = placeholder_regex().expect("regex compiles");

        let c1 = re
            .captures("{{NAMESPACE}}")
            .expect("must capture handlebars env");
        assert_eq!(extract_env_key(&c1), Some("NAMESPACE"));

        let c2 = re.captures("${APP_NAME}").expect("must capture brace env");
        assert_eq!(extract_env_key(&c2), Some("APP_NAME"));

        let c3 = re.captures("$REGION").expect("must capture short env");
        assert_eq!(extract_env_key(&c3), Some("REGION"));
    }

    #[test]
    fn values_key_to_env_var_handles_environment_prefix_and_dots() {
        assert_eq!(values_key_to_env_var("environment.APP_NAME"), "APP_NAME");
        assert_eq!(values_key_to_env_var("image.tag"), "IMAGE_TAG");
    }

    #[test]
    fn render_eval_exports_outputs_bash_exports() {
        let prompted = vec![
            ("environment.APP_NAME".to_string(), "demo-app".to_string()),
            ("image.tag".to_string(), "1.2.3".to_string()),
        ];
        let out = render_eval_exports(&prompted);
        assert!(out.contains("export APP_NAME='demo-app'"));
        assert!(out.contains("export IMAGE_TAG='1.2.3'"));
    }

    #[test]
    fn prompted_environment_values_only_keeps_environment_entries() {
        let prompted = vec![
            ("environment.IMAGE".to_string(), "nginx:1.2".to_string()),
            ("db.user".to_string(), "app".to_string()),
        ];
        let out = prompted_environment_values(&prompted);
        assert_eq!(out.get("IMAGE"), Some(&"nginx:1.2".to_string()));
        assert!(!out.contains_key("DB_USER"));
    }

    #[test]
    fn indent_multiline_value_uses_placeholder_line_indent() {
        let input = "data:\n  script: |\n    {{ .Values.script }}\n";
        let match_start = input
            .find("{{ .Values.script }}")
            .expect("placeholder should exist");
        let value = "echo first\necho second";

        let out = indent_multiline_value(value, input, match_start);
        assert_eq!(out, "echo first\n    echo second");
    }

    #[test]
    fn format_replacement_with_indent_uses_yaml_block_scalar_for_inline_value() {
        let input = "data:\n  script: {{ .Values.script }}\n";
        let token = "{{ .Values.script }}";
        let match_start = input.find(token).expect("placeholder should exist");
        let match_end = match_start + token.len();
        let value = "echo first\necho second";

        let out = format_replacement_with_indent(value, input, match_start, match_end);
        assert_eq!(out, "|\n    echo first\n    echo second");
    }

    #[test]
    fn format_replacement_with_indent_uses_block_scalar_keep_for_trailing_empty_lines() {
        let input = "data:\n  script: {{ .Values.script }}\n";
        let token = "{{ .Values.script }}";
        let match_start = input.find(token).expect("placeholder should exist");
        let match_end = match_start + token.len();
        let value = "echo first\n\n";

        let out = format_replacement_with_indent(value, input, match_start, match_end);
        assert_eq!(out, "|+\n    echo first\n    \n");
    }

    #[test]
    fn indent_multiline_signer_in_yaml_list_items_stays_valid_yaml() {
        let input = r#"name: kbs-certs
version: "0.3.11"

access_policy:
    read:
      - ANY
    update:
      - ${SIGNER}
    create_sessions:
      - ${SIGNER}
"#;
        let signer = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAtestkeyline\n-----END PUBLIC KEY-----";
        let re = placeholder_regex().expect("regex compiles");

        let rendered = re.replace_all(input, |caps: &regex::Captures| {
            if let Some(key) = extract_env_key(caps) {
                if key == "SIGNER" {
                    let m = caps.get(0).expect("full match present");
                    return format_replacement_with_indent(signer, input, m.start(), m.end());
                }
            }
            caps.get(0)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default()
        });

        let rendered = rendered.to_string();
        assert!(rendered.contains("- |\n        -----BEGIN PUBLIC KEY-----"));
        assert_eq!(rendered.matches("- |").count(), 2);
        let parsed: YamlValue = serde_yaml::from_str(&rendered).expect("rendered yaml is valid");
        assert!(matches!(parsed, YamlValue::Mapping(_)));
    }
}
