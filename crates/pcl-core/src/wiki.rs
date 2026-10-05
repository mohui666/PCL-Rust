//! Frozen upstream MC百科 index. Platform slugs identify projects; filenames never do.
use crate::resources::ResourceProvider;
use std::sync::OnceLock;
#[derive(Debug)]
pub struct Entry {
    pub id: usize,
    pub chinese: Option<String>,
    pub curseforge: Option<String>,
    pub modrinth: Option<String>,
}
impl Entry {
    pub fn slug(&self, source: ResourceProvider) -> Option<&str> {
        match source {
            ResourceProvider::Modrinth => self.modrinth.as_deref(),
            ResourceProvider::CurseForge => self.curseforge.as_deref(),
        }
    }
    pub fn url(&self) -> String {
        format!("https://www.mcmod.cn/class/{}.html", self.id)
    }
}
fn parse(data: &str) -> Vec<Entry> {
    let mut lines: Vec<_> = data.lines().collect();
    lines.pop(); // Last line encodes popularity, not an entry.
    let mut entries = Vec::new();
    for (line_no, line) in lines.into_iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        for data in line.split('¨') {
            let parts: Vec<_> = data.split('|').collect();
            let slugs = parts[0];
            let (curseforge, modrinth) = if let Some(slug) = slugs.strip_prefix('@') {
                (None, Some(slug.to_owned()))
            } else if let Some(slug) = slugs.strip_suffix('@') {
                (Some(slug.to_owned()), Some(slug.to_owned()))
            } else if let Some((cf, mr)) = slugs.split_once('@') {
                (Some(cf.to_owned()), Some(mr.to_owned()))
            } else {
                (Some(slugs.to_owned()), None)
            };
            let chinese = parts.last().filter(|_| parts.len() >= 2).map(|name| {
                let slug = curseforge
                    .as_ref()
                    .or(modrinth.as_ref())
                    .cloned()
                    .unwrap_or_default();
                let english = slug
                    .split('-')
                    .map(|word| {
                        let mut c = word.chars();
                        c.next()
                            .map(|first| first.to_uppercase().collect::<String>() + c.as_str())
                            .unwrap_or_default()
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                name.replace('*', &format!(" ({english})"))
            });
            entries.push(Entry {
                id: line_no + 1,
                chinese,
                curseforge,
                modrinth,
            });
        }
    }
    entries
}
pub fn entries() -> &'static [Entry] {
    static DATA: OnceLock<Vec<Entry>> = OnceLock::new();
    DATA.get_or_init(|| parse(include_str!("../assets/wiki/WikiEntries.txt")))
}
pub fn find(provider: ResourceProvider, slug: &str) -> Option<&'static Entry> {
    entries().iter().find(|e| e.slug(provider) == Some(slug))
}
/// Translate only an unambiguous complete Chinese name. Ambiguous phrases stay
/// with the provider's search engine rather than silently selecting a project.
pub fn search_query(provider: ResourceProvider, query: &str) -> String {
    if !query
        .chars()
        .any(|c| ('\u{3400}'..='\u{9fff}').contains(&c))
    {
        return query.into();
    }
    let mut candidates = entries()
        .iter()
        .filter(|entry| {
            entry
                .chinese
                .as_deref()
                .is_some_and(|name| name == query || name.split(" (").next() == Some(query))
        })
        .filter_map(|entry| entry.slug(provider));
    let Some(first) = candidates.next() else {
        return query.into();
    };
    if candidates.any(|next| next != first) {
        query.into()
    } else {
        first.replace('-', " ")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_platform_aliases_and_keeps_wiki_line_ids() {
        let entries = parse("\ncommon@|共同\ncf@mr|*\n@only-mr|单平台\nxyz\n");
        assert_eq!(entries[0].id, 2);
        assert_eq!(entries[0].slug(ResourceProvider::Modrinth), Some("common"));
        assert_eq!(entries[1].chinese.as_deref(), Some(" (Cf)"));
        assert!(entries[2].curseforge.is_none());
    }
    #[test]
    fn frozen_known_project_and_chinese_query_resolve_without_network() {
        let entry = find(ResourceProvider::CurseForge, "industrial-craft").unwrap();
        assert_eq!(entry.id, 2);
        assert!(entry.chinese.as_ref().unwrap().contains("工业时代2"));
        assert_eq!(
            search_query(ResourceProvider::CurseForge, "工业时代2"),
            "industrial craft"
        );
        assert_eq!(search_query(ResourceProvider::Modrinth, "sodium"), "sodium");
        assert!(entries().len() > 1000);
    }
}
