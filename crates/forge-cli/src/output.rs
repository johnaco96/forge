//! Plain-text output helpers.
//!
//! No colour and no progress bars: Forge's output is read as often from a log
//! or a pipe as from a terminal, and the CLI stays useful after a UI exists.

/// Renders aligned `label   value` rows.
pub fn fields(rows: &[(&str, String)]) -> String {
    let width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
    rows.iter()
        .map(|(label, value)| format!("  {label:<width$}  {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders a header row plus body rows with columns aligned to their contents.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.len());
            }
        }
    }

    let render = |cells: &[String]| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                if i + 1 == cells.len() {
                    cell.clone()
                } else {
                    format!("{cell:<width$}", width = widths[i])
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };

    let header: Vec<String> = headers.iter().map(|h| h.to_uppercase()).collect();
    std::iter::once(format!("  {}", render(&header)))
        .chain(rows.iter().map(|row| format!("  {}", render(row))))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A titled block of lines.
pub fn section(title: &str, body: impl AsRef<str>) -> String {
    format!("{title}\n{}", body.as_ref())
}

/// Bullet list.
pub fn bullets<I, S>(items: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    items
        .into_iter()
        .map(|item| format!("  - {}", item.as_ref()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_align_to_the_longest_label() {
        let rendered = fields(&[
            ("Repository", "forge".to_string()),
            ("Base commit", "a73cf21".to_string()),
        ]);
        assert_eq!(rendered, "  Repository   forge\n  Base commit  a73cf21");
    }

    #[test]
    fn tables_align_columns_and_do_not_pad_the_last_one() {
        let rendered = table(
            &["agent", "harness"],
            &[
                vec!["claude".to_string(), "claude-code".to_string()],
                vec!["pi".to_string(), "pi".to_string()],
            ],
        );
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], "  AGENT   HARNESS");
        assert_eq!(lines[1], "  claude  claude-code");
        assert_eq!(lines[2], "  pi      pi");
        assert!(lines.iter().all(|l| !l.ends_with(' ')));
    }
}
