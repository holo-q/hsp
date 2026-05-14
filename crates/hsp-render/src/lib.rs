use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub const COMPACT_LIMIT: usize = 240;
pub const SAMPLE_DEFAULT_MAX: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AliasKind {
    Symbol,
    File,
    Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasError {
    Unknown,
    Stale,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AliasIdentity {
    pub kind: AliasKind,
    pub name: String,
    pub path: String,
    pub line: u32,
    pub character: u32,
    pub symbol_kind: String,
    pub workspace_root: String,
    pub server_label: String,
    pub bucket_key: String,
    pub bucket_label: String,
}

impl AliasIdentity {
    pub fn symbol(
        name: impl Into<String>,
        path: impl Into<String>,
        line: u32,
        bucket_key: impl Into<String>,
        bucket_label: impl Into<String>,
    ) -> Self {
        Self {
            kind: AliasKind::Symbol,
            name: name.into(),
            path: path.into(),
            line,
            character: 0,
            symbol_kind: String::new(),
            workspace_root: String::new(),
            server_label: String::new(),
            bucket_key: bucket_key.into(),
            bucket_label: bucket_label.into(),
        }
    }

    pub fn file(path: impl Into<String>) -> Self {
        let path = path.into();
        Self {
            kind: AliasKind::File,
            name: path.clone(),
            path,
            line: 0,
            character: 0,
            symbol_kind: String::new(),
            workspace_root: String::new(),
            server_label: String::new(),
            bucket_key: String::new(),
            bucket_label: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasRecord {
    pub alias: String,
    pub bucket: String,
    pub member_index: u32,
    pub kind: AliasKind,
    pub identity: AliasIdentity,
    pub generation: u64,
    pub epoch_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasResolution {
    pub record: Option<AliasRecord>,
    pub error: Option<AliasError>,
    pub message: String,
}

impl AliasResolution {
    pub fn ok(record: AliasRecord) -> Self {
        Self {
            record: Some(record),
            error: None,
            message: String::new(),
        }
    }

    pub fn error(error: AliasError, message: impl Into<String>) -> Self {
        Self {
            record: None,
            error: Some(error),
            message: message.into(),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.record.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderMemorySnapshot {
    pub epoch_id: u64,
    pub generation: u64,
    pub records: Vec<AliasRecord>,
    pub bucket_for_key: Vec<(String, String)>,
    pub bucket_label: Vec<(String, String)>,
    pub bucket_member_count: Vec<(String, u32)>,
    pub next_bucket_index: u32,
    pub stale_aliases: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct RenderMemory {
    epoch_id: u64,
    generation: u64,
    records_by_identity: HashMap<AliasIdentity, AliasRecord>,
    records_by_alias: BTreeMap<String, AliasRecord>,
    bucket_for_key: BTreeMap<String, String>,
    bucket_label: BTreeMap<String, String>,
    bucket_member_count: BTreeMap<String, u32>,
    next_bucket_index: u32,
    stale_aliases: BTreeMap<String, String>,
}

impl RenderMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn epoch_id(&self) -> u64 {
        self.epoch_id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn touch(&mut self, identity: AliasIdentity) -> AliasRecord {
        if let Some(record) = self.records_by_identity.get(&identity) {
            return record.clone();
        }
        let bucket = self.allocate_bucket_prefix(&identity);
        let member = self.bucket_member_count.get(&bucket).copied().unwrap_or(0) + 1;
        self.bucket_member_count.insert(bucket.clone(), member);
        self.generation += 1;
        let record = AliasRecord {
            alias: format!("{bucket}{member}"),
            bucket,
            member_index: member,
            kind: identity.kind,
            identity,
            generation: self.generation,
            epoch_id: self.epoch_id,
        };
        self.records_by_identity
            .insert(record.identity.clone(), record.clone());
        self.records_by_alias
            .insert(record.alias.clone(), record.clone());
        record
    }

    pub fn get(&self, alias: &str) -> Option<&AliasRecord> {
        self.records_by_alias.get(alias)
    }

    pub fn records(&self) -> Vec<AliasRecord> {
        self.records_by_alias.values().cloned().collect()
    }

    pub fn lookup(&self, token: &str) -> AliasResolution {
        let raw = token.trim();
        if raw.is_empty() {
            return AliasResolution::error(AliasError::Invalid, "empty alias token");
        }
        if !raw.is_ascii() {
            return AliasResolution::error(
                AliasError::Invalid,
                format!("alias token {token:?} contains non-ASCII characters"),
            );
        }
        let inner = raw
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(raw)
            .trim();
        if inner.is_empty() {
            return AliasResolution::error(AliasError::Invalid, "empty alias token");
        }
        if inner.chars().all(|ch| ch.is_ascii_digit()) {
            return AliasResolution::error(
                AliasError::Invalid,
                format!("{token:?} is a graph handle, not a render-memory alias"),
            );
        }
        let split = inner
            .char_indices()
            .find(|(_, ch)| ch.is_ascii_digit())
            .map(|(index, _)| index);
        let Some(split) = split else {
            return AliasResolution::error(
                AliasError::Invalid,
                format!("alias token {token:?} does not match [A-Za-z]+\\d+"),
            );
        };
        let (bucket, member) = inner.split_at(split);
        if bucket.is_empty()
            || !bucket.chars().all(|ch| ch.is_ascii_alphabetic())
            || member.is_empty()
            || !member.chars().all(|ch| ch.is_ascii_digit())
        {
            return AliasResolution::error(
                AliasError::Invalid,
                format!("alias token {token:?} does not match [A-Za-z]+\\d+"),
            );
        }
        let Ok(member) = member.parse::<u32>() else {
            return AliasResolution::error(AliasError::Invalid, "alias member index is invalid");
        };
        if member == 0 {
            return AliasResolution::error(
                AliasError::Invalid,
                format!("alias member index in {token:?} must be positive"),
            );
        }
        let alias = format!("{}{member}", bucket.to_ascii_uppercase());
        if let Some(record) = self.records_by_alias.get(&alias) {
            return AliasResolution::ok(record.clone());
        }
        if let Some(reason) = self.stale_aliases.get(&alias) {
            return AliasResolution::error(
                AliasError::Stale,
                format!("Alias {alias} is stale: {reason}"),
            );
        }
        AliasResolution::error(
            AliasError::Unknown,
            format!(
                "Alias {alias} is not active in render memory gen={}. Run lsp_memory(action='legend') or re-anchor with lsp_grep.",
                self.generation,
            ),
        )
    }

    pub fn mark_stale(&mut self, alias: &str, reason: &str) -> Option<AliasRecord> {
        let record = self.records_by_alias.remove(alias)?;
        self.records_by_identity.remove(&record.identity);
        self.stale_aliases.insert(
            record.alias.clone(),
            if reason.is_empty() {
                "alias retired".to_string()
            } else {
                reason.to_string()
            },
        );
        self.generation += 1;
        Some(record)
    }

    pub fn clear_epoch(&mut self) {
        self.records_by_identity.clear();
        self.records_by_alias.clear();
        self.bucket_for_key.clear();
        self.bucket_label.clear();
        self.bucket_member_count.clear();
        self.stale_aliases.clear();
        self.next_bucket_index = 0;
        self.epoch_id += 1;
        self.generation += 1;
    }

    pub fn aliases_for_response(&self, records: &[AliasRecord], delta: bool) -> String {
        let mut grouped: BTreeMap<String, Vec<AliasRecord>> = BTreeMap::new();
        for record in records {
            grouped
                .entry(record.bucket.clone())
                .or_default()
                .push(record.clone());
        }
        if grouped.is_empty() {
            return String::new();
        }
        let mut lines = vec![format!(
            "legend{} gen={}:",
            if delta { "+" } else { "" },
            self.generation
        )];
        for (bucket, mut members) in grouped {
            members.sort_by_key(|record| record.member_index);
            if bucket == "F" || bucket == "T" {
                for record in members {
                    lines.push(format!("  {}", member_chip(&record)));
                }
                continue;
            }
            let chips = members
                .iter()
                .map(member_chip)
                .collect::<Vec<_>>()
                .join("  ");
            let label = self.bucket_label.get(&bucket).cloned().unwrap_or_default();
            if label.is_empty() {
                lines.push(format!("  {chips}"));
            } else {
                lines.push(format!("  {bucket}={label}  {chips}"));
            }
        }
        lines.join("\n")
    }

    pub fn snapshot(&self) -> RenderMemorySnapshot {
        RenderMemorySnapshot {
            epoch_id: self.epoch_id,
            generation: self.generation,
            records: self.records_by_alias.values().cloned().collect(),
            bucket_for_key: self
                .bucket_for_key
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            bucket_label: self
                .bucket_label
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            bucket_member_count: self
                .bucket_member_count
                .iter()
                .map(|(key, value)| (key.clone(), *value))
                .collect(),
            next_bucket_index: self.next_bucket_index,
            stale_aliases: self
                .stale_aliases
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        }
    }

    pub fn restore(&mut self, snapshot: RenderMemorySnapshot) {
        self.epoch_id = snapshot.epoch_id;
        self.generation = snapshot.generation;
        self.records_by_alias = snapshot
            .records
            .iter()
            .map(|record| (record.alias.clone(), record.clone()))
            .collect();
        self.records_by_identity = snapshot
            .records
            .into_iter()
            .map(|record| (record.identity.clone(), record))
            .collect();
        self.bucket_for_key = snapshot.bucket_for_key.into_iter().collect();
        self.bucket_label = snapshot.bucket_label.into_iter().collect();
        self.bucket_member_count = snapshot.bucket_member_count.into_iter().collect();
        self.next_bucket_index = snapshot.next_bucket_index;
        self.stale_aliases = snapshot.stale_aliases.into_iter().collect();
    }

    fn allocate_bucket_prefix(&mut self, identity: &AliasIdentity) -> String {
        match identity.kind {
            AliasKind::File => return "F".to_string(),
            AliasKind::Type => return "T".to_string(),
            AliasKind::Symbol => {}
        }
        let key = if !identity.bucket_key.is_empty() {
            identity.bucket_key.clone()
        } else if !identity.path.is_empty() {
            identity.path.clone()
        } else {
            identity.name.clone()
        };
        if let Some(prefix) = self.bucket_for_key.get(&key) {
            if !identity.bucket_label.is_empty()
                && self
                    .bucket_label
                    .get(prefix)
                    .is_none_or(|label| label.is_empty())
            {
                self.bucket_label
                    .insert(prefix.clone(), identity.bucket_label.clone());
            }
            return prefix.clone();
        }
        let prefix = self.next_symbol_prefix();
        self.bucket_for_key.insert(key.clone(), prefix.clone());
        self.bucket_label.insert(
            prefix.clone(),
            if identity.bucket_label.is_empty() {
                key
            } else {
                identity.bucket_label.clone()
            },
        );
        prefix
    }

    fn next_symbol_prefix(&mut self) -> String {
        loop {
            let candidate = index_to_alpha(self.next_bucket_index);
            self.next_bucket_index += 1;
            if candidate != "F" && candidate != "T" {
                return candidate;
            }
        }
    }
}

pub fn compact_one_line(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars()
        .take(limit.saturating_sub(3))
        .collect::<String>()
        + "..."
}

pub fn format_sample_lines(lines: &[u32], max_shown: usize) -> String {
    let shown = &lines[..lines.len().min(max_shown)];
    let mut parts = shown
        .iter()
        .map(|line| format!("L{line}"))
        .collect::<Vec<_>>();
    if lines.len() > shown.len() {
        parts.push("...".to_string());
    }
    parts.join(",")
}

pub fn format_sample_locs(
    locs: &[(Option<String>, u32)],
    max_shown: usize,
    primary_path: Option<&str>,
) -> String {
    let shown = &locs[..locs.len().min(max_shown)];
    let mut parts = Vec::new();
    for (path, line) in shown {
        if path.as_deref().is_none_or(|path| Some(path) == primary_path) {
            parts.push(format!("L{line}"));
        } else if let Some(path) = path {
            parts.push(format!(
                "{}:L{line}",
                Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(path)
            ));
        }
    }
    if locs.len() > shown.len() {
        parts.push("...".to_string());
    }
    parts.join(",")
}

pub fn format_truncation_footer(more: usize, kind: &str, knob: &str) -> String {
    format!("... +{more} more {kind}; raise {knob} to unfold.")
}

pub fn format_empty_state(scope: &str, target: Option<&str>) -> String {
    match target {
        Some(target) if !target.is_empty() => format!("No {scope} for {target}."),
        _ => format!("No {scope}."),
    }
}

pub fn format_compact_row(parts: &[&str], sep: &str, limit: usize) -> String {
    let text = parts
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(sep);
    compact_one_line(&text.split_whitespace().collect::<Vec<_>>().join(" "), limit)
}

pub fn format_path_dense(aliases: &[&str], edge_labels: Option<&[&str]>) -> Result<String, String> {
    if aliases.is_empty() {
        return Ok(String::new());
    }
    if let Some(edge_labels) = edge_labels {
        let expected = aliases.len().saturating_sub(1);
        if edge_labels.len() != expected {
            return Err(format!(
                "edge_labels has {} entries but {} aliases need {expected} edges",
                edge_labels.len(),
                aliases.len(),
            ));
        }
    }
    let mut chunks = vec![aliases[0].to_string()];
    for index in 0..aliases.len() - 1 {
        if let Some(edge_labels) = edge_labels {
            chunks.push(format!(" -{}-> ", edge_labels[index]));
        } else {
            chunks.push(" -> ".to_string());
        }
        chunks.push(aliases[index + 1].to_string());
    }
    Ok(chunks.join(""))
}

pub fn format_path_dense_header(
    handle: &str,
    cost: u32,
    hops: u32,
    status: &str,
    dense: &str,
) -> String {
    let head = format!("{handle} cost {cost} hops {hops} {status}")
        .trim()
        .to_string();
    if dense.is_empty() {
        head
    } else {
        format!("{head}  {dense}")
    }
}

pub fn format_alias_chip(handle: &str, alias: &str, tail: &str) -> String {
    [handle, alias, tail]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegendMember {
    pub alias: String,
    pub name: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegendBucket {
    pub bucket_alias: String,
    pub bucket_label: String,
    pub members: Vec<LegendMember>,
}

pub fn format_legend_block(
    buckets: &[LegendBucket],
    generation: Option<u64>,
    delta: bool,
) -> String {
    let bucket_list = buckets
        .iter()
        .filter(|bucket| !bucket.members.is_empty() || !bucket.bucket_label.is_empty())
        .collect::<Vec<_>>();
    if bucket_list.is_empty() {
        return String::new();
    }
    let mut rows = vec![match generation {
        Some(generation) => format!("legend{} gen={generation}:", if delta { "+" } else { "" }),
        None => format!("legend{}:", if delta { "+" } else { "" }),
    }];
    let renders = bucket_list
        .iter()
        .map(|bucket| format!("{}={}", bucket.bucket_alias, bucket.bucket_label))
        .collect::<Vec<_>>();
    let pad_width = renders.iter().map(String::len).max().unwrap_or(0);
    for (bucket, render) in bucket_list.iter().zip(renders) {
        let chips = bucket
            .members
            .iter()
            .map(|member| format!("{}={}@L{}", member.alias, member.name, member.line))
            .collect::<Vec<_>>();
        if chips.is_empty() {
            rows.push(format!("  {render}"));
        } else {
            rows.push(format!("  {:pad_width$}  {}", render, chips.join("  ")));
        }
    }
    rows.join("\n")
}

fn member_chip(record: &AliasRecord) -> String {
    if record.kind == AliasKind::File {
        return format!("{}={}", record.alias, record.identity.path);
    }
    format!(
        "{}={}@L{}",
        record.alias, record.identity.name, record.identity.line
    )
}

fn index_to_alpha(index: u32) -> String {
    let mut label = String::new();
    let mut current = index;
    loop {
        label.insert(0, (b'A' + (current % 26) as u8) as char);
        current /= 26;
        if current == 0 {
            break;
        }
        current -= 1;
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_are_stable_and_bucketed() {
        let mut memory = RenderMemory::new();
        let first = memory.touch(AliasIdentity::symbol(
            "render",
            "src/view.rs",
            10,
            "View",
            "view.rs::View",
        ));
        let same = memory.touch(first.identity.clone());
        let second = memory.touch(AliasIdentity::symbol(
            "update",
            "src/view.rs",
            20,
            "View",
            "view.rs::View",
        ));
        let file = memory.touch(AliasIdentity::file("src/view.rs"));

        assert_eq!(first.alias, "A1");
        assert_eq!(same.alias, "A1");
        assert_eq!(second.alias, "A2");
        assert_eq!(file.alias, "F1");
        assert!(memory.lookup("[A1]").is_ok());
    }

    #[test]
    fn stale_aliases_are_not_unknown() {
        let mut memory = RenderMemory::new();
        let record = memory.touch(AliasIdentity::file("src/lib.rs"));
        memory.mark_stale(&record.alias, "file moved");
        let resolution = memory.lookup("F1");

        assert_eq!(resolution.error, Some(AliasError::Stale));
        assert!(resolution.message.contains("file moved"));
    }

    #[test]
    fn clear_epoch_recycles_aliases_under_new_epoch() {
        let mut memory = RenderMemory::new();
        let first = memory.touch(AliasIdentity::file("a.rs"));
        memory.clear_epoch();
        let second = memory.touch(AliasIdentity::file("b.rs"));

        assert_eq!(first.alias, "F1");
        assert_eq!(second.alias, "F1");
        assert_eq!(second.epoch_id, first.epoch_id + 1);
    }

    #[test]
    fn snapshot_restore_round_trips_alias_book() {
        let mut memory = RenderMemory::new();
        memory.touch(AliasIdentity::symbol("f", "a.rs", 1, "a", "a.rs"));
        let snapshot = memory.snapshot();
        let mut restored = RenderMemory::new();
        restored.restore(snapshot);

        assert!(restored.lookup("A1").is_ok());
        assert_eq!(restored.generation(), memory.generation());
    }

    #[test]
    fn legend_renders_grouped_members() {
        let mut memory = RenderMemory::new();
        let a = memory.touch(AliasIdentity::symbol("render", "view.rs", 10, "View", "view.rs::View"));
        let b = memory.touch(AliasIdentity::symbol("update", "view.rs", 20, "View", "view.rs::View"));

        assert_eq!(
            memory.aliases_for_response(&[a, b], false),
            "legend gen=2:\n  A=view.rs::View  A1=render@L10  A2=update@L20"
        );
    }

    #[test]
    fn format_helpers_match_dense_contract() {
        assert_eq!(format_sample_lines(&[57, 694, 218, 999], 3), "L57,L694,L218,...");
        assert_eq!(
            format_path_dense(&["A3", "A7", "J1"], Some(&["calls", "refs"])).unwrap(),
            "A3 -calls-> A7 -refs-> J1"
        );
        assert_eq!(
            format_path_dense_header("[P0]", 3, 3, "verified", "A3 -> A7 -> J1"),
            "[P0] cost 3 hops 3 verified  A3 -> A7 -> J1"
        );
        assert_eq!(
            format_truncation_footer(7, "refs", "max_refs"),
            "... +7 more refs; raise max_refs to unfold."
        );
    }

    #[test]
    fn format_legend_block_aligns_buckets() {
        let text = format_legend_block(
            &[
                LegendBucket {
                    bucket_alias: "A".to_string(),
                    bucket_label: "View".to_string(),
                    members: vec![LegendMember {
                        alias: "A1".to_string(),
                        name: "render".to_string(),
                        line: 10,
                    }],
                },
                LegendBucket {
                    bucket_alias: "AA".to_string(),
                    bucket_label: "Controller".to_string(),
                    members: vec![LegendMember {
                        alias: "AA1".to_string(),
                        name: "tick".to_string(),
                        line: 20,
                    }],
                },
            ],
            Some(2),
            false,
        );

        assert_eq!(
            text,
            "legend gen=2:\n  A=View         A1=render@L10\n  AA=Controller  AA1=tick@L20"
        );
    }
}
