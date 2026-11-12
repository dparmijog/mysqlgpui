//! Editor autocomplete: SQL keywords of the connection's dialect plus the
//! open database's table and column names.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gpui_kit::component::input::{CompletionProvider, Rope, RopeExt as _};
use gpui_kit::*;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    TextEdit,
};
use sqldr_core::Table;

/// Most suggestions shown at once; prefix matching keeps real lists short.
const MAX_SUGGESTIONS: usize = 100;

/// Tables of the open database, replaced whenever the list (re)loads.
pub type SchemaHandle = Rc<RefCell<Rc<[Table]>>>;

pub struct SqlCompletions {
    keywords: &'static [&'static str],
    schema: SchemaHandle,
}

impl SqlCompletions {
    pub fn new(keywords: &'static [&'static str], schema: SchemaHandle) -> Self {
        Self { keywords, schema }
    }
}

#[derive(Debug, PartialEq)]
struct Suggestion {
    label: String,
    kind: CompletionItemKind,
    detail: Option<String>,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Suggestions for the identifier being typed at the end of `line_prefix`
/// (the current line up to the cursor). Returns the byte length of that
/// partial identifier, which the chosen suggestion replaces.
///
/// After `name.` only columns are offered: those of table `name`, or of
/// every table when `name` isn't one (most likely an alias).
fn suggest(line_prefix: &str, keywords: &[&str], tables: &[Table]) -> (usize, Vec<Suggestion>) {
    let word_start = line_prefix.rfind(|c: char| !is_ident(c)).map_or(0, |i| {
        i + line_prefix[i..].chars().next().map_or(1, char::len_utf8)
    });
    let word = &line_prefix[word_start..];
    let before = &line_prefix[..word_start];
    let word_lower = word.to_lowercase();
    let starts = |name: &str| name.to_lowercase().starts_with(&word_lower);
    let mut out = Vec::new();

    if let Some(qualified) = before.strip_suffix('.') {
        let qualifier = &qualified[qualified.rfind(|c: char| !is_ident(c)).map_or(0, |i| i + 1)..];
        let owner = tables.iter().find(|t| t.name.eq_ignore_ascii_case(qualifier));
        let scope = owner.map_or(tables, std::slice::from_ref);
        for table in scope {
            for col in table.columns.iter().filter(|c| starts(&c.name)) {
                if out.iter().any(|s: &Suggestion| s.label == col.name) {
                    continue;
                }
                out.push(Suggestion {
                    label: col.name.clone(),
                    kind: CompletionItemKind::FIELD,
                    detail: Some(format!("{} · {}", table.name, col.ty)),
                });
            }
        }
        out.truncate(MAX_SUGGESTIONS);
        return (word.len(), out);
    }

    // Nothing typed yet: a popup on every space or newline would be noise.
    if word.is_empty() {
        return (0, out);
    }
    for table in tables.iter().filter(|t| starts(&t.name)) {
        out.push(Suggestion {
            label: table.name.clone(),
            kind: CompletionItemKind::CLASS,
            detail: Some(format!("table · {} columns", table.columns.len())),
        });
    }
    for table in tables {
        for col in table.columns.iter().filter(|c| starts(&c.name)) {
            if out.iter().any(|s| s.label == col.name) {
                continue;
            }
            out.push(Suggestion {
                label: col.name.clone(),
                kind: CompletionItemKind::FIELD,
                detail: Some(format!("{} · {}", table.name, col.ty)),
            });
        }
    }
    // Keywords follow the case being typed: `sel` → `select`.
    let lowercase = word.chars().all(|c| !c.is_uppercase());
    for kw in keywords.iter().filter(|kw| starts(kw)) {
        out.push(Suggestion {
            label: if lowercase { kw.to_lowercase() } else { kw.to_string() },
            kind: CompletionItemKind::KEYWORD,
            detail: None,
        });
    }
    out.truncate(MAX_SUGGESTIONS);
    (word.len(), out)
}

impl CompletionProvider for SqlCompletions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let point = text.offset_to_point(offset);
        let line_prefix = text.slice(text.line_start_offset(point.row)..offset).to_string();
        let tables = self.schema.borrow().clone();
        let (word_len, suggestions) = suggest(&line_prefix, self.keywords, &tables);

        let range = lsp_types::Range {
            start: text.offset_to_position(offset - word_len),
            end: text.offset_to_position(offset),
        };
        let items = suggestions
            .into_iter()
            .map(|s| CompletionItem {
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range,
                    new_text: s.label.clone(),
                })),
                label: s.label,
                kind: Some(s.kind),
                detail: s.detail,
                ..Default::default()
            })
            .collect();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        !new_text.is_empty() && new_text.chars().all(|c| is_ident(c) || c == '.')
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: the module's `gpui_kit::*` glob would shadow `#[test]`.
    use super::suggest;
    use sqldr_core::{Column, Table};

    fn table(name: &str, cols: &[&str]) -> Table {
        Table {
            name: name.into(),
            columns: cols
                .iter()
                .map(|c| Column { name: c.to_string(), ty: "int".into(), nullable: false, key: None })
                .collect(),
            indexes: vec![],
            foreign_keys: vec![],
        }
    }

    fn labels(line: &str) -> (usize, Vec<String>) {
        let tables = [table("members", &["id", "name"]), table("pets", &["id", "member_id"])];
        let (len, s) = suggest(line, &["SELECT", "SET", "FROM"], &tables);
        (len, s.into_iter().map(|s| s.label).collect())
    }

    #[test]
    fn prefix_matches_tables_columns_then_keywords_in_the_typed_case() {
        assert_eq!(labels("se"), (2, vec!["select".into(), "set".into()]));
        assert_eq!(labels("SE"), (2, vec!["SELECT".into(), "SET".into()]));
        assert_eq!(labels("SELECT * FROM mem"), (3, vec!["members".into(), "member_id".into()]));
    }

    #[test]
    fn after_a_dot_only_that_tables_columns_are_offered() {
        assert_eq!(labels("WHERE pets."), (0, vec!["id".into(), "member_id".into()]));
        assert_eq!(labels("WHERE members.n"), (1, vec!["name".into()]));
        // Unknown qualifier (an alias): columns of every table, deduplicated.
        assert_eq!(labels("p.i"), (1, vec!["id".into()]));
    }

    #[test]
    fn nothing_is_suggested_before_typing_a_word() {
        assert_eq!(labels("SELECT "), (0, vec![]));
    }
}
