use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;
use serde::Serialize;

const MAX_BYTES: usize = 4096;
const WEBHOOK_URL: &str = "https://qyapi.weixin.qq.com/cgi-bin/webhook/send";

#[derive(Parser, Debug)]
#[command(about = "Send markdown_v2 messages to WeCom (Enterprise WeChat) bot webhook")]
struct Args {
    /// WeCom bot webhook key
    #[arg(short, long)]
    key: String,

    /// Path to markdown file; if omitted, read from stdin
    #[arg(short, long)]
    file: Option<PathBuf>,

    /// Translate known English summary lines (osv-scanner style) to Chinese.
    #[arg(short, long, default_value_t = true)]
    translate: bool,

    /// Path to a git repository; its branch / commit / author info will be
    /// prepended to the first message.
    #[arg(short, long)]
    git: Option<PathBuf>,
}

#[derive(Serialize)]
struct Payload<'a> {
    msgtype: &'a str,
    markdown_v2: MarkdownV2<'a>,
}

#[derive(Serialize)]
struct MarkdownV2<'a> {
    content: &'a str,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let content = match args.file {
        Some(path) => {
            fs::read_to_string(&path).map_err(|e| format!("read {}: {}", path.display(), e))?
        }
        None => {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf)?;
            buf
        }
    };

    let content = if args.translate {
        translate(&content)
    } else {
        content
    };

    let content = match args.git.as_deref() {
        Some(repo) => match git_header(repo) {
            Ok(header) => format!("{}\n{}", header, content),
            Err(e) => {
                eprintln!("warning: skip git header: {}", e);
                content
            }
        },
        None => content,
    };

    let chunks = split_content(&content, MAX_BYTES);
    let total = chunks.len();
    let url = format!("{}?key={}", WEBHOOK_URL, args.key);

    for (i, chunk) in chunks.iter().enumerate() {
        let payload = Payload {
            msgtype: "markdown_v2",
            markdown_v2: MarkdownV2 { content: chunk },
        };
        let resp: serde_json::Value = ureq::post(&url).send_json(&payload)?.into_json()?;
        let errcode = resp.get("errcode").and_then(|v| v.as_i64()).unwrap_or(-1);
        if errcode != 0 {
            eprintln!("[{}/{}] failed: {}", i + 1, total, resp);
            std::process::exit(1);
        }
        println!("[{}/{}] sent ({} bytes)", i + 1, total, chunk.len());
    }

    Ok(())
}

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
fn split_content(content: &str, max_bytes: usize) -> Vec<String> {
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

use regex::Regex;

/// Truncate `s` to at most `max` chars, appending "..." if it was longer.
/// The "..." counts toward the limit (so result has at most `max` chars).
fn truncate_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(3);
    let mut out: String = s.chars().take(keep).collect();
    out.push_str("...");
    out
}

fn git_run(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("spawn git: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Build a markdown header summarizing the repo state.
fn git_header(repo: &Path) -> Result<String, String> {
    if !repo.exists() {
        return Err(format!("path not found: {}", repo.display()));
    }
    let branch = git_run(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let short = git_run(repo, &["rev-parse", "--short", "HEAD"])?;
    let subject = git_run(repo, &["log", "-1", "--pretty=%s"]).unwrap_or_default();
    let subject = truncate_chars(&subject, 50);
    let author = git_run(repo, &["log", "-1", "--pretty=%an"]).unwrap_or_default();
    let date = git_run(repo, &["log", "-1", "--pretty=%ad", "--date=format:%Y-%m-%d %H:%M:%S"])
        .unwrap_or_default();
    let remote = git_run(repo, &["config", "--get", "remote.origin.url"]).unwrap_or_default();

    let repo_name = if !remote.is_empty() {
        remote
            .trim_end_matches(".git")
            .rsplit(['/', ':'])
            .next()
            .unwrap_or(&remote)
            .to_string()
    } else {
        repo.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| repo.display().to_string())
    };

    let mut lines = vec![format!("**仓库**: {}", repo_name)];
    if !branch.is_empty() && branch != "HEAD" {
        lines.push(format!("**分支**: {}", branch));
    }
    lines.push(format!("**提交**: `{}` {}", short, subject));
    if !author.is_empty() {
        lines.push(format!("**作者**: {}", author));
    }
    if !date.is_empty() {
        lines.push(format!("**时间**: {}", date));
    }
    lines.push("\n---".to_string());
    Ok(lines.join("\n"))
}

/// Translate osv-scanner-style English summary lines to Chinese.
/// Supported patterns:
///   "Total <a> packages affected by <b> known vulnerabilities
///    (<c> Critical, <d> High, <e> Medium, <f> Low, <g> Unknown) from <h> ecosystems."
///   "<n> vulnerabilities can be fixed."
fn translate(input: &str) -> String {
    let total_re = Regex::new(
        r"Total\s+(\d+)\s+packages?\s+affected\s+by\s+(\d+)\s+known\s+vulnerabilit(?:y|ies)\s*\(\s*(\d+)\s+Critical\s*,\s*(\d+)\s+High\s*,\s*(\d+)\s+Medium\s*,\s*(\d+)\s+Low\s*,\s*(\d+)\s+Unknown\s*\)\s+from\s+(\d+)\s+ecosystems?\.",
    )
    .unwrap();
    let fixable_re = Regex::new(r"(\d+)\s+vulnerabilit(?:y|ies)\s+can\s+be\s+fixed\.").unwrap();

    let s = total_re.replace_all(input, |c: &regex::Captures| {
        format!(
            "共有 {} 个软件包受到 {} 个已知漏洞影响（严重 {} 个，高危 {} 个，中危 {} 个，低危 {} 个，未知 {} 个），来自 {} 个生态系统。",
            &c[1], &c[2], &c[3], &c[4], &c[5], &c[6], &c[7], &c[8]
        )
    });
    let s = fixable_re.replace_all(&s, |c: &regex::Captures| {
        format!("{} 个漏洞可以修复。", &c[1])
    });
    s.into_owned()
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

    #[test]
    fn translate_osv_summary() {
        let input = "Total 12 packages affected by 23 known vulnerabilities (0 Critical, 22 High, 1 Medium, 0 Low, 0 Unknown) from 2 ecosystems.\n23 vulnerabilities can be fixed.";
        let out = translate(input);
        assert_eq!(
            out,
            "共有 12 个软件包受到 23 个已知漏洞影响（严重 0 个，高危 22 个，中危 1 个，低危 0 个，未知 0 个），来自 2 个生态系统。\n23 个漏洞可以修复。"
        );
    }

    #[test]
    fn translate_singular_one_vuln() {
        assert_eq!(
            translate("1 vulnerability can be fixed."),
            "1 个漏洞可以修复。"
        );
    }

    #[test]
    fn translate_no_match_unchanged() {
        assert_eq!(translate("hello world"), "hello world");
    }

    #[test]
    fn truncate_chars_works() {
        assert_eq!(truncate_chars("hello", 50), "hello");
        let s: String = "中".repeat(60);
        let t = truncate_chars(&s, 50);
        assert_eq!(t.chars().count(), 50);
        assert!(t.ends_with("..."));
        assert_eq!(truncate_chars(&"a".repeat(50), 50), "a".repeat(50));
        assert_eq!(truncate_chars(&"a".repeat(51), 50), format!("{}...", "a".repeat(47)));
    }
}
