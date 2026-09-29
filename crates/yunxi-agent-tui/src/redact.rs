//! 终端里显示工具参数/输出前的**脱敏**。
//!
//! 设计原则来自 Miyu 的 `render/tool_display.rs` 模块注释（本仓库
//! `references/miyu-agent/`）：
//!
//! > `redact_sensitive_inline` / `redact_bearer_token` 是必需的：工具参数里可能
//! > 带 token 或密钥，而终端内容会被截图、会进日志。
//!
//! 照搬的是这条**理由**，不是它的实现 —— 那份实现在快照里已经不存在了（只在
//! 注释里被提到），所以这里按同样的原则重写。
//!
//! **保守优先**：只掩掉**形态明确**的密钥，不去猜。过度脱敏会把工具输出变得
//! 没用（用户点开详情就是想看命令到底跑了什么），而漏掉一个密钥的代价更大 ——
//! 所以拿不准的形态宁可漏，形态明确的必须掩。

/// 掩掉之后留下的字符数：够认出「这是同一个密钥」，不够还原它。
const KEEP_PREFIX: usize = 4;

/// 把一段文本里形态明确的密钥换成掩码。
pub(crate) fn redact_secrets(text: &str) -> String {
    let mut out = redact_quoted_assignments(text);
    out = redact_bearer(&out);
    out = redact_prefixed_tokens(&out);
    out
}

/// 掩码：留前几个字符，其余替换掉。
fn mask(secret: &str) -> String {
    let visible: String = secret.chars().take(KEEP_PREFIX).collect();
    if visible.chars().count() < secret.chars().count() {
        format!("{visible}…<已脱敏 {} 字符>", secret.chars().count())
    } else {
        // 短到不值得掩 —— 掩了反而看不出是什么。
        secret.to_string()
    }
}

/// `"api_key": "sk-..."` / `"password": "..."` 这类 JSON / TOML 赋值。
///
/// 只认**键名明确**是密钥的那几种（`api_key` / `apikey` / `token` / `password` /
/// `secret` / `authorization` / `access_token` / `refresh_token`），不碰通用字段。
fn redact_quoted_assignments(text: &str) -> String {
    const SENSITIVE_KEYS: [&str; 9] = [
        "api_key",
        "apikey",
        "api-key",
        "token",
        "access_token",
        "refresh_token",
        "password",
        "passwd",
        "secret",
    ];
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        // 找一个键：`"key"` 或 `key`
        let rest = &text[index..];
        let Some((key, key_span)) = scan_key(rest) else {
            out.push_str(&text[index..index + next_char_len(text, index)]);
            index += next_char_len(text, index);
            continue;
        };
        let lowered = key.to_ascii_lowercase();
        if !SENSITIVE_KEYS.contains(&lowered.as_str()) {
            out.push_str(&text[index..index + key_span]);
            index += key_span;
            continue;
        }
        // 键之后必须紧跟 `:` 或 `=`（允许空白），才是赋值。
        let after_key = index + key_span;
        let separator = text[after_key..]
            .char_indices()
            .find(|(_, ch)| !ch.is_whitespace());
        let Some((sep_offset, sep_char)) = separator else {
            out.push_str(&text[index..]);
            break;
        };
        if sep_char != ':' && sep_char != '=' {
            out.push_str(&text[index..after_key]);
            index = after_key;
            continue;
        }
        let value_start = after_key + sep_offset + sep_char.len_utf8();
        let (value, quoted) = scan_value(&text[value_start..]);
        out.push_str(&text[index..value_start]);
        if value.is_empty() {
            index = value_start;
            continue;
        }
        out.push_str(&mask(&value));
        index = value_start + value.len() + if quoted { 1 } else { 0 };
        if quoted {
            // 补上收尾引号（`scan_value` 只吃到了内容）
            out.push_str(&text[value_start + value.len()..index]);
        }
    }
    out
}

/// `Bearer <token>`（HTTP 头里最常见的那种）。
fn redact_bearer(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(position) = rest.find("Bearer ") {
        let after = position + "Bearer ".len();
        out.push_str(&rest[..after]);
        let token: String = rest[after..]
            .chars()
            .take_while(|ch| !ch.is_whitespace() && *ch != '"' && *ch != '\'')
            .collect();
        if token.is_empty() {
            rest = &rest[after..];
            continue;
        }
        out.push_str(&mask(&token));
        rest = &rest[after + token.len()..];
    }
    out.push_str(rest);
    out
}

/// 有明确前缀的密钥：`sk-...` / `ghp_...` / `xoxb-...` 等。
fn redact_prefixed_tokens(text: &str) -> String {
    const PREFIXES: [&str; 8] = [
        "sk-",
        "sk_",
        "ghp_",
        "gho_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "glpat-",
    ];
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let found = PREFIXES
            .iter()
            .filter_map(|prefix| rest.find(prefix).map(|at| (at, *prefix)))
            .min_by_key(|(at, _)| *at);
        let Some((at, prefix)) = found else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..at]);
        let token: String = rest[at..]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
            .collect();
        if token.len() <= prefix.len() {
            // 只是个前缀，后面没东西 —— 不是密钥
            out.push_str(prefix);
            rest = &rest[at + prefix.len()..];
            continue;
        }
        out.push_str(&mask(&token));
        rest = &rest[at + token.len()..];
    }
    out
}

fn next_char_len(text: &str, index: usize) -> usize {
    text[index..].chars().next().map_or(1, char::len_utf8)
}

/// 扫描一个键：返回 (键名, 含引号的总长度)。
fn scan_key(text: &str) -> Option<(String, usize)> {
    let mut chars = text.char_indices();
    let (_, first) = chars.next()?;
    if first == '"' || first == '\'' {
        let quote = first;
        let mut key = String::new();
        for (offset, ch) in chars {
            if ch == quote {
                return Some((key, offset + ch.len_utf8()));
            }
            if !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-' {
                return None;
            }
            key.push(ch);
        }
        None
    } else {
        let mut key = String::new();
        let mut length = 0;
        for (offset, ch) in chars {
            if !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-' {
                break;
            }
            key.push(ch);
            length = offset + ch.len_utf8();
        }
        (!key.is_empty()).then_some((key, length))
    }
}

/// 扫描一个值：返回 (值, 是否被引号包着)。
fn scan_value(text: &str) -> (String, bool) {
    let mut chars = text.char_indices();
    let Some((_, first)) = chars.next() else {
        return (String::new(), false);
    };
    if first == '"' || first == '\'' {
        let quote = first;
        let value: String = text[first.len_utf8()..]
            .chars()
            .take_while(|ch| *ch != quote)
            .collect();
        return (value, true);
    }
    let value: String = text
        .chars()
        .take_while(|ch| !ch.is_whitespace() && *ch != ',' && *ch != '}' && *ch != ']')
        .collect();
    (value, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_json_style_secrets_but_keeps_the_shape_recognisable() {
        let text = r#"{"api_key":"sk-abcdef1234567890","query":"weather"}"#;
        let redacted = redact_secrets(text);
        assert!(!redacted.contains("abcdef1234567890"), "{redacted}");
        assert!(redacted.contains("query"), "非密钥字段不该被动：{redacted}");
        assert!(redacted.contains("weather"), "值也不该被动：{redacted}");
        assert!(
            redacted.contains("已脱敏"),
            "要能看出这里被脱敏了：{redacted}"
        );
    }

    #[test]
    fn masks_bearer_tokens_and_prefixed_keys() {
        let redacted = redact_secrets("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig");
        assert!(!redacted.contains("payload"), "{redacted}");
        let redacted = redact_secrets("GITHUB_TOKEN=ghp_0123456789abcdefghij");
        assert!(!redacted.contains("0123456789abcdefghij"), "{redacted}");
    }

    #[test]
    fn ordinary_text_is_left_completely_alone() {
        // 过度脱敏比漏掉更常见，也更烦人：用户点开详情就是想看真实内容。
        for text in [
            "cargo build --release",
            r#"{"path":"/home/almost/Projects","limit":20}"#,
            "df -h",
            "shell · 已完成",
            r#"{"name":"using-superpowers"}"#,
            "https://api.deepseek.com/v1",
            "",
        ] {
            assert_eq!(redact_secrets(text), text, "普通文本被改动了：{text}");
        }
    }

    #[test]
    fn a_bare_key_name_is_not_a_secret() {
        // `token` 作为普通词出现（不是赋值）时不能乱掩。
        assert_eq!(
            redact_secrets("token bucket exhausted"),
            "token bucket exhausted"
        );
        assert_eq!(redact_secrets("sk-"), "sk-", "光是个前缀不该当成密钥");
    }

    #[test]
    fn unicode_around_secrets_survives() {
        // 工具参数里有中文/emoji 是常态，脱敏不能把它们切坏。
        let text = r#"{"api_key":"sk-secretvalue123456","note":"中文说明 👩‍💻"}"#;
        let redacted = redact_secrets(text);
        assert!(!redacted.contains("secretvalue123456"), "{redacted}");
        assert!(redacted.contains("中文说明"), "{redacted}");
        assert!(redacted.contains("👩‍💻"), "{redacted}");
    }
}
