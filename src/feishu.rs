//! Feishu card assembly: parse markdown content into Text/Table blocks and
//! pack them into interactive cards (markdown tables become real `table`
//! elements, ≤5 per card, ≤20KB each).

use serde_json::{json, Value};

use crate::chunk::split_content;

/// A parsed piece of input: free-form text (→ markdown element) or a markdown
/// table (→ Feishu table element).
pub(crate) enum Block {
    Text(String),
    Table { columns: Vec<String>, rows: Vec<Vec<String>> },
}

/// Does this line begin a markdown table row?
fn row_is_pipe(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// Is this line a table separator like `| --- | :--: |`?
fn row_is_sep(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.contains("---")
}

/// Split a `| a | b | c |` row into trimmed cells.
fn parse_table_row(line: &str) -> Vec<String> {
    let s = line.trim().trim_start_matches('|').trim_end_matches('|');
    s.split('|').map(|c| c.trim().to_string()).collect()
}

/// Parse content into an ordered list of Text / Table blocks.
pub(crate) fn parse_blocks(content: &str) -> Vec<Block> {
    let lines: Vec<&str> = content.lines().collect();
    let mut blocks: Vec<Block> = Vec::new();
    let mut text = String::new();
    let mut i = 0;

    while i < lines.len() {
        if row_is_pipe(lines[i]) && i + 1 < lines.len() && row_is_sep(lines[i + 1]) {
            if !text.is_empty() {
                blocks.push(Block::Text(std::mem::take(&mut text)));
            }
            let columns = parse_table_row(lines[i]);
            i += 2;
            let mut rows: Vec<Vec<String>> = Vec::new();
            while i < lines.len() && row_is_pipe(lines[i]) {
                rows.push(parse_table_row(lines[i]));
                i += 1;
            }
            blocks.push(Block::Table { columns, rows });
        } else {
            text.push_str(lines[i]);
            text.push('\n');
            i += 1;
        }
    }
    if !text.is_empty() {
        blocks.push(Block::Text(text));
    }
    blocks
}

/// Serialized byte length of a JSON value.
fn json_len(v: &Value) -> usize {
    serde_json::to_string(v).map(|s| s.len()).unwrap_or(0)
}

/// Build one row object `{"c0":"..","c1":".."}` aligned to the column count.
fn row_object(columns: &[String], row: &[String]) -> Value {
    let mut obj = serde_json::Map::new();
    for (i, _) in columns.iter().enumerate() {
        let cell = row.get(i).map(|s| s.as_str()).unwrap_or("");
        obj.insert(format!("c{}", i), json!(cell));
    }
    json!(obj)
}

/// Build a Feishu `table` element from column names and a slice of rows.
fn table_element(columns: &[String], rows: &[Vec<String>]) -> Value {
    let col_defs: Vec<Value> = columns
        .iter()
        .enumerate()
        .map(|(i, name)| {
            json!({ "name": format!("c{}", i), "display_name": name, "data_type": "text" })
        })
        .collect();
    let row_objs: Vec<Value> = rows.iter().map(|r| row_object(columns, r)).collect();
    json!({ "tag": "table", "page_size": 10, "columns": col_defs, "rows": row_objs })
}

/// Split a single large table into one or more `table` elements, each within
/// `budget` serialized bytes (column defs are repeated on each element).
fn split_table_elements(columns: &[String], rows: &[Vec<String>], budget: usize) -> Vec<Value> {
    let header_bytes = json_len(&table_element(columns, &[]));
    let mut out: Vec<Value> = Vec::new();
    let mut batch: Vec<Vec<String>> = Vec::new();
    let mut bytes = header_bytes;

    for row in rows {
        let row_bytes = json_len(&row_object(columns, row)) + 1; // +1 for the array comma
        if !batch.is_empty() && bytes + row_bytes > budget {
            out.push(table_element(columns, &batch));
            batch.clear();
            bytes = header_bytes;
        }
        batch.push(row.clone());
        bytes += row_bytes;
    }
    if !batch.is_empty() {
        out.push(table_element(columns, &batch));
    } else if out.is_empty() {
        out.push(table_element(columns, &[])); // keep an empty table to preserve the header
    }
    out
}

/// Greedily pack blocks into one or more cards. Each card's elements serialize
/// to ≤ `budget` bytes and contain at most 5 table elements (Feishu limit).
pub(crate) fn pack_feishu_cards(blocks: &[Block], budget: usize) -> Vec<Vec<Value>> {
    const OVERHEAD: usize = 128; // {"msg_type":"interactive","card":{"elements":[...]}} + optional sign/timestamp
    let elem_budget = budget.saturating_sub(OVERHEAD);
    let text_budget = elem_budget.saturating_sub(64); // room for the markdown element wrapper

    let mut cards: Vec<Vec<Value>> = Vec::new();
    let mut cur: Vec<Value> = Vec::new();
    let mut cur_bytes = 0usize;
    let mut table_count = 0usize;

    for block in blocks {
        let elements: Vec<Value> = match block {
            Block::Text(s) => split_content(s, text_budget)
                .into_iter()
                .map(|piece| json!({ "tag": "markdown", "content": piece }))
                .collect(),
            Block::Table { columns, rows } => split_table_elements(columns, rows, elem_budget),
        };

        for elem in elements {
            let elem_bytes = json_len(&elem);
            let is_table = elem.get("tag").and_then(|t| t.as_str()) == Some("table");
            let needs_new_card = !cur.is_empty()
                && (cur_bytes + elem_bytes > elem_budget || (is_table && table_count >= 5));
            if needs_new_card {
                cards.push(std::mem::take(&mut cur));
                cur_bytes = 0;
                table_count = 0;
            }
            cur.push(elem);
            cur_bytes += elem_bytes;
            if is_table {
                table_count += 1;
            }
        }
    }
    if !cur.is_empty() {
        cards.push(cur);
    }
    if cards.is_empty() {
        cards.push(vec![json!({ "tag": "markdown", "content": "" })]);
    }
    cards
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_table_row_trims_cells() {
        assert_eq!(parse_table_row("| a | b | c |"), vec!["a", "b", "c"]);
        assert_eq!(parse_table_row("|  a | b |\n"), vec!["a", "b"]);
    }

    #[test]
    fn parse_blocks_text_then_table_then_text() {
        let content = "intro\n| A | B |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |\ntail\n";
        let blocks = parse_blocks(content);
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[0], Block::Text(_)));
        match &blocks[1] {
            Block::Table { columns, rows } => {
                assert_eq!(columns, &vec!["A".to_string(), "B".to_string()]);
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0], vec!["1".to_string(), "2".to_string()]);
            }
            _ => panic!("expected table"),
        }
        assert!(matches!(&blocks[2], Block::Text(_)));
    }

    #[test]
    fn pack_one_small_table_one_card() {
        let blocks = vec![Block::Table {
            columns: vec!["A".into(), "B".into()],
            rows: vec![vec!["1".into(), "2".into()]],
        }];
        let cards = pack_feishu_cards(&blocks, 18000);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].len(), 1);
        assert_eq!(cards[0][0]["tag"], "table");
    }

    #[test]
    fn pack_splits_at_five_tables() {
        let mut blocks = Vec::new();
        for _ in 0..6 {
            blocks.push(Block::Table {
                columns: vec!["A".into()],
                rows: vec![vec!["x".into()]],
            });
        }
        // Big budget so only the 5-tables-per-card rule forces a new card.
        let cards = pack_feishu_cards(&blocks, 200_000);
        assert!(cards.len() >= 2);
        for card in &cards {
            let tables = card.iter().filter(|e| e["tag"].as_str() == Some("table")).count();
            assert!(tables <= 5);
        }
    }

    #[test]
    fn pack_empty_input_yields_one_empty_card() {
        let cards = pack_feishu_cards(&[], 18000);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0][0]["tag"], "markdown");
    }
}
