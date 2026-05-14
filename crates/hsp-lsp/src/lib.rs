use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainServer {
    pub command: String,
    pub args: Vec<String>,
    pub name: String,
    pub label: String,
}

impl ChainServer {
    pub fn new(command: impl Into<String>, args: Vec<String>, label: impl Into<String>) -> Self {
        let command = command.into();
        Self {
            name: command.clone(),
            command,
            args,
            label: label.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainParseError {
    message: String,
}

impl ChainParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for ChainParseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ChainParseError {}

pub fn parse_replace(raw: &str) -> BTreeMap<String, String> {
    raw.split(',')
        .filter_map(|entry| {
            let (old, new) = entry.trim().split_once('=')?;
            let old = old.trim();
            let new = new.trim();
            (!old.is_empty() && !new.is_empty()).then(|| (old.to_string(), new.to_string()))
        })
        .collect()
}

pub fn parse_chain_from_env() -> Result<Vec<ChainServer>, ChainParseError> {
    parse_chain(|name| std::env::var(name).unwrap_or_default())
}

pub fn parse_prefer_from_env(chain: &[ChainServer]) -> BTreeMap<String, usize> {
    parse_prefer(|name| std::env::var(name).unwrap_or_default(), chain)
}

pub fn parse_chain(env: impl Fn(&str) -> String) -> Result<Vec<ChainServer>, ChainParseError> {
    let replace = parse_replace(&env("LSP_REPLACE"));
    let sub = |command: &str| {
        replace
            .get(command)
            .cloned()
            .unwrap_or_else(|| command.to_string())
    };

    let servers_env = env("LSP_SERVERS");
    let servers_env = servers_env.trim();
    if !servers_env.is_empty() {
        let chain = servers_env
            .split(';')
            .filter_map(|entry| {
                let tokens = entry.split_whitespace().collect::<Vec<_>>();
                let (command, args) = tokens.split_first()?;
                let command = sub(command);
                let args = args.iter().map(|arg| (*arg).to_string()).collect::<Vec<_>>();
                Some((command, args))
            })
            .enumerate()
            .map(|(index, (command, args))| {
                let label = if index == 0 {
                    command.clone()
                } else if index == 1 {
                    format!("{command} (fallback)")
                } else {
                    format!("{command} (fallback {index})")
                };
                ChainServer::new(command, args, label)
            })
            .collect::<Vec<_>>();
        if chain.is_empty() {
            return Err(ChainParseError::new("LSP_SERVERS is empty or malformed"));
        }
        return Ok(chain);
    }

    let primary = sub(env("LSP_COMMAND").trim());
    if primary.is_empty() {
        return Err(ChainParseError::new(
            "LSP_SERVERS or LSP_COMMAND environment variable is required",
        ));
    }

    let mut chain = vec![ChainServer::new(
        primary.clone(),
        split_args(&env("LSP_ARGS")),
        primary,
    )];

    let first_fallback = sub(env("LSP_FALLBACK_COMMAND").trim());
    if !first_fallback.is_empty() {
        chain.push(ChainServer::new(
            first_fallback.clone(),
            split_args(&env("LSP_FALLBACK_ARGS")),
            format!("{first_fallback} (fallback)"),
        ));
    }

    let mut index = 2;
    loop {
        let command_name = format!("LSP_FALLBACK_{index}_COMMAND");
        let command = sub(env(&command_name).trim());
        if command.is_empty() {
            break;
        }
        let args_name = format!("LSP_FALLBACK_{index}_ARGS");
        chain.push(ChainServer::new(
            command.clone(),
            split_args(&env(&args_name)),
            format!("{command} (fallback {index})"),
        ));
        index += 1;
    }

    Ok(chain)
}

pub fn parse_prefer(env: impl Fn(&str) -> String, chain: &[ChainServer]) -> BTreeMap<String, usize> {
    let prefer = env("LSP_PREFER");
    let prefer = prefer.trim();
    if prefer.is_empty() {
        return BTreeMap::new();
    }
    let replace = parse_replace(&env("LSP_REPLACE"));

    prefer
        .split(',')
        .filter_map(|entry| {
            let (method, command) = entry.trim().split_once('=')?;
            let method = method.trim();
            let command = command.trim();
            if method.is_empty() || command.is_empty() {
                return None;
            }
            let command = replace
                .get(command)
                .map(String::as_str)
                .unwrap_or(command);
            chain
                .iter()
                .position(|server| server.command == command)
                .map(|index| (method.to_string(), index))
        })
        .collect()
}

pub fn file_uri(path: impl AsRef<Path>) -> Result<String, std::io::Error> {
    let absolute = std::path::absolute(path)?;
    Ok(format!("file://{}", percent_encode_path(&absolute)))
}

pub fn language_id_for_path(path: impl AsRef<Path>) -> &'static str {
    match path.as_ref().extension().and_then(|extension| extension.to_str()) {
        Some("py" | "pyi") => "python",
        Some("rs") => "rust",
        Some("go") => "go",
        Some("js") => "javascript",
        Some("ts") => "typescript",
        Some("jsx") => "javascriptreact",
        Some("tsx") => "typescriptreact",
        Some("java") => "java",
        Some("c" | "h") => "c",
        Some("cpp") => "cpp",
        Some("rb") => "ruby",
        Some("lua") => "lua",
        Some("zig") => "zig",
        _ => "plaintext",
    }
}

pub fn language_id_for_uri(uri: &str) -> &'static str {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    language_id_for_path(path)
}

fn split_args(raw: &str) -> Vec<String> {
    raw.split_whitespace().map(str::to_string).collect()
}

fn percent_encode_path(path: &Path) -> String {
    path.to_string_lossy()
        .as_bytes()
        .iter()
        .flat_map(|byte| match *byte {
            b'/' | b'-' | b'.' | b'_' | b'~' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' => {
                vec![*byte as char]
            }
            byte => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> String + 'a {
        |name| {
            values
                .iter()
                .find_map(|(key, value)| (*key == name).then(|| (*value).to_string()))
                .unwrap_or_default()
        }
    }

    #[test]
    fn parse_replace_ignores_malformed_entries() {
        assert_eq!(
            parse_replace("basedpyright=pylance, nope, ty = ty-nightly "),
            BTreeMap::from([
                ("basedpyright".to_string(), "pylance".to_string()),
                ("ty".to_string(), "ty-nightly".to_string()),
            ])
        );
    }

    #[test]
    fn parse_lsp_servers_chain_with_replacements() {
        let chain = parse_chain(env(&[
            ("LSP_REPLACE", "basedpyright-langserver=pylance-language-server"),
            (
                "LSP_SERVERS",
                "ty server; basedpyright-langserver --stdio; pyright-langserver --stdio",
            ),
        ]))
        .unwrap();

        assert_eq!(chain[0], ChainServer::new("ty", vec!["server".to_string()], "ty"));
        assert_eq!(chain[1].command, "pylance-language-server");
        assert_eq!(chain[1].label, "pylance-language-server (fallback)");
        assert_eq!(chain[2].label, "pyright-langserver (fallback 2)");
    }

    #[test]
    fn parse_legacy_chain_and_numbered_fallbacks() {
        let chain = parse_chain(env(&[
            ("LSP_COMMAND", "rust-analyzer"),
            ("LSP_FALLBACK_COMMAND", "ra-mirror"),
            ("LSP_FALLBACK_ARGS", "--stdio"),
            ("LSP_FALLBACK_2_COMMAND", "ra-cold"),
            ("LSP_FALLBACK_2_ARGS", "--cold"),
        ]))
        .unwrap();

        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].label, "rust-analyzer");
        assert_eq!(chain[1].args, vec!["--stdio"]);
        assert_eq!(chain[2].label, "ra-cold (fallback 2)");
    }

    #[test]
    fn parse_prefer_uses_replaced_command_names() {
        let chain = parse_chain(env(&[
            ("LSP_REPLACE", "basedpyright-langserver=pylance-language-server"),
            ("LSP_SERVERS", "ty server; basedpyright-langserver --stdio"),
        ]))
        .unwrap();
        let prefer = parse_prefer(
            env(&[
                ("LSP_REPLACE", "basedpyright-langserver=pylance-language-server"),
                ("LSP_PREFER", "workspace/willRenameFiles=basedpyright-langserver"),
            ]),
            &chain,
        );

        assert_eq!(prefer["workspace/willRenameFiles"], 1);
    }

    #[test]
    fn language_id_matches_reference_extension_map() {
        assert_eq!(language_id_for_path("x.py"), "python");
        assert_eq!(language_id_for_path("x.tsx"), "typescriptreact");
        assert_eq!(language_id_for_path("x.unknown"), "plaintext");
    }

    #[test]
    fn file_uri_uses_file_scheme_and_percent_encoding() {
        let uri = file_uri(std::path::PathBuf::from("a path.rs")).unwrap();

        assert!(uri.starts_with("file:///"));
        assert!(uri.ends_with("a%20path.rs"));
    }
}
