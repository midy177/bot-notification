//! Input content transforms: osv-scanner English→Chinese translation and the
//! git-repo summary header.

use std::path::Path;
use std::process::Command;

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
pub(crate) fn git_header(repo: &Path) -> Result<String, String> {
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
pub(crate) fn translate(input: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

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
