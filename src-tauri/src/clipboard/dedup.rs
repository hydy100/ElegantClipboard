use blake3::Hasher;

const ZERO_WIDTH_CHARS: [char; 5] = ['\u{200B}', '\u{200C}', '\u{200D}', '\u{2060}', '\u{FEFF}'];

fn hash_with_prefix(prefix: &[u8], bytes: &[u8]) -> String {
    let mut hasher = Hasher::new();
    hasher.update(prefix);
    hasher.update(bytes);
    hasher.finalize().to_hex().to_string()
}

/// Normalize user-visible text so semantically equivalent clipboard text
/// (line endings, zero-width chars, trailing spaces/tabs) hashes consistently.
///
/// 单次遍历完成所有标准化操作，避免多次中间 String 分配：
/// 1. \r\n / \r → \n
/// 2. 过滤零宽字符
/// 3. NBSP → 空格
/// 4. 去除每行行尾空白
/// 5. 去除末尾连续空行
pub(crate) fn normalize_semantic_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    // 当前行的内容（含行尾空白），遇到换行时裁剪行尾空白后写入 result
    let mut line_buf = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                // \r\n 或 \r 都视为 \n
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                // 行结束：裁剪行尾空白后写入
                let trimmed = line_buf.trim_end_matches([' ', '\t']);
                result.push_str(trimmed);
                result.push('\n');
                line_buf.clear();
            }
            '\n' => {
                let trimmed = line_buf.trim_end_matches([' ', '\t']);
                result.push_str(trimmed);
                result.push('\n');
                line_buf.clear();
            }
            '\u{00A0}' => line_buf.push(' '),
            c if ZERO_WIDTH_CHARS.contains(&c) => { /* 跳过零宽字符 */ }
            c => line_buf.push(c),
        }
    }

    // 处理最后一行（无换行结尾的情况）
    if !line_buf.is_empty() {
        let trimmed = line_buf.trim_end_matches([' ', '\t']);
        result.push_str(trimmed);
    }

    // 去除末尾连续空行
    let end = result.trim_end_matches('\n').len();
    result.truncate(end);

    result
}

pub(crate) fn semantic_hash_from_text(text: &str) -> Option<String> {
    let normalized = normalize_semantic_text(text);
    if normalized.is_empty() {
        return None;
    }
    Some(hash_with_prefix(b"text:", normalized.as_bytes()))
}

fn starts_with_ignore_ascii_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// Extract the host portion of a URL authority without accepting a path as a
/// host.  Keeping this parser small makes URL classification deterministic and
/// avoids the false positives caused by checking only for a dot.
fn extract_url_host(authority: &str) -> &str {
    let authority = authority.split(['/', '?', '#']).next().unwrap_or(authority);
    if authority.starts_with('[') {
        return authority
            .find(']')
            .map(|end| &authority[..=end])
            .unwrap_or(authority);
    }
    authority.split(':').next().unwrap_or(authority)
}

fn is_valid_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4 && parts.iter().all(|part| part.parse::<u8>().is_ok())
}

fn is_valid_ipv6(host: &str) -> bool {
    if !host.starts_with('[') || !host.ends_with(']') || host.len() <= 2 {
        return false;
    }
    host[1..host.len() - 1]
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() || byte == b':' || byte == b'.')
}

fn is_valid_domain_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 || !host.is_ascii() {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    if labels.iter().any(|label| {
        label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }) {
        return false;
    }
    let tld = labels.last().copied().unwrap_or_default();
    tld.len() >= 2 && tld.bytes().any(|byte| byte.is_ascii_alphabetic())
}

fn is_valid_url_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || is_valid_ipv4(host)
        || is_valid_ipv6(host)
        || is_valid_domain_host(host)
}

/// Return the trimmed URL text when `text` is a supported single-line URL.
pub(crate) fn canonical_url_text(text: &str) -> Option<&str> {
    is_url(text).then(|| text.trim())
}

/// Classify only standalone URLs.  This is deliberately stricter than the
/// frontend linkifier so ordinary prose and malformed links stay text.
pub(crate) fn is_url(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.lines().count() != 1 || trimmed.chars().any(char::is_whitespace) {
        return false;
    }

    let remainder = if starts_with_ignore_ascii_case(trimmed, "http://") {
        trimmed.get(7..)
    } else if starts_with_ignore_ascii_case(trimmed, "https://") {
        trimmed.get(8..)
    } else if starts_with_ignore_ascii_case(trimmed, "ftp://") {
        trimmed.get(6..)
    } else if starts_with_ignore_ascii_case(trimmed, "www.") {
        trimmed.get(4..)
    } else {
        None
    };

    remainder
        .map(extract_url_host)
        .is_some_and(is_valid_url_host)
}

/// Remove volatile RTF metadata generated by Word/Outlook before hashing.
/// The parser works on character boundaries so non-ASCII RTF text cannot
/// trigger a byte-index panic while the ASCII control words are normalized.
pub(crate) fn normalize_rtf_for_hash(rtf_bytes: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(rtf_bytes) else {
        return rtf_bytes.to_vec();
    };

    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < chars.len() {
        let byte_index = chars[i].0;
        if text[byte_index..].starts_with("{\\*\\datastore") {
            let open = (0..=i)
                .rev()
                .find(|&index| chars[index].1 == '{')
                .unwrap_or(i);
            let mut depth = 0usize;
            for j in open..chars.len() {
                match chars[j].1 {
                    '{' => depth += 1,
                    '}' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            i = j + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if depth == 0 {
                continue;
            }
        }

        let remaining = &text[byte_index..];
        if remaining.starts_with("\\mdispDef1") {
            i += "\\mdispDef1".chars().count();
            if i < chars.len() && chars[i].1 == ' ' {
                i += 1;
            }
            continue;
        }

        let command_len = if remaining.starts_with("\\insrsid") {
            Some("\\insrsid".chars().count())
        } else if remaining.starts_with("\\rsid") {
            Some("\\rsid".chars().count())
        } else {
            None
        };
        if let Some(command_len) = command_len {
            let mut digit_start = i + command_len;
            if digit_start < chars.len() && chars[digit_start].1 == ' ' {
                digit_start += 1;
            }
            let mut end = digit_start;
            while end < chars.len() && chars[end].1.is_ascii_digit() {
                end += 1;
            }
            if end > digit_start {
                i = end;
                continue;
            }
        }

        out.push(chars[i].1);
        i += 1;
    }
    out.into_bytes()
}

pub(crate) fn compute_semantic_hash(
    content_type: &str,
    text_content: Option<&str>,
    content_hash: &str,
) -> String {
    if content_type.eq_ignore_ascii_case("url") {
        return content_hash.to_string();
    }
    let is_text_like = content_type.eq_ignore_ascii_case("text")
        || content_type.eq_ignore_ascii_case("html")
        || content_type.eq_ignore_ascii_case("rtf");
    if is_text_like
        && let Some(text) = text_content
        && let Some(hash) = semantic_hash_from_text(text)
    {
        return hash;
    }
    content_hash.to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_url_text, compute_semantic_hash, is_url, normalize_rtf_for_hash,
        normalize_semantic_text,
    };

    #[test]
    fn normalize_text_removes_invisible_chars_and_trailing_whitespace() {
        let input = "A\u{200B}\u{00A0}B\t  \r\nline 2\t\n\n";
        let normalized = normalize_semantic_text(input);
        assert_eq!(normalized, "A B\nline 2");
    }

    #[test]
    fn compute_semantic_hash_accepts_uppercase_content_type() {
        let text_hash = compute_semantic_hash("TEXT", Some("hello"), "fallback");
        let html_hash = compute_semantic_hash("HTML", Some("hello"), "fallback");
        let rtf_hash = compute_semantic_hash("RTF", Some("hello"), "fallback");

        assert_eq!(text_hash, html_hash);
        assert_eq!(text_hash, rtf_hash);
        assert_ne!(text_hash, "fallback");
    }

    #[test]
    fn url_classifier_accepts_domains_addresses_and_localhost() {
        assert!(is_url("https://example.com/path?q=1"));
        assert!(is_url("HTTP://127.0.0.1:8080"));
        assert!(is_url("ftp://[2001:db8::1]/file"));
        assert!(is_url("www.example.com/page"));
        assert!(is_url("http://localhost:3000"));
        assert_eq!(canonical_url_text("  https://example.com  "), Some("https://example.com"));
    }

    #[test]
    fn url_classifier_rejects_prose_and_malformed_hosts() {
        assert!(!is_url("visit https://example.com"));
        assert!(!is_url("https://x.y"));
        assert!(!is_url("https://example.com\nnext"));
        assert!(!is_url("www.这不是网址.测试"));
        assert!(!is_url("http://999.1.1.1"));
    }

    #[test]
    fn rtf_hash_normalization_removes_volatile_fields_and_keeps_unicode_safe() {
        let first = "{\\rtf1\\ansi hello\\rsid12345{\\*\\datastore{nested}}\\mdispDef1界}";
        let second = "{\\rtf1\\ansi hello\\rsid99999\\mdispDef1界}";
        assert_eq!(
            normalize_rtf_for_hash(first.as_bytes()),
            normalize_rtf_for_hash(second.as_bytes())
        );
    }

    #[test]
    fn invalid_rtf_is_hashed_without_transformation() {
        let invalid = [0xff, 0xfe, 0x00];
        assert_eq!(normalize_rtf_for_hash(&invalid), invalid);
    }
}
