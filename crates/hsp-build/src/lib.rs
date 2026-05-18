use std::path::Path;

const BUILD_SUBCOMMANDS: &[&str] = &[
    "bench", "build", "check", "clippy", "compile", "install", "lint", "package", "publish",
    "run", "test", "verify",
];

const DIRECT_CHECKERS: &[&str] = &[
    "biome",
    "black",
    "eslint",
    "flake8",
    "isort",
    "mypy",
    "phpstan",
    "phpunit",
    "prettier",
    "pylint",
    "pyright",
    "pytest",
    "ruff",
    "shellcheck",
    "stylelint",
    "ty",
];

const BUILD_FIRST_TOKENS: &[&str] = &[
    "bun",
    "cargo",
    "cmake",
    "composer",
    "deno",
    "dotnet",
    "go",
    "gradle",
    "just",
    "make",
    "mvn",
    "ninja",
    "nox",
    "npm",
    "npx",
    "pnpm",
    "poetry",
    "pytest",
    "rk",
    "spaceship",
    "swift",
    "tox",
    "uv",
    "xcodebuild",
    "yarn",
];

const PYTHON_MODULE_CHECKERS: &[&str] = &["mypy", "pytest", "ruff", "unittest"];

const PATHY_OPTIONS_WITH_VALUE: &[&str] = &[
    "--config",
    "--config-file",
    "--directory",
    "--extra",
    "--group",
    "--manifest-path",
    "--only-group",
    "--package",
    "--python",
    "--project",
    "--target",
    "--target-dir",
    "--with",
    "--with-editable",
    "--with-requirements",
    "--without",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandGateSpec {
    pub argv: Vec<String>,
    pub full_workspace: bool,
    pub files: Vec<String>,
    pub tool: String,
    pub phase: BuildPhase,
}

impl CommandGateSpec {
    pub fn targets(&self) -> String {
        self.argv.join(" ")
    }

    pub fn files_csv(&self) -> String {
        self.files.join(",")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildPhase {
    Build,
    Check,
    Lint,
    Test,
    Upgrade,
    Unknown,
}

impl BuildPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Check => "check",
            Self::Lint => "lint",
            Self::Test => "test",
            Self::Upgrade => "upgrade",
            Self::Unknown => "unknown",
        }
    }
}

pub fn command_gate_spec_from_line(command: &str) -> Option<CommandGateSpec> {
    command_gate_spec(&split_command_line(command))
}

pub fn command_gate_spec(argv: &[String]) -> Option<CommandGateSpec> {
    gate_spec_for_argv(strip_env_assignments(argv))
}

fn gate_spec_for_argv(argv: &[String]) -> Option<CommandGateSpec> {
    let first = basename(argv.first()?);
    if let Some(nested) = runner_inner_argv(&first, argv) {
        return gate_spec_for_argv(nested);
    }
    if first == "python" && argv.len() >= 3 && argv[1] == "-m" {
        let module = argv[2].as_str();
        if PYTHON_MODULE_CHECKERS.contains(&module) {
            return Some(path_scoped_spec(
                argv,
                &argv[3..],
                module.to_string(),
                phase_from_token(module),
            ));
        }
    }
    if DIRECT_CHECKERS.contains(&first.as_str()) {
        return Some(path_scoped_spec(
            argv,
            &argv[1..],
            first.clone(),
            phase_from_token(&first),
        ));
    }
    if matches!(
        first.as_str(),
        "make" | "just" | "ninja" | "cmake" | "gradle" | "mvn" | "rk" | "xcodebuild"
    ) {
        return Some(workspace_spec(argv, first, phase_from_argv(argv)));
    }
    match first.as_str() {
        "spaceship" => spaceship_gate_spec(argv),
        "uv" => runner_inner_argv(&first, argv).and_then(gate_spec_for_argv),
        "npm" | "pnpm" | "yarn" => node_gate_spec(&first, argv),
        "bun" => bun_gate_spec(argv),
        "deno" => deno_gate_spec(argv),
        "go" => go_gate_spec(argv),
        "cargo" | "dotnet" | "swift" => subcommand_gate_spec(argv, first),
        "tox" | "nox" | "composer" => Some(workspace_spec(argv, first, BuildPhase::Test)),
        _ => fallback_subcommand_spec(argv, first),
    }
}

fn strip_env_assignments(argv: &[String]) -> &[String] {
    let mut index = 0;
    while let Some(arg) = argv.get(index) {
        let Some((name, _value)) = arg.split_once('=') else {
            break;
        };
        if arg.starts_with('-') || !name.chars().all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
            break;
        }
        index += 1;
    }
    &argv[index..]
}

fn runner_inner_argv<'a>(first: &str, argv: &'a [String]) -> Option<&'a [String]> {
    match first {
        "uv" if argv.get(1).is_some_and(|arg| matches!(arg.as_str(), "run" | "tool")) => {
            skip_runner_options(&argv[2..])
        }
        "poetry" | "pipenv" if argv.get(1).is_some_and(|arg| arg == "run") => {
            skip_runner_options(&argv[2..])
        }
        "npx" => skip_runner_options(&argv[1..]),
        _ => None,
    }
}

fn skip_runner_options(argv: &[String]) -> Option<&[String]> {
    let mut index = 0;
    while let Some(arg) = argv.get(index) {
        if arg == "--" {
            return Some(&argv[index + 1..]);
        }
        if !arg.starts_with('-') {
            return Some(&argv[index..]);
        }
        index += if option_takes_value(arg) && index + 1 < argv.len() {
            2
        } else {
            1
        };
    }
    None
}

fn node_gate_spec(first: &str, argv: &[String]) -> Option<CommandGateSpec> {
    let sub = argv.get(1)?.as_str();
    if matches!(sub, "test" | "build" | "lint" | "publish") {
        return Some(workspace_spec(argv, first.to_string(), phase_from_token(sub)));
    }
    if matches!(sub, "run" | "exec" | "dlx") && argv.len() >= 3 {
        if sub == "run" {
            return Some(workspace_spec(argv, first.to_string(), phase_from_token(&argv[2])));
        }
        return gate_spec_for_argv(&argv[2..]);
    }
    None
}

fn go_gate_spec(argv: &[String]) -> Option<CommandGateSpec> {
    let sub = argv.get(1)?.as_str();
    if !matches!(sub, "test" | "build" | "vet" | "list") {
        return None;
    }
    let paths = command_paths(&argv[2..]);
    if paths_cover_workspace(&paths) {
        return Some(workspace_spec(argv, "go".to_string(), phase_from_token(sub)));
    }
    Some(CommandGateSpec {
        argv: argv.to_vec(),
        full_workspace: paths.is_empty(),
        files: paths,
        tool: "go".to_string(),
        phase: phase_from_token(sub),
    })
}

fn bun_gate_spec(argv: &[String]) -> Option<CommandGateSpec> {
    let sub = argv.get(1)?.as_str();
    if sub == "test" {
        let paths = command_paths(&argv[2..]);
        if paths_cover_workspace(&paths) {
            return Some(workspace_spec(argv, "bun".to_string(), BuildPhase::Test));
        }
        return Some(CommandGateSpec {
            argv: argv.to_vec(),
            full_workspace: paths.is_empty(),
            files: paths,
            tool: "bun".to_string(),
            phase: BuildPhase::Test,
        });
    }
    if matches!(sub, "run" | "build") {
        return Some(workspace_spec(argv, "bun".to_string(), phase_from_token(sub)));
    }
    None
}

fn deno_gate_spec(argv: &[String]) -> Option<CommandGateSpec> {
    let sub = argv.get(1)?.as_str();
    if !matches!(sub, "check" | "fmt" | "lint" | "test") {
        return None;
    }
    let paths = command_paths(&argv[2..]);
    if paths_cover_workspace(&paths) {
        return Some(workspace_spec(argv, "deno".to_string(), phase_from_token(sub)));
    }
    Some(CommandGateSpec {
        argv: argv.to_vec(),
        full_workspace: paths.is_empty(),
        files: paths,
        tool: "deno".to_string(),
        phase: phase_from_token(sub),
    })
}

fn subcommand_gate_spec(argv: &[String], tool: String) -> Option<CommandGateSpec> {
    let sub = argv.get(1)?.as_str();
    BUILD_SUBCOMMANDS
        .contains(&sub)
        .then(|| workspace_spec(argv, tool, phase_from_token(sub)))
}

fn spaceship_gate_spec(argv: &[String]) -> Option<CommandGateSpec> {
    let sub = argv.get(1)?.as_str();
    matches!(sub, "build" | "check" | "upgrade")
        .then(|| workspace_spec(argv, "spaceship".to_string(), phase_from_token(sub)))
}

fn fallback_subcommand_spec(argv: &[String], first: String) -> Option<CommandGateSpec> {
    if !BUILD_FIRST_TOKENS.contains(&first.as_str()) {
        return None;
    }
    let sub = argv.get(1)?;
    BUILD_SUBCOMMANDS
        .contains(&sub.as_str())
        .then(|| workspace_spec(argv, first, phase_from_token(sub)))
}

fn path_scoped_spec(
    argv: &[String],
    args: &[String],
    tool: String,
    phase: BuildPhase,
) -> CommandGateSpec {
    let paths = command_paths(args);
    if paths_cover_workspace(&paths) {
        return workspace_spec(argv, tool, phase);
    }
    CommandGateSpec {
        argv: argv.to_vec(),
        full_workspace: paths.is_empty(),
        files: paths,
        tool,
        phase,
    }
}

fn workspace_spec(argv: &[String], tool: String, phase: BuildPhase) -> CommandGateSpec {
    CommandGateSpec {
        argv: argv.to_vec(),
        full_workspace: true,
        files: Vec::new(),
        tool,
        phase,
    }
}

fn command_paths(args: &[String]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut index = 0;
    let mut after_double_dash = false;
    while let Some(arg) = args.get(index) {
        if arg == "--" {
            after_double_dash = true;
            index += 1;
            continue;
        }
        if !after_double_dash && arg.starts_with('-') {
            index += if option_takes_value(arg) && index + 1 < args.len() {
                2
            } else {
                1
            };
            continue;
        }
        if looks_like_path(arg) && !paths.contains(arg) {
            paths.push(arg.clone());
        }
        index += 1;
    }
    paths
}

fn paths_cover_workspace(paths: &[String]) -> bool {
    paths
        .iter()
        .any(|path| matches!(path.as_str(), "." | "./" | "./..." | "..."))
}

fn option_takes_value(arg: &str) -> bool {
    PATHY_OPTIONS_WITH_VALUE.contains(&arg) && !arg.contains('=')
}

fn looks_like_path(arg: &str) -> bool {
    if arg.is_empty() || arg.starts_with('-') {
        return false;
    }
    matches!(arg, "." | "..")
        || arg.contains('/')
        || arg.starts_with('.')
        || Path::new(arg).extension().is_some()
        || Path::new(arg).exists()
}

fn basename(value: &str) -> String {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(value)
        .to_string()
}

fn phase_from_argv(argv: &[String]) -> BuildPhase {
    argv.get(1)
        .map(|arg| phase_from_token(arg))
        .unwrap_or(BuildPhase::Unknown)
}

fn phase_from_token(token: &str) -> BuildPhase {
    match token {
        "bench" | "test" | "pytest" | "phpunit" | "unittest" => BuildPhase::Test,
        "build" | "compile" | "install" | "package" | "publish" | "run" => BuildPhase::Build,
        "check" | "mypy" | "pyright" | "ty" | "verify" | "vet" => BuildPhase::Check,
        "black" | "biome" | "clippy" | "eslint" | "flake8" | "fmt" | "isort" | "lint"
        | "prettier" | "pylint" | "ruff" | "shellcheck" | "stylelint" => BuildPhase::Lint,
        "upgrade" => BuildPhase::Upgrade,
        _ => BuildPhase::Unknown,
    }
}

fn split_command_line(command: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in command.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            ' ' | '\t' | '\n' if !current.is_empty() => {
                argv.push(std::mem::take(&mut current));
            }
            ' ' | '\t' | '\n' => {}
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        argv.push(current);
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_common_checker_ecosystems() {
        for command in [
            "cargo check",
            "cargo clippy --all-targets",
            "go test ./...",
            "go vet ./pkg",
            "uv run ruff check src",
            "python -m pytest tests/test_cli_log.py",
            "npm test",
            "pnpm run lint",
            "yarn build",
            "dotnet test",
            "rk test",
            "make test",
            "just lint",
            "mvn test",
            "gradle check",
            "eslint src/hsp",
            "npx eslint src/hsp",
            "biome check src",
            "prettier --check src/hsp/cli.py",
            "shellcheck scripts/hsp.sh",
            "deno lint src",
            "bun test src",
            "tox",
            "nox",
            "spaceship build",
            "spaceship check",
            "spaceship upgrade 42",
        ] {
            assert!(command_gate_spec_from_line(command).is_some(), "{command}");
        }
    }

    #[test]
    fn cargo_check_is_workspace_wide() {
        let spec = command_gate_spec_from_line("cargo check").expect("cargo check spec");
        assert!(spec.full_workspace);
        assert!(spec.files.is_empty());
        assert_eq!(spec.tool, "cargo");
        assert_eq!(spec.phase, BuildPhase::Check);
    }

    #[test]
    fn checker_paths_are_file_scoped() {
        let spec = command_gate_spec_from_line("ruff check src/hsp/cli.py").expect("ruff spec");
        assert!(!spec.full_workspace);
        assert_eq!(spec.files, ["src/hsp/cli.py"]);
        assert_eq!(spec.phase, BuildPhase::Lint);
    }

    #[test]
    fn dot_scopes_are_workspace_wide() {
        for command in ["ruff check .", "go test ./..."] {
            let spec = command_gate_spec_from_line(command).expect("dot scope spec");
            assert!(spec.full_workspace);
            assert!(spec.files.is_empty());
        }
    }

    #[test]
    fn runner_options_are_skipped_before_nested_command() {
        let spec = command_gate_spec_from_line("uv run --project tools ruff check src")
            .expect("nested uv spec");
        assert_eq!(spec.tool, "ruff");
        assert_eq!(spec.files, ["src"]);
    }

    #[test]
    fn quoted_paths_survive_command_line_split() {
        let spec = command_gate_spec_from_line("pytest 'tests/path with spaces/test_cli.py'")
            .expect("quoted spec");
        assert_eq!(spec.files, ["tests/path with spaces/test_cli.py"]);
    }
}
