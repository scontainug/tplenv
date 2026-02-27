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

const BASH_COMPLETION: &str = include_str!("../completions/tplenv.bash");
const ZSH_COMPLETION: &str = include_str!("../completions/_tplenv");

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
    after_help = "Quick examples:\n  tplenv --file app.yaml --values Values.yaml\n  tplenv --file app.yaml --indent\n  tplenv --file app.yaml --create-values-file\n  tplenv --file app.yaml --value-file-only --create-values-file --force\n  tplenv --file-pattern \"configs/<NUM>-*.yaml\" --values Values.yaml\n  tplenv --file-pattern \"configs/<NUM>-*.yaml\" --output rendered.yaml\n  eval \"$(tplenv --file app.yaml --create-values-file --eval)\"\n  tplenv --install-completion\n  tplenv --install-completion zsh\n",
    disable_help_flag = false,
    next_line_help = true,
    group(
        ArgGroup::new("input")
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
    #[arg(
        long = "values-file",
        visible_alias = "values",
        default_value = "Values.yaml"
    )]
    values: PathBuf,

    /// Output file path (default: stdout). Use "-" to force stdout.
    /// With multiple input files, output becomes one YAML multi-document stream.
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    /// Show each placeholder replacement while rendering
    #[arg(short = 'v', long = "verbose", default_value_t = false)]
    verbose: bool,

    /// Ask questions for missing placeholders, then write/update the values file first
    /// Env placeholders are stored under environment.<VAR>.
    /// environment.<VAR> in values file has priority over OS env vars.
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

    /// Show template context before each --create-values-file prompt
    #[arg(long = "context", default_value_t = false)]
    context: bool,

    /// Install shell completion (auto, bash, or zsh)
    #[arg(
        long = "install-completion",
        num_args = 0..=1,
        default_missing_value = "auto",
        value_name = "SHELL"
    )]
    install_completion: Option<String>,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();

    if let Some(shell_arg) = args.install_completion.as_deref() {
        install_completion(shell_arg)?;
        return Ok(());
    }

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
    let prompt_contexts = collect_prompt_contexts(&templates, &re, args.context);
    let prompt_order = collect_prompt_order(&templates, &re);

    if args.force && !args.create_values_file {
        bail!("--force can only be used together with --create-values-file");
    }
    if args.eval && !args.create_values_file {
        bail!("--eval can only be used together with --create-values-file");
    }

    let include_environment_vars_in_prompts = args.create_values_file;
    let existing_os_env_vars: BTreeSet<String> = if args.value_file_only {
        BTreeSet::new()
    } else {
        env_vars
            .iter()
            .filter(|v| env::var_os(v).is_some())
            .cloned()
            .collect()
    };
    let existing_os_env_values: HashMap<String, String> = if args.value_file_only {
        HashMap::new()
    } else {
        env_vars
            .iter()
            .filter_map(|v| env::var_os(v).map(|os| (v.clone(), os.to_string_lossy().to_string())))
            .collect()
    };
    let needs_values_prompt =
        !values_paths.is_empty() || (include_environment_vars_in_prompts && !env_vars.is_empty());
    let mut prompted_values: Vec<(String, String)> = Vec::new();
    if args.create_values_file && needs_values_prompt {
        let prompt_opts = PromptUpdateOptions {
            include_environment_vars: include_environment_vars_in_prompts,
            skip_existing_env_vars: &existing_os_env_vars,
            existing_os_env_values: &existing_os_env_values,
            prompt_contexts: &prompt_contexts,
            prompt_order: &prompt_order,
            force: args.force,
            verbose: args.verbose,
        };
        prompted_values =
            prompt_and_update_values_file(&args.values, &values_paths, &env_vars, &prompt_opts)?;
    }
    let prompted_env_map = prompted_environment_values(&prompted_values);

    // Load values YAML:
    // - required when .Values placeholders exist
    // - optional (if exists) for env placeholder precedence via environment.<VAR>
    let values_yaml: Option<YamlValue> = if !values_paths.is_empty() {
        load_values_yaml(&args.values)?
    } else if !env_vars.is_empty() {
        Some(load_values_yaml_if_exists(&args.values)?)
    } else {
        None
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
            if args.verbose {
                for (name, val) in &resolved {
                    if let Some(os) = env::var_os(name) {
                        let env_val = os.to_string_lossy().to_string();
                        if env_val != *val {
                            eprintln!(
                                "warning: env {name} differs from values file environment.{name}; using values file value"
                            );
                        }
                    }
                }
            }
            env_map = resolved;

            // Treat missing env substitutions as missing values file keys.
            missing_env.clear();
            for p in missing_paths {
                missing_values.push(p);
            }
        }
    } else {
        for v in &env_vars {
            let os_val = env::var_os(v).map(|os| os.to_string_lossy().to_string());

            if let Some(yaml) = values_yaml.as_ref() {
                let path = env_var_values_path(v);
                if let Some(val) = lookup_yaml_path(yaml, &path) {
                    let values_val = yaml_value_to_string(val)?;
                    if args.verbose
                        && let Some(env_val) = os_val.as_ref()
                        && env_val != &values_val
                    {
                        eprintln!(
                            "warning: env {v} differs from values file {path}; using values file value"
                        );
                    }
                    env_map.insert(v.clone(), values_val);
                    continue;
                }
            }
            if let Some(val) = prompted_env_map.get(v) {
                env_map.insert(v.clone(), val.clone());
                continue;
            }
            if let Some(val) = os_val {
                env_map.insert(v.clone(), val);
            } else {
                missing_env.push(v.clone());
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
        let script = render_eval_exports_with_env(&prompted_values, &env_map);
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
    opts: &PromptUpdateOptions<'_>,
) -> Result<Vec<(String, String)>> {
    let mut root = load_values_yaml_if_exists(path)?;
    let mut prompted_values: Vec<(String, String)> = Vec::new();
    let mut changed = false;

    if opts.include_environment_vars && !opts.force {
        for var in env_vars {
            let path_key = env_var_values_path(var);
            if lookup_yaml_path(&root, &path_key).is_none()
                && let Some(val) = opts.existing_os_env_values.get(var)
            {
                set_yaml_path(&mut root, &path_key, YamlValue::String(val.clone()));
                prompted_values.push((path_key, val.clone()));
                changed = true;
            }
        }
    }

    let env_skip = if opts.force {
        BTreeSet::new()
    } else {
        opts.skip_existing_env_vars.clone()
    };
    let all_prompt_paths = collect_prompt_paths(
        values_paths,
        env_vars,
        opts.include_environment_vars,
        &env_skip,
    );
    let mut prompt_paths: Vec<String> = if opts.force {
        all_prompt_paths.iter().cloned().collect()
    } else {
        all_prompt_paths
            .iter()
            .filter(|p| lookup_yaml_path(&root, p).is_none())
            .cloned()
            .collect()
    };
    let rank: HashMap<&str, usize> = opts
        .prompt_order
        .iter()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    prompt_paths.sort_by_key(|k| rank.get(k.as_str()).copied().unwrap_or(usize::MAX));

    if prompt_paths.is_empty() {
        if opts.verbose {
            eprintln!("No values to prompt for in {}", path.display());
        }
        if !changed {
            return Ok(prompted_values);
        }
    } else {
        for p in prompt_paths {
            let default_value = lookup_yaml_path(&root, &p).cloned();
            let context = opts.prompt_contexts.get(&p).map(|s| s.as_str());
            let chosen = prompt_for_yaml_key(&p, default_value.as_ref(), context)?;
            let chosen_text = yaml_value_to_string(&chosen)?;
            prompted_values.push((p.clone(), chosen_text));
            set_yaml_path(&mut root, &p, chosen);
            changed = true;
        }
    }

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory: {}", parent.display()))?;
    }

    if changed {
        let out = serde_yaml::to_string(&root)?;
        fs::write(path, out)
            .with_context(|| format!("failed to write values file: {}", path.display()))?;
    }
    Ok(prompted_values)
}

struct PromptUpdateOptions<'a> {
    include_environment_vars: bool,
    skip_existing_env_vars: &'a BTreeSet<String>,
    existing_os_env_values: &'a HashMap<String, String>,
    prompt_contexts: &'a HashMap<String, String>,
    prompt_order: &'a [String],
    force: bool,
    verbose: bool,
}

fn collect_prompt_paths(
    values_paths: &BTreeSet<String>,
    env_vars: &BTreeSet<String>,
    include_environment_vars: bool,
    skip_existing_env_vars: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut all: BTreeSet<String> = values_paths.clone();
    if include_environment_vars {
        for var in env_vars {
            if skip_existing_env_vars.contains(var) {
                continue;
            }
            all.insert(env_var_values_path(var));
        }
    }
    all
}

fn collect_prompt_contexts(
    templates: &[(PathBuf, String)],
    re: &Regex,
    extended_context: bool,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let include_file_header = templates.len() > 1;

    for (path, input) in templates {
        for cap in re.captures_iter(input) {
            let key = if let Some(p) = cap.get(1) {
                p.as_str().to_string()
            } else if let Some(env) = extract_env_key(&cap) {
                env_var_values_path(env)
            } else {
                continue;
            };

            if out.contains_key(&key) {
                continue;
            }

            let mut text = extract_prompt_context(input, &cap, re, &key, extended_context);
            if include_file_header {
                text = format!("[{}]\n{}", path.display(), text);
            }
            out.insert(key, text);
        }
    }

    out
}

fn collect_prompt_order(templates: &[(PathBuf, String)], re: &Regex) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();

    for (_, input) in templates {
        for cap in re.captures_iter(input) {
            let key = if let Some(p) = cap.get(1) {
                p.as_str().to_string()
            } else if let Some(env) = extract_env_key(&cap) {
                env_var_values_path(env)
            } else {
                continue;
            };

            if seen.insert(key.clone()) {
                out.push(key);
            }
        }
    }
    out
}

fn extract_prompt_context(
    input: &str,
    caps: &regex::Captures,
    re: &Regex,
    key: &str,
    extended_context: bool,
) -> String {
    let m = if let Some(m) = caps.get(0) {
        m
    } else {
        return String::new();
    };

    let lines = line_ranges(input);
    let line_idx = line_index_for_pos(&lines, m.start()).unwrap_or(0);
    if !extended_context {
        return trim_line_ending(&input[lines[line_idx].0..lines[line_idx].1]).to_string();
    }

    let (para_start, para_end) = paragraph_bounds(input, &lines, line_idx);
    let para_text = &input[lines[para_start].0..lines[para_end].1];
    let keys = collect_prompt_keys(para_text, re);
    if keys.len() == 1 && keys.contains(key) {
        return trim_surrounding_newlines(para_text).to_string();
    }

    if let Some((list_start, list_end)) =
        list_entry_bounds(input, &lines, line_idx, para_start, para_end)
    {
        let text = &input[lines[list_start].0..lines[list_end].1];
        return trim_surrounding_newlines(text).to_string();
    }

    trim_line_ending(&input[lines[line_idx].0..lines[line_idx].1]).to_string()
}

fn collect_prompt_keys(text: &str, re: &Regex) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for cap in re.captures_iter(text) {
        if let Some(p) = cap.get(1) {
            keys.insert(p.as_str().to_string());
        } else if let Some(env) = extract_env_key(&cap) {
            keys.insert(env_var_values_path(env));
        }
    }
    keys
}

fn line_ranges(input: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    for (i, b) in input.bytes().enumerate() {
        if b == b'\n' {
            ranges.push((start, i + 1));
            start = i + 1;
        }
    }
    if start < input.len() {
        ranges.push((start, input.len()));
    }
    if ranges.is_empty() {
        ranges.push((0, 0));
    }
    ranges
}

fn line_index_for_pos(lines: &[(usize, usize)], pos: usize) -> Option<usize> {
    lines.iter().position(|(s, e)| *s <= pos && pos < *e)
}

fn paragraph_bounds(input: &str, lines: &[(usize, usize)], line_idx: usize) -> (usize, usize) {
    let mut start = line_idx;
    while start > 0 {
        let prev = trim_line_ending(&input[lines[start - 1].0..lines[start - 1].1]);
        if prev.trim().is_empty() {
            break;
        }
        start -= 1;
    }

    let mut end = line_idx;
    while end + 1 < lines.len() {
        let next = trim_line_ending(&input[lines[end + 1].0..lines[end + 1].1]);
        if next.trim().is_empty() {
            break;
        }
        end += 1;
    }
    (start, end)
}

fn is_list_item_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("- ") {
        return true;
    }

    let mut chars = trimmed.chars().peekable();
    let mut has_digit = false;
    while let Some(c) = chars.peek().copied() {
        if c.is_ascii_digit() {
            has_digit = true;
            chars.next();
        } else {
            break;
        }
    }
    if !has_digit {
        return false;
    }
    match chars.next() {
        Some('.') | Some(')') => matches!(chars.next(), Some(' ')),
        _ => false,
    }
}

fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

fn list_entry_bounds(
    input: &str,
    lines: &[(usize, usize)],
    line_idx: usize,
    para_start: usize,
    para_end: usize,
) -> Option<(usize, usize)> {
    let mut anchor = None;
    for idx in (para_start..=line_idx).rev() {
        let line = trim_line_ending(&input[lines[idx].0..lines[idx].1]);
        if is_list_item_line(line) {
            anchor = Some(idx);
            break;
        }
    }
    let anchor_idx = anchor?;
    let anchor_line = trim_line_ending(&input[lines[anchor_idx].0..lines[anchor_idx].1]);
    let anchor_indent = leading_spaces(anchor_line);

    let mut end = para_end;
    for idx in (anchor_idx + 1)..=para_end {
        let line = trim_line_ending(&input[lines[idx].0..lines[idx].1]);
        if line.trim().is_empty() {
            end = idx.saturating_sub(1);
            break;
        }
        if is_list_item_line(line) && leading_spaces(line) <= anchor_indent {
            end = idx.saturating_sub(1);
            break;
        }
    }
    Some((anchor_idx, end))
}

fn trim_line_ending(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

fn trim_surrounding_newlines(s: &str) -> &str {
    s.trim_matches(['\r', '\n'])
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

fn render_eval_exports_with_env(
    prompted_values: &[(String, String)],
    resolved_env_map: &HashMap<String, String>,
) -> String {
    let mut export_map: HashMap<String, String> = HashMap::new();

    for (key, value) in prompted_values {
        let env_name = values_key_to_env_var(key);
        export_map.insert(env_name, value.clone());
    }

    // Always export resolved env placeholders in --eval mode, even without --force.
    for (name, value) in resolved_env_map {
        export_map.insert(name.clone(), value.clone());
    }

    let mut names: Vec<String> = export_map.keys().cloned().collect();
    names.sort();

    let mut out = String::new();
    for name in names {
        if let Some(value) = export_map.get(&name) {
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

fn prompt_for_yaml_key(
    path: &str,
    default: Option<&YamlValue>,
    context: Option<&str>,
) -> Result<YamlValue> {
    let mut prompt = format!("Enter value for values file key {path}");
    if let Some(v) = default {
        let default_text = yaml_value_to_string(v)?;
        prompt.push_str(&format!(" [{default_text}]"));
    }
    prompt.push_str(": ");

    let mut err = io::stderr().lock();
    if let Some(ctx) = context {
        err.write_all(b"\n")?;
        err.write_all(ctx.as_bytes())?;
        err.write_all(b"\n")?;
    }
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

#[derive(Copy, Clone)]
enum CompletionShell {
    Bash,
    Zsh,
}

fn install_completion(shell_arg: &str) -> Result<()> {
    let shell = resolve_completion_shell(shell_arg)?;
    let home = home_dir()?;

    match shell {
        CompletionShell::Bash => {
            let data_home = env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share"));
            let target_dir = data_home.join("bash-completion/completions");
            fs::create_dir_all(&target_dir)
                .with_context(|| format!("failed to create {}", target_dir.display()))?;
            let target = target_dir.join("tplenv");
            fs::write(&target, BASH_COMPLETION)
                .with_context(|| format!("failed to write {}", target.display()))?;
            eprintln!("Installed bash completion: {}", target.display());
            eprintln!("Open a new shell, or run: source {}", target.display());
        }
        CompletionShell::Zsh => {
            let target_dir = home.join(".zsh/completions");
            fs::create_dir_all(&target_dir)
                .with_context(|| format!("failed to create {}", target_dir.display()))?;
            let target = target_dir.join("_tplenv");
            fs::write(&target, ZSH_COMPLETION)
                .with_context(|| format!("failed to write {}", target.display()))?;

            let zshrc = home.join(".zshrc");
            ensure_line_in_file(&zshrc, "fpath=(~/.zsh/completions $fpath)")?;
            ensure_line_in_file(&zshrc, "autoload -Uz compinit && compinit")?;

            eprintln!("Installed zsh completion: {}", target.display());
            eprintln!(
                "Open a new shell, or run: fpath=(~/.zsh/completions $fpath); autoload -Uz compinit && compinit"
            );
        }
    }

    Ok(())
}

fn resolve_completion_shell(shell_arg: &str) -> Result<CompletionShell> {
    if shell_arg == "auto" {
        let shell = env::var("SHELL").unwrap_or_default();
        let base = Path::new(&shell)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        return match base {
            "bash" => Ok(CompletionShell::Bash),
            "zsh" => Ok(CompletionShell::Zsh),
            _ => bail!(
                "could not detect shell from SHELL={shell}; use --install-completion bash|zsh"
            ),
        };
    }

    match shell_arg {
        "bash" => Ok(CompletionShell::Bash),
        "zsh" => Ok(CompletionShell::Zsh),
        _ => bail!("unsupported shell '{shell_arg}', expected bash or zsh"),
    }
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))
}

fn ensure_line_in_file(path: &Path, line: &str) -> Result<()> {
    let content = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?
    } else {
        String::new()
    };
    if content.lines().any(|l| l == line) {
        return Ok(());
    }

    let mut updated = content;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(line);
    updated.push('\n');

    fs::write(path, updated).with_context(|| format!("failed to write {}", path.display()))?;
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
        let skip = BTreeSet::new();

        let all = collect_prompt_paths(&values_paths, &env_vars, true, &skip);

        assert!(all.contains("db.user"));
        assert!(all.contains("environment.APP_NAME"));
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn collect_prompt_paths_skips_existing_env_vars_without_force() {
        let values_paths = BTreeSet::new();
        let env_vars = BTreeSet::from(["IMAGE".to_string(), "SIGNER".to_string()]);
        let skip = BTreeSet::from(["IMAGE".to_string()]);

        let all = collect_prompt_paths(&values_paths, &env_vars, true, &skip);
        assert!(!all.contains("environment.IMAGE"));
        assert!(all.contains("environment.SIGNER"));
    }

    #[test]
    fn extract_prompt_context_defaults_to_single_line() {
        let input = "image: ${IMAGE}\n";
        let re = placeholder_regex().expect("regex compiles");
        let cap = re.captures(input).expect("capture exists");
        let got = extract_prompt_context(input, &cap, &re, "environment.IMAGE", false);
        assert_eq!(got, "image: ${IMAGE}");
    }

    #[test]
    fn extract_prompt_context_uses_paragraph_for_single_variable() {
        let input = "title: ${IMAGE}\nnotes: hello\n\nother: x\n";
        let re = placeholder_regex().expect("regex compiles");
        let cap = re.captures(input).expect("capture exists");
        let got = extract_prompt_context(input, &cap, &re, "environment.IMAGE", true);
        assert_eq!(got, "title: ${IMAGE}\nnotes: hello");
    }

    #[test]
    fn extract_prompt_context_uses_list_entry_when_paragraph_has_multiple_variables() {
        let input = "items:\n  - user: ${A}\n    note: x\n  - user: ${B}\n";
        let re = placeholder_regex().expect("regex compiles");
        let cap = re
            .captures_iter(input)
            .nth(1)
            .expect("second placeholder capture");
        let got = extract_prompt_context(input, &cap, &re, "environment.B", true);
        assert_eq!(got, "  - user: ${B}");
    }

    #[test]
    fn collect_prompt_order_follows_file_occurrence() {
        let templates = vec![(
            PathBuf::from("a.yaml"),
            "x: ${B}\ny: {{ .Values.alpha }}\nz: ${A}\n".to_string(),
        )];
        let re = placeholder_regex().expect("regex compiles");
        let order = collect_prompt_order(&templates, &re);
        assert_eq!(
            order,
            vec![
                "environment.B".to_string(),
                "alpha".to_string(),
                "environment.A".to_string()
            ]
        );
    }

    #[test]
    fn resolve_completion_shell_parses_explicit_values() {
        assert!(matches!(
            resolve_completion_shell("bash").expect("bash shell"),
            CompletionShell::Bash
        ));
        assert!(matches!(
            resolve_completion_shell("zsh").expect("zsh shell"),
            CompletionShell::Zsh
        ));
        assert!(resolve_completion_shell("fish").is_err());
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
        let out = render_eval_exports_with_env(&prompted, &HashMap::new());
        assert!(out.contains("export APP_NAME='demo-app'"));
        assert!(out.contains("export IMAGE_TAG='1.2.3'"));
    }

    #[test]
    fn render_eval_exports_with_env_always_includes_resolved_env_values() {
        let prompted = vec![("image.tag".to_string(), "1.2.3".to_string())];
        let resolved_env = HashMap::from([("IMAGE".to_string(), "repo/app:7".to_string())]);
        let out = render_eval_exports_with_env(&prompted, &resolved_env);
        assert!(out.contains("export IMAGE='repo/app:7'"));
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
            if let Some(key) = extract_env_key(caps)
                && key == "SIGNER"
            {
                let m = caps.get(0).expect("full match present");
                return format_replacement_with_indent(signer, input, m.start(), m.end());
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
