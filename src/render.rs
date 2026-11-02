use serde_json::{Map, Value};

pub fn json(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("serializing a JSON value cannot fail")
}

pub fn text(value: &Value) -> String {
    let mut lines = Vec::new();
    text_lines(value, None, &mut lines);
    lines.join("\n")
}

fn text_lines(value: &Value, label: Option<&str>, lines: &mut Vec<String>) {
    match value {
        Value::Array(items) if items.iter().all(is_scalar) => {
            push_text_line(label, items.iter().map(scalar_text).collect(), lines);
        }
        Value::Array(items) => {
            for item in items {
                text_lines(item, label, lines);
            }
        }
        Value::Object(map) => {
            let scalars: Vec<_> = map
                .values()
                .filter(|value| is_scalar(value))
                .map(scalar_text)
                .collect();
            if !scalars.is_empty() {
                push_text_line(label, scalars, lines);
            }
            for (key, child) in map.iter().filter(|(_, value)| !is_scalar(value)) {
                text_lines(child, Some(&key.to_uppercase()), lines);
            }
        }
        scalar => push_text_line(label, vec![scalar_text(scalar)], lines),
    }
}

fn push_text_line(label: Option<&str>, values: Vec<String>, lines: &mut Vec<String>) {
    let mut columns: Vec<String> = label.map(str::to_owned).into_iter().collect();
    columns.extend(values);
    lines.push(columns.join("\t"));
}

pub fn table(value: &Value) -> String {
    let mut blocks = Vec::new();
    table_blocks(value, None, &mut blocks);
    blocks.join("\n")
}

fn table_blocks(value: &Value, title: Option<&str>, blocks: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            let rows: Vec<_> = map
                .iter()
                .filter(|(_, value)| is_cell(value))
                .map(|(key, value)| vec![key.clone(), cell_text(value)])
                .collect();
            if !rows.is_empty() {
                blocks.push(grid(title, &["Key".to_owned(), "Value".to_owned()], &rows));
            }
            for (key, child) in map.iter().filter(|(_, value)| !is_cell(value)) {
                table_blocks(child, Some(key), blocks);
            }
        }
        Value::Array(items) if items.iter().all(Value::is_object) => {
            let objects: Vec<&Map<String, Value>> =
                items.iter().filter_map(Value::as_object).collect();
            let mut columns: Vec<String> = Vec::new();
            for object in &objects {
                for (key, value) in object.iter() {
                    if is_cell(value) && !columns.contains(key) {
                        columns.push(key.clone());
                    }
                }
            }
            let rows: Vec<_> = objects
                .iter()
                .map(|object| {
                    columns
                        .iter()
                        .map(|column| object.get(column).map(cell_text).unwrap_or_default())
                        .collect()
                })
                .collect();
            blocks.push(grid(title, &columns, &rows));
        }
        Value::Array(items) => {
            let rows: Vec<_> = items.iter().map(|item| vec![cell_text(item)]).collect();
            blocks.push(grid(title, &["Value".to_owned()], &rows));
        }
        scalar => blocks.push(grid(
            title,
            &["Value".to_owned()],
            &[vec![cell_text(scalar)]],
        )),
    }
}

fn grid(title: Option<&str>, header: &[String], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = header.iter().map(|cell| width(cell)).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(width(cell));
        }
    }
    let border = format!(
        "+{}+",
        widths
            .iter()
            .map(|width| "-".repeat(width + 2))
            .collect::<Vec<_>>()
            .join("+")
    );
    let line = |cells: &[String]| {
        format!(
            "|{}|",
            cells
                .iter()
                .zip(&widths)
                .map(|(cell, width_)| format!(" {cell}{} ", " ".repeat(width_ - width(cell))))
                .collect::<Vec<_>>()
                .join("|")
        )
    };
    let mut output = Vec::new();
    if let Some(title) = title {
        output.push(title.to_owned());
    }
    output.push(border.clone());
    output.push(line(header));
    output.push(border.clone());
    for row in rows {
        output.push(line(row));
    }
    output.push(border);
    output.join("\n")
}

fn width(text: &str) -> usize {
    text.chars()
        .map(|character| match character as u32 {
            0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6 => 2,
            _ => 1,
        })
        .sum()
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Array(_) | Value::Object(_))
}

fn is_cell(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().all(is_scalar),
        Value::Object(_) => false,
        _ => true,
    }
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn cell_text(value: &Value) -> String {
    match value {
        Value::Array(items) => items.iter().map(scalar_text).collect::<Vec<_>>().join(", "),
        Value::Null => String::new(),
        other => scalar_text(other),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn text_prefixes_nested_lists_with_uppercase_keys() {
        let value =
            json!({"Date": "2026-08-31", "DueTasks": [{"Name": "a", "NextDates": ["2026-08-31"]}]});
        assert_eq!(
            text(&value),
            "2026-08-31\nDUETASKS\ta\nNEXTDATES\t2026-08-31"
        );
    }

    #[test]
    fn table_aligns_wide_characters() {
        let value = json!({"Tasks": [{"Name": "日記", "Order": 1}, {"Name": "ab", "Order": 2}]});
        assert_eq!(
            table(&value),
            "Tasks\n+------+-------+\n| Name | Order |\n+------+-------+\n| 日記 | 1     |\n| ab   | 2     |\n+------+-------+"
        );
    }
}
