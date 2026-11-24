use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::path::{Path, PathBuf};

/// Tool input mirroring Zed's built-in agent `edit_file` tool
/// (`crates/agent/src/tools/edit_file_tool.rs` / `edit_session.rs`).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct EditFileToolInput {
    /// The full path of the file to edit.
    pub path: String,
    /// List of edit operations to apply sequentially.
    /// Each edit finds `old_text` in the file and replaces it with `new_text`.
    pub edits: Vec<Edit>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Edit {
    /// The exact text to find in the file. Be minimal: include only the unique lines,
    /// or enough context to identify non-unique lines.
    pub old_text: String,
    /// The text to replace it with.
    pub new_text: String,
}

#[derive(Debug)]
pub enum EditFileOutcome {
    /// No `edits` were requested; the file was left untouched.
    NoEdits,
    /// The edits were applied and written to disk.
    Success { summary: String },
    /// No edits were applied; the error explains what happened.
    Failure { error: String },
}

pub const EDIT_FILE_TOOL_NAME: &str = "edit_file";
const CONTEXT_LINES: usize = 3;

pub fn tool_json_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["path", "edits"],
        "properties": {
            "path": {
                "type": "string",
                "description": "The full path of the file to edit in the project."
            },
            "edits": {
                "type": "array",
                "description": "List of edit operations to apply sequentially.",
                "items": {
                    "type": "object",
                    "required": ["old_text", "new_text"],
                    "properties": {
                        "old_text": {
                            "type": "string",
                            "description": "The exact text to find in the file. Be minimal: \
                                            include only the unique lines, or enough context \
                                            to identify non-unique lines."
                        },
                        "new_text": {
                            "type": "string",
                            "description": "The text to replace it with."
                        }
                    }
                }
            }
        }
    })
}

/// Applies `edits` to the file at `path` and writes the result to disk, mirroring the
/// semantics of the built-in Zed agent `edit_file` tool: edits are applied sequentially,
/// each `old_text` must identify exactly one location in the current file content, and
/// failures report the requested excerpt plus surrounding context.
pub fn edit_file(path: &str, edits: &[Edit]) -> EditFileOutcome {
    let requested_path = PathBuf::from(path);
    match apply_edits(&requested_path, edits) {
        Err(error) => EditFileOutcome::Failure {
            error: failure_message(&requested_path, error),
        },
        Ok(None) => EditFileOutcome::NoEdits,
        Ok(Some(new_text)) => {
            if let Err(error) = std::fs::write(&requested_path, new_text.as_bytes()) {
                return EditFileOutcome::Failure {
                    error: format!(
                        "Failed to edit {}: the edited file could not be written: {error}",
                        requested_path.display()
                    ),
                };
            }
            let summary = format!(
                "Edited {} successfully\n{}",
                requested_path.display(),
                excerpt_of_first_change(edits, &new_text)
            );
            EditFileOutcome::Success { summary }
        }
    }
}

enum EditError {
    Io(String),
    NonUtf8,
    Ambiguous { lines: Vec<usize> },
    NotFound { excerpt: String },
    EmptyOldText,
}

fn apply_edits(path: &Path, edits: &[Edit]) -> Result<Option<String>, EditError> {
    if !path.exists() {
        return Err(EditError::Io(format!("no such file: {}", path.display())));
    }
    let bytes = std::fs::read(path).map_err(|error| {
        EditError::Io(format!("{} could not be read: {error}", path.display()))
    })?;
    let mut text = String::from_utf8(bytes).map_err(|_| EditError::NonUtf8)?;
    let mut edited = false;
    for (edit_index, edit) in edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(EditError::EmptyOldText);
        }
        let mut occurrences = find_exact(&text, &edit.old_text);
        if occurrences.is_empty() {
            occurrences = find_fuzzy(&text, &edit.old_text);
            match occurrences.len() {
                0 => {
                    return Err(EditError::NotFound {
                        excerpt: failure_excerpt(&text, &edit.old_text, edit_index),
                    });
                }
                1 => {}
                _ => {
                    return Err(EditError::Ambiguous {
                        lines: occurrences.iter().map(|o| o.line + 1).collect(),
                    });
                }
            }
        } else if occurrences.len() > 1 {
            return Err(EditError::Ambiguous {
                lines: occurrences.iter().map(|o| o.line + 1).collect(),
            });
        }
        let occurrence = &occurrences[0];
        let replacement = edit.new_text.replace('\r', "");
        let byte_range = match &occurrence.byte_range {
            RangeOrLines::Range(range) => range.clone(),
            RangeOrLines::Lines { start, end } => {
                let start_byte = text
                    .line_indices()
                    .nth(*start)
                    .map(|(byte, _)| byte)
                    .unwrap_or(text.len());
                let end_byte = text
                    .line_indices()
                    .nth(*end)
                    .map(|(byte, _)| byte)
                    .unwrap_or(text.len());
                start_byte..end_byte
            }
        };
        text.replace_range(byte_range.clone(), &replacement);
        edited = true;
    }
    Ok(if edited { Some(text) } else { None })
}

/// Occurrence reports the 0-based `line` the fix begins at, along with either a
/// precise byte range (exact match) or the 0-based line span it covers (fuzzy match).
struct Occurrence {
    line: usize,
    byte_range: RangeOrLines,
}

enum RangeOrLines {
    Range(Range<usize>),
    Lines { start: usize, end: usize },
}

trait LineIndices {
    fn line_indices(&self) -> impl Iterator<Item = (usize, &str)>;
}

impl LineIndices for str {
    fn line_indices(&self) -> impl Iterator<Item = (usize, &str)> {
        self.split_inclusive('\n').enumerate()
    }
}

fn find_exact(text: &str, pattern: &str) -> Vec<Occurrence> {
    text.match_indices(pattern)
        .map(|(byte_offset, _)| Occurrence {
            line: text[..byte_offset].matches('\n').count(),
            byte_range: RangeOrLines::Range(byte_offset..byte_offset + pattern.len()),
        })
        .collect()
}

/// Fuzzy fallback mirroring Zed's whitespace-insensitive edit matching: a pattern
/// matches a span of lines whose trimmed, non-empty lines equal the pattern's trimmed,
/// non-empty lines in order. Each match is reported at most once.
fn find_fuzzy(text: &str, pattern: &str) -> Vec<Occurrence> {
    let pattern_lines: Vec<&str> = pattern
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect();
    if pattern_lines.is_empty() {
        return Vec::new();
    }
    let lines: Vec<(usize, &str)> = text.split_inclusive('\n').enumerate().collect();
    let mut occurrences = Vec::new();
    for (start, (line_index, _)) in lines.iter().enumerate() {
        let mut pattern_index = 0;
        let mut end = start;
        for (_, line) in lines.iter().skip(start) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                end += 1;
                continue;
            }
            if trimmed == pattern_lines[pattern_index] {
                pattern_index += 1;
                end += 1;
                if pattern_index == pattern_lines.len() {
                    break;
                }
            } else if pattern_index > 0 {
                break;
            } else {
                pattern_index = 0;
                end += 1;
                break;
            }
        }
        if pattern_index == pattern_lines.len() {
            occurrences.push(Occurrence {
                line: *line_index,
                byte_range: RangeOrLines::Lines { start, end },
            });
            if occurrences.len() > 1 {
                break;
            }
        }
    }
    occurrences
}

/// Excerpt of the region of `text` closest to `pattern`, formatted like Zed's
/// `read_file` output: a right-aligned 6-character line number, a tab, then content.
fn excerpt_around(text: &str, pattern: &str) -> String {
    let first_pattern_line = pattern.lines().find_map(|line| {
        let trimmed = line.trim();
        (!trimmed.is_empty()).then_some(trimmed.to_string())
    });
    let Some(first_pattern_line) = first_pattern_line else {
        return String::new();
    };
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let anchor = lines
        .iter()
        .position(|line| line.trim().contains(&first_pattern_line))
        .unwrap_or(0);
    let start = anchor.saturating_sub(CONTEXT_LINES);
    let end = (anchor + CONTEXT_LINES + 1).min(lines.len());
    format_numbered(&lines[start..end], start)
}

fn failure_excerpt(text: &str, pattern: &str, edit_index: usize) -> String {
    format!(
        "Edit {edit_index}: the requested excerpt from the file:\n{}",
        excerpt_around(text, pattern)
    )
}

fn excerpt_of_first_change(edits: &[Edit], new_text: &str) -> String {
    let text_lines: Vec<&str> = new_text.split_inclusive('\n').collect();
    let anchor = edits
        .first()
        .and_then(|edit| {
            let first_new_line = edit.new_text.lines().find_map(|line| {
                let trimmed = line.trim();
                (!trimmed.is_empty()).then_some(trimmed.to_string())
            })?;
            text_lines
                .iter()
                .position(|line| line.trim().contains(&first_new_line))
        })
        .unwrap_or(0);
    let start = anchor.saturating_sub(CONTEXT_LINES);
    let end = (anchor + CONTEXT_LINES * 2 + 1).min(text_lines.len());
    format_numbered(&text_lines[start..end], start)
}

fn format_numbered(lines: &[&str], start_line: usize) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{:>6}\t{}", start_line + index + 1, line))
        .collect()
}

fn failure_message(path: &Path, error: EditError) -> String {
    match error {
        EditError::Io(message) => format!("Failed to edit {}: {message}", path.display()),
        EditError::NonUtf8 => format!(
            "Failed to edit {}: the file is not valid UTF-8.",
            path.display()
        ),
        EditError::EmptyOldText => format!(
            "Failed to edit {}: `old_text` in an edit is empty. Provide the exact text to replace.",
            path.display()
        ),
        EditError::Ambiguous { lines } => format!(
            "Failed to edit {}: the string to replace is not unique. It occurs on lines: {}.",
            path.display(),
            lines
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        EditError::NotFound { excerpt } => {
            format!("Failed to edit {}: no match found.\n{excerpt}", path.display())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempFile {
        _dir: tempfile::TempDir,
        path: PathBuf,
    }

    fn temp_file(contents: &str) -> TempFile {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, contents).unwrap();
        TempFile { _dir: dir, path }
    }

    fn edit(old_text: &str, new_text: &str) -> Edit {
        Edit {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
        }
    }

    #[test]
    fn edits_reduce_to_a_single_unique_match() {
        let temp = temp_file("line 1\nline 2\nline 3\n");
        let outcome = edit_file(
            &temp.path.display().to_string(),
            &[edit("line 2", "modified line 2")],
        );
        let EditFileOutcome::Success { summary } = outcome else {
            panic!("expected success, got {outcome:?}");
        };
        assert!(summary.starts_with("Edited"), "{summary}");
        assert!(summary.contains("modified line 2"), "{summary}");
        assert_eq!(
            std::fs::read_to_string(&temp.path).unwrap(),
            "line 1\nmodified line 2\nline 3\n"
        );
    }

    #[test]
    fn errors_when_the_match_is_ambiguous() {
        let temp = temp_file("greet();\nlog();\ngreet();\n");
        let outcome = edit_file(
            &temp.path.display().to_string(),
            &[edit("greet();", "hello();")],
        );
        let EditFileOutcome::Failure { error } = outcome else {
            panic!("expected failure");
        };
        assert!(error.contains("not unique"), "{error}");
        assert!(error.contains("1, 3"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&temp.path).unwrap(),
            "greet();\nlog();\ngreet();\n"
        );
    }

    #[test]
    fn errors_when_the_match_is_missing() {
        let temp = temp_file("fn main() {\n    run();\n}\n");
        let outcome = edit_file(
            &temp.path.display().to_string(),
            &[edit("fn main() {\n    fly();\n}", "fn main() {\n}\n")],
        );
        let EditFileOutcome::Failure { error } = outcome else {
            panic!("expected failure");
        };
        assert!(error.contains("no match found"), "{error}");
        assert!(error.contains("run();"), "{error}");
    }

    #[test]
    fn errors_when_the_file_is_missing() {
        let outcome = edit_file(
            "/nonexistent/zef-edit-file-mcp/file.txt",
            &[edit("a", "b")],
        );
        let EditFileOutcome::Failure { error } = outcome else {
            panic!("expected failure");
        };
        assert!(error.contains("no such file"), "{error}");
    }

    #[test]
    fn matching_is_whitespace_insensitive_as_a_fallback() {
        let temp = temp_file("fn main() {\n    run();\n}\n");
        let outcome = edit_file(
            &temp.path.display().to_string(),
            &[edit("fn main() {\nrun();\n}", "fn main() {\n    run2();\n}\n")],
        );
        let EditFileOutcome::Success { .. } = outcome else {
            panic!("expected success, got {outcome:?}");
        };
        assert_eq!(
            std::fs::read_to_string(&temp.path).unwrap(),
            "fn main() {\n    run2();\n}\n"
        );
    }

    #[test]
    fn whitespace_insensitive_matching_still_requires_uniqueness() {
        let temp = temp_file("item\nother\n    item\n");
        let outcome = edit_file(
            &temp.path.display().to_string(),
            &[edit("item", "renamed")],
        );
        let EditFileOutcome::Failure { error } = outcome else {
            panic!("expected failure");
        };
        assert!(error.contains("not unique"), "{error}");
    }

    #[test]
    fn sequential_edits_apply_in_order() {
        let temp = temp_file("one\ntwo\nthree\n");
        let outcome = edit_file(
            &temp.path.display().to_string(),
            &[edit("one", "uno"), edit("two", "dos")],
        );
        assert!(matches!(outcome, EditFileOutcome::Success { .. }));
        assert_eq!(
            std::fs::read_to_string(&temp.path).unwrap(),
            "uno\ndos\nthree\n"
        );
    }

    #[test]
    fn empty_edits_list_leaves_the_file_untouched() {
        let temp = temp_file("same\n");
        let outcome = edit_file(&temp.path.display().to_string(), &[]);
        assert!(matches!(outcome, EditFileOutcome::NoEdits));
        assert_eq!(std::fs::read_to_string(&temp.path).unwrap(), "same\n");
    }

    #[test]
    fn later_edits_match_the_results_of_earlier_ones() {
        let temp = temp_file("a\nb\n");
        let outcome = edit_file(
            &temp.path.display().to_string(),
            &[edit("a", "a2"), edit("a2\nb", "a3\nb")],
        );
        assert!(matches!(outcome, EditFileOutcome::Success { .. }));
        assert_eq!(std::fs::read_to_string(&temp.path).unwrap(), "a3\nb\n");
    }

    #[test]
    fn rejects_empty_old_text() {
        let temp = temp_file("content\n");
        let outcome = edit_file(&temp.path.display().to_string(), &[edit("", "x")]);
        let EditFileOutcome::Failure { error } = outcome else {
            panic!("expected failure");
        };
        assert!(error.contains("old_text"), "{error}");
    }

    #[test]
    fn non_utf8_files_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binary.bin");
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0x01]).unwrap();
        let outcome = edit_file(&path.display().to_string(), &[edit("a", "b")]);
        let EditFileOutcome::Failure { error } = outcome else {
            panic!("expected failure");
        };
        assert!(error.contains("UTF-8"), "{error}");
    }

    #[test]
    fn json_schema_has_expected_shape() {
        let schema = tool_json_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], serde_json::json!(["path", "edits"]));
        assert_eq!(
            schema["properties"]["edits"]["items"]["required"],
            serde_json::json!(["old_text", "new_text"])
        );
    }
}
