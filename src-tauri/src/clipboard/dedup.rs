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

pub(crate) fn compute_semantic_hash(
    content_type: &str,
    text_content: Option<&str>,
    content_hash: &str,
) -> String {
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
    use super::{compute_semantic_hash, normalize_semantic_text};

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
}
