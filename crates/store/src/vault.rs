//! Memories as Markdown notes in an Obsidian vault: `<vault>/ferrobot/<agent>/<key>.md` and
//! `<vault>/ferrobot/Global memory/<key>.md`, so the operator reads and edits them in Obsidian.
//! The setting `memory_vault` turns it on; the `memories` tables then stay untouched.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ferrobot_core::AgentId;
use rusqlite::{Connection, OptionalExtension};
use time::OffsetDateTime;

use crate::memory::MemoryEntry;
use crate::{Store, StoreError};

pub(crate) const SETTING: &str = "memory_vault";
const ROOT: &str = "ferrobot";
/// Agent names are lowercase, so this never clashes with an agent's folder.
const GLOBAL: &str = "Global memory";

fn io(e: &std::io::Error) -> StoreError {
    StoreError::Vault(e.to_string())
}

/// The vault folder, when memories live there.
pub(crate) fn vault(c: &Connection) -> Result<Option<PathBuf>, StoreError> {
    let raw: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [SETTING],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|r| serde_json::from_str::<Option<PathBuf>>(&r).map_err(StoreError::corrupt))
        .transpose()
        .map(Option::flatten)
}

/// Where a scope's notes live: an agent's own folder (by its current name) or the global one.
pub(crate) fn folder(
    c: &Connection,
    vault: &Path,
    agent: Option<AgentId>,
) -> Result<PathBuf, StoreError> {
    let name = match agent {
        None => GLOBAL.to_owned(),
        Some(id) => c
            .query_row(
                "SELECT name FROM agents WHERE id = ?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound {
                entity: "agent",
                key: id.to_string(),
            })?,
    };
    Ok(vault.join(ROOT).join(name))
}

/// A file name for `key`: no path separators, no leading dot, nothing Obsidian or Windows rejects.
fn file_stem(key: &str) -> String {
    let safe: String = key
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    let safe = safe.trim_matches([' ', '-']);
    if safe.is_empty() {
        "memory".to_owned()
    } else {
        safe.chars().take(100).collect()
    }
}

pub(crate) struct Note {
    pub path: PathBuf,
    pub entry: MemoryEntry,
}

fn quoted(v: &str) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// `(frontmatter fields, body)`; notes made in Obsidian may have no frontmatter.
fn split(text: &str) -> (Vec<(&str, String)>, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (Vec::new(), text);
    };
    let Some(end) = rest
        .find("\n---\n")
        .or_else(|| rest.ends_with("\n---").then(|| rest.len() - 4))
    else {
        return (Vec::new(), text);
    };
    let fields = rest[..end]
        .lines()
        .filter_map(|line| {
            let (k, v) = line.split_once(':')?;
            let v = v.trim();
            let v = serde_json::from_str::<String>(v).unwrap_or_else(|_| v.to_owned());
            Some((k.trim(), v))
        })
        .collect();
    let body = rest.get(end + 5..).unwrap_or_default();
    (fields, body)
}

fn read(path: &Path) -> Option<Note> {
    let text = std::fs::read_to_string(path).ok()?;
    let modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let (fields, body) = split(&text);
    let field = |name: &str| {
        fields
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.clone())
    };
    let key = field("key").unwrap_or_else(|| {
        path.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });
    Some(Note {
        path: path.to_owned(),
        entry: MemoryEntry {
            key,
            content: body.trim_end().to_owned(),
            updated_at: OffsetDateTime::from(modified),
            author: field("author_id").and_then(|a| a.parse().ok()),
        },
    })
}

/// Every note in `dir`, oldest file name first.
pub(crate) fn notes(dir: &Path) -> Vec<Note> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "md") && p.is_file())
        .collect();
    paths.sort();
    paths.iter().filter_map(|p| read(p)).collect()
}

pub(crate) fn find(dir: &Path, key: &str) -> Option<Note> {
    notes(dir).into_iter().find(|n| n.entry.key == key)
}

/// Writes `key` as a note, into its existing file or a new one whose name no other key holds.
pub(crate) fn write(
    dir: &Path,
    key: &str,
    content: &str,
    author: Option<(AgentId, &str)>,
) -> Result<(), StoreError> {
    std::fs::create_dir_all(dir).map_err(|e| io(&e))?;
    let path = if let Some(note) = find(dir, key) {
        note.path
    } else {
        let stem = file_stem(key);
        (1..=10_000)
            .map(|n| match n {
                1 => dir.join(format!("{stem}.md")),
                n => dir.join(format!("{stem} {n}.md")),
            })
            .find(|p| !p.exists())
            .ok_or_else(|| StoreError::Vault(format!("no free file name for {stem}")))?
    };
    let mut text = format!("---\nkey: {}\n", quoted(key));
    if let Some((id, name)) = author {
        let id = quoted(&id.to_string());
        text.push_str(&["author: ", &quoted(name), "\nauthor_id: ", &id, "\n"].concat());
    }
    text.push_str("tags: [ferrobot]\n---\n");
    text.push_str(content);
    text.push('\n');
    std::fs::write(&path, text).map_err(|e| io(&e))
}

/// Best matches first: notes holding more of the query's words (as prefixes, any case), then
/// the newest. An empty query gives the newest notes.
pub(crate) fn search(dir: &Path, query: &str, limit: u32) -> Vec<MemoryEntry> {
    let words: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mut scored: Vec<(usize, MemoryEntry)> = notes(dir)
        .into_iter()
        .map(|n| {
            let text = format!("{} {}", n.entry.key, n.entry.content).to_lowercase();
            let tokens: Vec<&str> = text
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| !t.is_empty())
                .collect();
            let score = words
                .iter()
                .filter(|w| tokens.iter().any(|t| t.starts_with(w.as_str())))
                .count();
            (score, n.entry)
        })
        .filter(|(score, _)| words.is_empty() || *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.updated_at.cmp(&a.1.updated_at)));
    scored
        .into_iter()
        .take(limit as usize)
        .map(|(_, e)| e)
        .collect()
}

/// Keeps a renamed agent's notes: its folder follows the new name unless that one exists.
pub(crate) fn rename_folder(c: &Connection, old: &str, new: &str) -> Result<(), StoreError> {
    if let Some(v) = vault(c)? {
        let (from, to) = (v.join(ROOT).join(old), v.join(ROOT).join(new));
        if from.is_dir() && !to.exists() {
            std::fs::rename(from, to).map_err(|e| io(&e))?;
        }
    }
    Ok(())
}

/// A stored memory to copy: key, content, author id, author name, owning agent.
type CopyRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

impl Store {
    /// The vault folder memories live in; `None`: the built-in memory.
    pub async fn memory_vault(&self) -> Result<Option<PathBuf>, StoreError> {
        self.with(|c| vault(c)).await
    }

    /// Moves memories to `vault` (or back to the built-in memory with `None`). Memories already
    /// stored are copied into the vault without replacing notes that hold the same key; the
    /// built-in memory keeps its copy. Returns how many notes were written.
    pub async fn set_memory_vault(&self, vault: Option<PathBuf>) -> Result<usize, StoreError> {
        self.with(move |c| {
            let mut copied = 0;
            if let Some(v) = &vault {
                let mut stmt = c.prepare(
                    "SELECT m.key, m.content, NULL, NULL, a.id FROM memories m
                     JOIN agents a ON a.id = m.agent_id
                     UNION ALL
                     SELECT g.key, g.content, g.author, w.name, NULL FROM global_memories g
                     LEFT JOIN agents w ON w.id = g.author",
                )?;
                let rows: Vec<CopyRow> = stmt
                    .query_map([], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })?
                    .collect::<Result<_, _>>()?;
                for (key, content, author, author_name, agent) in rows {
                    let agent = agent.and_then(|a| a.parse::<AgentId>().ok());
                    let dir = folder(c, v, agent)?;
                    if find(&dir, &key).is_some() {
                        continue;
                    }
                    let author = author
                        .and_then(|a| a.parse::<AgentId>().ok())
                        .zip(author_name);
                    write(
                        &dir,
                        &key,
                        &content,
                        author.as_ref().map(|(id, n)| (*id, n.as_str())),
                    )?;
                    copied += 1;
                }
            }
            let value = serde_json::to_string(&vault).map_err(StoreError::corrupt)?;
            c.execute(
                "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value,
                                                 updated_at = excluded.updated_at",
                rusqlite::params![SETTING, value, crate::util::fmt_ts(crate::util::now())],
            )?;
            Ok(copied)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_become_safe_file_names() {
        assert_eq!(file_stem("deploy-days"), "deploy-days");
        assert_eq!(file_stem("../../etc/passwd"), "etc-passwd");
        assert_eq!(file_stem(".hidden"), "hidden");
        assert_eq!(file_stem("a/b\\c:d"), "a-b-c-d");
        assert_eq!(file_stem("..."), "memory");
    }

    #[test]
    fn notes_keep_their_key_and_author_and_obsidian_notes_need_no_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let id = AgentId::new();
        write(dir.path(), "a/b", "first", Some((id, "ada"))).unwrap();
        write(dir.path(), "a-b", "second", None).unwrap();
        write(dir.path(), "a/b", "first, changed", Some((id, "ada"))).unwrap();
        std::fs::write(
            dir.path().join("Team rules.md"),
            "Ship on Fridays? Never.\n",
        )
        .unwrap();
        let notes = notes(dir.path());
        let got: Vec<(&str, &str, Option<AgentId>)> = notes
            .iter()
            .map(|n| {
                (
                    n.entry.key.as_str(),
                    n.entry.content.as_str(),
                    n.entry.author,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("Team rules", "Ship on Fridays? Never.", None),
                ("a-b", "second", None),
                ("a/b", "first, changed", Some(id)),
            ]
        );
        assert!(
            dir.path().join("a-b 2.md").exists(),
            "a clash gets its own file"
        );
        let found = search(dir.path(), "fri", 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "Team rules");
        assert_eq!(search(dir.path(), "", 2).len(), 2);
    }

    #[tokio::test]
    async fn with_a_vault_memories_are_notes_and_the_stored_ones_move_in() {
        use ferrobot_core::AgentName;

        use crate::{AgentPatch, NewAgent};
        let store = Store::open_in_memory().unwrap();
        let agent = |name: &str| NewAgent {
            created_by: None,
            name: AgentName::parse(name).unwrap(),
            description: String::new(),
            instructions: String::new(),
        };
        let ada = store.create_agent(agent("ada")).await.unwrap().id;
        let bob = store.create_agent(agent("bob")).await.unwrap().id;
        store
            .remember(ada, "deploy".into(), "Never on Fridays".into())
            .await
            .unwrap();
        store
            .remember_global("team".into(), "Ship daily".into(), Some(ada))
            .await
            .unwrap();
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path().join("ferrobot");
        let moved = store
            .set_memory_vault(Some(vault.path().to_owned()))
            .await
            .unwrap();
        assert_eq!(moved, 2);
        assert!(root.join("ada/deploy.md").is_file());
        let team = std::fs::read_to_string(root.join("Global memory/team.md")).unwrap();
        assert!(
            team.contains("author: \"ada\"") && team.ends_with("Ship daily\n"),
            "{team}"
        );

        assert_eq!(
            store.recall(ada, "friday".into(), 10).await.unwrap()[0].key,
            "deploy"
        );
        assert_eq!(store.recall(bob, String::new(), 10).await.unwrap(), []);
        assert!(matches!(
            store
                .remember_global("team".into(), "mine".into(), Some(bob))
                .await,
            Err(StoreError::Conflict { .. })
        ));
        assert!(matches!(
            store.forget_global("team".into(), Some(bob)).await,
            Err(StoreError::Conflict { .. })
        ));
        store
            .remember_global("team".into(), "Ship daily, test first".into(), None)
            .await
            .unwrap();
        let global = store.recall_global("test".into(), 10).await.unwrap();
        assert_eq!(
            (global[0].content.as_str(), global[0].author),
            ("Ship daily, test first", None)
        );

        let rename = AgentPatch {
            name: Some(AgentName::parse("ada-two").unwrap()),
            ..AgentPatch::default()
        };
        store.update_agent_profile(ada, rename).await.unwrap();
        assert!(
            root.join("ada-two/deploy.md").is_file(),
            "the folder follows the name"
        );
        assert!(store.forget(ada, "deploy".into()).await.unwrap());
        assert!(!root.join("ada-two/deploy.md").exists());

        store.set_memory_vault(None).await.unwrap();
        let back = store.recall(ada, String::new(), 10).await.unwrap();
        assert_eq!(
            back[0].content, "Never on Fridays",
            "the built-in memory kept its copy"
        );
    }
}
