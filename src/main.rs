//! bot-notification — send markdown content to a WeCom or Feishu (Lark) bot.

mod chunk;
mod content;
mod feishu;
mod webhook;

use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, ValueEnum};

use chunk::split_content;
use content::{git_header, translate};
use feishu::{pack_feishu_cards, parse_blocks};
use webhook::{build_feishu_payload, build_url, build_wechat_payload, feishu_sign, is_success};

/// Per-chunk content size limit for WeCom markdown_v2.
const WECHAT_MAX_BYTES: usize = 4096;
/// Feishu caps the whole request body at 20 KB; 18000 leaves headroom for the
/// card JSON envelope and the optional timestamp/sign fields.
const FEISHU_MAX_BYTES: usize = 18000;

#[derive(Copy, Clone, Debug, ValueEnum)]
pub(crate) enum Platform {
    Wechat,
    Feishu,
}

#[derive(Parser, Debug)]
#[command(about = "Send markdown messages to a WeCom or Feishu (Lark) bot webhook")]
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

    /// Target platform: wechat (WeCom) or feishu (Lark).
    #[arg(short = 'P', long, value_enum, default_value_t = Platform::Wechat)]
    platform: Platform,

    /// Feishu signing secret (only effective with --platform feishu).
    #[arg(long)]
    secret: Option<String>,
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

    let url = build_url(args.platform, &args.key);

    // Feishu signing: compute timestamp + sign once and reuse for every card.
    let auth = match (args.platform, args.secret.as_deref()) {
        (Platform::Feishu, Some(secret)) => {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| format!("system clock: {}", e))?
                .as_secs();
            Some((ts.to_string(), feishu_sign(secret, ts)))
        }
        (Platform::Wechat, Some(_)) => {
            eprintln!("warning: --secret is ignored for wechat platform");
            None
        }
        _ => None,
    };

    // Build the list of request bodies. WeCom sends one markdown_v2 per text
    // chunk; Feishu packs parsed Text/Table blocks into interactive cards
    // (tables become real `table` elements, ≤5 per card, ≤20KB each).
    let payloads: Vec<serde_json::Value> = match args.platform {
        Platform::Wechat => split_content(&content, WECHAT_MAX_BYTES)
            .into_iter()
            .map(|chunk| build_wechat_payload(&chunk))
            .collect(),
        Platform::Feishu => {
            let blocks = parse_blocks(&content);
            let auth_ref = auth.as_ref().map(|(t, s)| (t.as_str(), s.as_str()));
            pack_feishu_cards(&blocks, FEISHU_MAX_BYTES)
                .into_iter()
                .map(|elements| build_feishu_payload(&elements, auth_ref))
                .collect()
        }
    };
    let total = payloads.len();

    for (i, payload) in payloads.iter().enumerate() {
        let resp: serde_json::Value = ureq::post(&url).send_json(&payload)?.into_json()?;
        if !is_success(args.platform, &resp) {
            eprintln!("[{}/{}] failed: {}", i + 1, total, resp);
            std::process::exit(1);
        }
        let bytes = serde_json::to_string(payload).map(|s| s.len()).unwrap_or(0);
        println!("[{}/{}] sent ({} bytes)", i + 1, total, bytes);
    }

    Ok(())
}
