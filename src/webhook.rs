//! Platform webhook layer: build request URLs/bodies, compute the Feishu
//! signature, and decide success per platform.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::Platform;

const WEBHOOK_URL: &str = "https://qyapi.weixin.qq.com/cgi-bin/webhook/send";
const FEISHU_HOOK_BASE: &str = "https://open.feishu.cn/open-apis/bot/v2/hook";

/// Build the webhook URL for the given platform from the user-supplied key.
/// Feishu accepts either a full webhook URL or a bare hook token.
pub(crate) fn build_url(platform: Platform, key: &str) -> String {
    match platform {
        Platform::Wechat => format!("{}?key={}", WEBHOOK_URL, key),
        Platform::Feishu => {
            if key.starts_with("http://") || key.starts_with("https://") {
                key.to_string()
            } else {
                format!("{}/{}", FEISHU_HOOK_BASE, key)
            }
        }
    }
}

/// Construct the WeCom request body for a single text chunk.
pub(crate) fn build_wechat_payload(chunk: &str) -> Value {
    json!({ "msgtype": "markdown_v2", "markdown_v2": { "content": chunk } })
}

/// Construct the Feishu interactive-card request body for a list of elements.
/// `auth` carries an optional (timestamp, sign) pair for signed bots.
pub(crate) fn build_feishu_payload(elements: &[Value], auth: Option<(&str, &str)>) -> Value {
    let mut payload = json!({ "msg_type": "interactive", "card": { "elements": elements } });
    if let Some((timestamp, sign)) = auth {
        payload["timestamp"] = json!(timestamp);
        payload["sign"] = json!(sign);
    }
    payload
}

/// Feishu signature: Base64(HmacSHA256(key = "<timestamp>\n<secret>", msg = "")).
pub(crate) fn feishu_sign(secret: &str, timestamp: u64) -> String {
    let string_to_sign = format!("{}\n{}", timestamp, secret);
    let mut mac = Hmac::<Sha256>::new_from_slice(string_to_sign.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(b"");
    STANDARD.encode(mac.finalize().into_bytes())
}

/// Whether the platform response indicates success.
pub(crate) fn is_success(platform: Platform, resp: &Value) -> bool {
    let field = match platform {
        Platform::Wechat => "errcode",
        Platform::Feishu => "code",
    };
    resp.get(field).and_then(|v| v.as_i64()) == Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Platform;

    #[test]
    fn wechat_url_keeps_query() {
        assert_eq!(
            build_url(Platform::Wechat, "K-abc"),
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=K-abc"
        );
    }

    #[test]
    fn feishu_url_full_or_token() {
        assert_eq!(
            build_url(Platform::Feishu, "https://open.feishu.cn/open-apis/bot/v2/hook/abc"),
            "https://open.feishu.cn/open-apis/bot/v2/hook/abc"
        );
        assert_eq!(
            build_url(Platform::Feishu, "abc-123"),
            "https://open.feishu.cn/open-apis/bot/v2/hook/abc-123"
        );
    }

    #[test]
    fn wechat_payload_shape() {
        let p = build_wechat_payload("hi");
        assert_eq!(p["msgtype"], "markdown_v2");
        assert_eq!(p["markdown_v2"]["content"], "hi");
    }

    #[test]
    fn feishu_payload_without_sign() {
        let p = build_feishu_payload(&[json!({ "tag": "markdown", "content": "hi" })], None);
        assert_eq!(p["msg_type"], "interactive");
        assert_eq!(p["card"]["elements"][0]["tag"], "markdown");
        assert_eq!(p["card"]["elements"][0]["content"], "hi");
        assert!(p.get("sign").is_none());
        assert!(p.get("timestamp").is_none());
    }

    #[test]
    fn feishu_payload_with_sign() {
        let p = build_feishu_payload(
            &[json!({ "tag": "markdown", "content": "hi" })],
            Some(("1599360473", "sig")),
        );
        assert_eq!(p["timestamp"], "1599360473");
        assert_eq!(p["sign"], "sig");
        assert_eq!(p["card"]["elements"][0]["content"], "hi");
    }

    #[test]
    fn is_success_by_platform() {
        assert!(is_success(Platform::Wechat, &json!({"errcode": 0})));
        assert!(!is_success(Platform::Wechat, &json!({"errcode": 9})));
        assert!(is_success(
            Platform::Feishu,
            &json!({"code": 0, "msg": "success"})
        ));
        assert!(!is_success(Platform::Feishu, &json!({"code": 19021})));
    }

    #[test]
    fn feishu_sign_matches_reference() {
        // Reference computed via Python:
        //   base64.b64encode(hmac.new(b"1599360473\ndemo", b"", hashlib.sha256).digest())
        assert_eq!(feishu_sign("demo", 1599360473), "l1N0gAcBjdwBvGm1xMjOF0XSyaLRpR7tuO5dHfhAYc8=");
    }

    #[test]
    fn feishu_sign_is_deterministic_and_sensitive() {
        let a = feishu_sign("demo", 1599360473);
        assert_eq!(a, feishu_sign("demo", 1599360473));
        assert_ne!(a, feishu_sign("demo", 1599360474));
        assert_ne!(a, feishu_sign("other", 1599360473));
    }
}
