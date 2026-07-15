// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::sync::LazyLock;

use regex::Regex;

pub const TROJANS: &[&str] = &[
    "most recent",
    "the latest",
    "the right one",
    "inactive",
    "stale",
    "clean up",
    "cleanup",
    "tidy up",
    "the team",
    "the migration",
    "best",
    "soon",
    "asap",
    "production-ready",
    "small change",
    "the usual",
    "old",
    "unused",
    "duplicates",
    "outdated",
    "everyone",
    "the important ones",
    "recent",
    "active users",
    "temporary",
    "obsolete",
];

static DEFUSERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(\b(older|newer|more|less|greater)\s+than\b|\b\d+\s*(days?|hours?|weeks?|months?|years?|%|percent)\b|\bwhere\s+\w+\s*(=|<|>|!=)|\bdefined as\b|\bmeaning\b|\bi\.e\.|\bspecifically\b)",
    )
    .expect("trojan defuser regex")
});

pub fn detect(text: &str) -> Vec<String> {
    let t = text.to_lowercase();
    let mut found: Vec<String> = TROJANS
        .iter()
        .filter(|p| {
            let re = format!(r"\b{}\b", regex::escape(p));
            Regex::new(&re).map(|r| r.is_match(&t)).unwrap_or(false)
        })
        .map(|s| (*s).to_string())
        .collect();
    found.sort();
    found.dedup();
    let deduped: Vec<String> = found
        .iter()
        .filter(|p| !found.iter().any(|q| *p != q && q.contains(p.as_str())))
        .cloned()
        .collect();
    found = deduped;
    if found.is_empty() {
        return vec![];
    }
    let mut kept = Vec::new();
    for p in &found {
        let re = format!(r"\b{}\b", regex::escape(p));
        let Some(m) = Regex::new(&re).ok().and_then(|r| r.find(&t)) else {
            continue;
        };
        let window = &t[m.end()..t.len().min(m.end() + 60)];
        if !DEFUSERS.is_match(window) {
            kept.push(p.clone());
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_inactive() {
        assert_eq!(detect("delete inactive users"), vec!["inactive"]);
    }

    #[test]
    fn defuses_quantified() {
        assert!(detect("delete users inactive for more than 90 days").is_empty());
    }

    #[test]
    fn subphrase_dedup() {
        let d = detect("the most recent order");
        assert_eq!(d, vec!["most recent"]);
    }
}
