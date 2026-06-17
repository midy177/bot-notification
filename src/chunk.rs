//! Content chunking: split text into ≤N-byte chunks on line boundaries,
//! re-prepending markdown table headers across chunk boundaries.

/// Annotate each line; for table body rows, attach the index of the
/// "header\nseparator\n" string in `headers` so it can be re-prepended on a split.
fn annotate_lines(content: &str) -> (Vec<(&str, Option<usize>)>, Vec<String>) {
    let raw: Vec<&str> = content.split_inclusive('\n').collect();
    let mut out = Vec::with_capacity(raw.len());
    let mut headers: Vec<String> = Vec::new();
    let starts_pipe = |s: &str| s.trim_end_matches('\n').trim_start().starts_with('|');
    let is_sep = |s: &str| {
        let t = s.trim_end_matches('\n').trim_start();
        t.starts_with('|') && t.contains("---")
    };

    let mut i = 0;
    while i < raw.len() {
        if starts_pipe(raw[i]) && i + 1 < raw.len() && is_sep(raw[i + 1]) {
            let combined = format!("{}{}", raw[i], raw[i + 1]);
            let id = headers.len();
            headers.push(combined);
            out.push((raw[i], None));
            out.push((raw[i + 1], None));
            i += 2;
            while i < raw.len() && starts_pipe(raw[i]) {
                out.push((raw[i], Some(id)));
                i += 1;
            }
        } else {
            out.push((raw[i], None));
            i += 1;
        }
    }
    (out, headers)
}

/// Split content into chunks <= max_bytes, preferring line boundaries.
/// If the split lands inside a markdown table, the table header+separator
/// is automatically re-prepended to the continuation chunk.
pub(crate) fn split_content(content: &str, max_bytes: usize) -> Vec<String> {
    let (lines, headers) = annotate_lines(content);
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();

    let start_continuation = |current: &mut String, table_id: Option<usize>| {
        if let Some(id) = table_id {
            current.push_str(&headers[id]);
        }
    };

    for (line, table_id) in &lines {
        if line.len() > max_bytes {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let pieces = hard_split(line, max_bytes);
            let last_idx = pieces.len() - 1;
            for (i, piece) in pieces.into_iter().enumerate() {
                if i == last_idx {
                    current = piece;
                } else {
                    chunks.push(piece);
                }
            }
            continue;
        }

        if current.len() + line.len() > max_bytes {
            chunks.push(std::mem::take(&mut current));
            start_continuation(&mut current, *table_id);
        }
        current.push_str(line);
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

fn hard_split(s: &str, max_bytes: usize) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        let mut end = (start + max_bytes).min(bytes.len());
        while end < bytes.len() && (bytes[end] & 0b1100_0000) == 0b1000_0000 {
            end -= 1;
        }
        out.push(s[start..end].to_string());
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_split_when_small() {
        let v = split_content("hello", 4096);
        assert_eq!(v, vec!["hello".to_string()]);
    }

    #[test]
    fn split_on_line_boundary() {
        let line = "a".repeat(100) + "\n";
        let content = line.repeat(50);
        let chunks = split_content(&content, 500);
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.len() <= 500);
        }
    }

    #[test]
    fn hard_split_long_line() {
        let content = "x".repeat(10_000);
        let chunks = split_content(&content, 4096);
        assert!(chunks.iter().all(|c| c.len() <= 4096));
        assert_eq!(chunks.concat(), content);
    }

    #[test]
    fn utf8_boundary_safe() {
        let content = "中".repeat(2000);
        let chunks = split_content(&content, 4096);
        for c in &chunks {
            assert!(c.len() <= 4096);
            assert!(std::str::from_utf8(c.as_bytes()).is_ok());
        }
    }

    #[test]
    fn table_header_re_prepended_on_split() {
        let mut content = String::from("preamble\n");
        content.push_str(&"x".repeat(300));
        content.push('\n');
        content.push_str("| 姓名 | 尺寸 | 地址 |\n");
        content.push_str("| :--- | :--: | ---: |\n");
        for i in 0..40 {
            content.push_str(&format!("| name{} | L | city{} |\n", i, i));
        }
        let chunks = split_content(&content, 400);
        assert!(chunks.len() > 1);
        // every continuation chunk should begin with the table header line
        for c in &chunks[1..] {
            assert!(
                c.starts_with("| 姓名 | 尺寸 | 地址 |\n| :--- | :--: | ---: |\n"),
                "chunk did not start with table header:\n{}",
                c
            );
        }
        // first chunk must NOT have the header re-prepended (header appears once)
        assert_eq!(chunks[0].matches("| 姓名 | 尺寸 | 地址 |").count(), 1);
    }
}
