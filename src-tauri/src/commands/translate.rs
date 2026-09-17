use std::sync::Arc;
use tauri::{Emitter, Manager, State};

use super::AppState;
use crate::database;

/// 构建 HTTP 客户端（根据代理配置）
fn build_client(proxy_mode: &str, proxy_url: &str) -> Result<reqwest::blocking::Client, String> {
    let builder = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(30))
        .danger_accept_invalid_certs(true);

    let builder = crate::proxy::apply_proxy(builder, proxy_mode, proxy_url)?;

    builder.build().map_err(|e| format!("创建 HTTP 客户端失败: {}", e))
}

const MICROSOFT_TRANSLATE_URL: &str = "https://edge.microsoft.com/translate/translatetext";
const MICROSOFT_MAX_CHARS_PER_REQUEST: usize = 45_000;
const MICROSOFT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0";

fn microsoft_language_code(language: &str) -> &str {
    match language {
        "auto" => "",
        "zh" | "zh-CN" | "zh-CHS" => "zh-Hans",
        "zh-TW" | "zh-CHT" => "zh-Hant",
        other => other,
    }
}

fn microsoft_translate_url(from: &str, to: &str) -> String {
    format!(
        "{MICROSOFT_TRANSLATE_URL}?from={}&to={}&isEnterpriseClient=false",
        urlencoded(microsoft_language_code(from)),
        urlencoded(microsoft_language_code(to)),
    )
}

fn parse_microsoft_translation(value: &serde_json::Value) -> Result<String, String> {
    value
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["translations"].as_array())
        .and_then(|translations| translations.first())
        .and_then(|translation| translation["text"].as_str())
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "微软翻译响应格式异常或结果为空".to_string())
}

fn translate_microsoft_once(
    client: &reqwest::blocking::Client,
    text: &str,
    from: &str,
    to: &str,
) -> Result<String, String> {
    // Edge 在 2026 年 8 月下线了旧的 /translate/auth token 接口，
    // 当前免费接口直接接受字符串数组，不再需要 Authorization。
    let body = serde_json::json!([text]);
    let resp = client
        .post(microsoft_translate_url(from, to))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("User-Agent", MICROSOFT_USER_AGENT)
        .json(&body)
        .send()
        .map_err(|e| format!("微软翻译请求失败: {}", e))?;
    let status = resp.status();
    let resp_text = resp.text().map_err(|e| format!("读取微软翻译响应失败: {}", e))?;
    if !status.is_success() {
        return Err(format!("微软翻译错误 ({}): {}", status, resp_text));
    }
    let value: serde_json::Value = serde_json::from_str(&resp_text)
        .map_err(|e| format!("解析微软翻译响应失败: {}", e))?;
    parse_microsoft_translation(&value)
}

/// 微软翻译（Edge 当前免鉴权接口，无需 API Key）
fn translate_microsoft(client: &reqwest::blocking::Client, text: &str, from: &str, to: &str) -> Result<String, String> {
    if text.chars().count() <= MICROSOFT_MAX_CHARS_PER_REQUEST {
        return translate_microsoft_once(client, text, from, to);
    }

    // Edge 接口单次约 50,000 字符；按字符边界拆分，避免 UTF-8 字节截断。
    let mut translated = Vec::new();
    let mut chunk = String::new();
    let mut chunk_chars = 0;
    for character in text.chars() {
        chunk.push(character);
        chunk_chars += 1;
        if chunk_chars >= MICROSOFT_MAX_CHARS_PER_REQUEST {
            translated.push(translate_microsoft_once(client, &chunk, from, to)?);
            chunk.clear();
            chunk_chars = 0;
        }
    }
    if !chunk.is_empty() {
        translated.push(translate_microsoft_once(client, &chunk, from, to)?);
    }
    Ok(translated.join(""))
}

/// DeepLX 翻译（自定义接口地址）
fn translate_deeplx(client: &reqwest::blocking::Client, text: &str, from: &str, to: &str, endpoint: &str) -> Result<String, String> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err("请在设置中填写 DeepLX 请求地址".to_string());
    }
    let source_lang = if from == "auto" { "" } else { from };
    let body = serde_json::json!({
        "text": text,
        "source_lang": source_lang,
        "target_lang": to,
    });
    let resp = client
        .post(endpoint)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .map_err(|e| format!("DeepLX 请求失败: {}", e))?;
    let status = resp.status();
    let resp_text = resp.text().map_err(|e| format!("读取响应失败: {}", e))?;
    if !status.is_success() {
        return Err(format!("DeepLX 错误 ({}): {}", status, resp_text));
    }
    let val: serde_json::Value = serde_json::from_str(&resp_text)
        .map_err(|e| format!("解析响应失败: {}", e))?;
    // DeepLX 响应格式: { "code": 200, "data": "translated text", ... }
    if let Some(data) = val["data"].as_str() {
        if !data.is_empty() {
            return Ok(data.to_string());
        }
    }
    // 兼容其他格式
    if let Some(alternatives) = val["alternatives"].as_array() {
        if let Some(first) = alternatives.first().and_then(|v| v.as_str()) {
            return Ok(first.to_string());
        }
    }
    Err(format!("DeepLX 翻译结果异常: {}", resp_text))
}

/// 谷歌翻译（免费接口）
fn translate_google_free(client: &reqwest::blocking::Client, text: &str, from: &str, to: &str) -> Result<String, String> {
    let sl = if from == "auto" { "auto" } else { from };
    let url = format!(
        "https://translate.googleapis.com/translate_a/single?client=gtx&sl={sl}&tl={to}&dt=t&q={q}",
        sl = sl,
        to = to,
        q = urlencoded(text),
    );
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("谷歌翻译请求失败: {}", e))?;
    let status = resp.status();
    let resp_text = resp.text().map_err(|e| format!("读取响应失败: {}", e))?;
    if !status.is_success() {
        return Err(format!("谷歌翻译错误 ({}): {}", status, resp_text));
    }
    let val: serde_json::Value = serde_json::from_str(&resp_text)
        .map_err(|e| format!("解析响应失败: {}", e))?;
    // 响应格式: [[["translated","original",...], ...], ...]
    let mut result = String::new();
    if let Some(sentences) = val[0].as_array() {
        for sentence in sentences {
            if let Some(t) = sentence[0].as_str() {
                result.push_str(t);
            }
        }
    }
    if result.is_empty() {
        Err("翻译结果为空".to_string())
    } else {
        Ok(result)
    }
}

/// 谷歌翻译（API Key 版）
fn translate_google_api(client: &reqwest::blocking::Client, text: &str, from: &str, to: &str, api_key: &str) -> Result<String, String> {
    if api_key.is_empty() {
        return Err("请在设置中填写 Google API Key".to_string());
    }
    let source = if from == "auto" { "" } else { from };
    let url = format!(
        "https://translation.googleapis.com/language/translate/v2?key={key}",
        key = api_key,
    );
    let mut body = serde_json::json!({
        "q": text,
        "target": to,
        "format": "text",
    });
    if !source.is_empty() {
        body["source"] = serde_json::json!(source);
    }
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .map_err(|e| format!("Google API 请求失败: {}", e))?;
    let status = resp.status();
    let resp_text = resp.text().map_err(|e| format!("读取响应失败: {}", e))?;
    if !status.is_success() {
        return Err(format!("Google API 错误 ({}): {}", status, resp_text));
    }
    let val: serde_json::Value = serde_json::from_str(&resp_text)
        .map_err(|e| format!("解析响应失败: {}", e))?;
    val["data"]["translations"][0]["translatedText"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "翻译结果格式异常".to_string())
}

/// 百度翻译
fn translate_baidu(client: &reqwest::blocking::Client, text: &str, from: &str, to: &str, app_id: &str, secret_key: &str) -> Result<String, String> {
    if app_id.is_empty() || secret_key.is_empty() {
        return Err("请在设置中填写百度翻译 APP ID 和密钥".to_string());
    }
    // 语言映射
    let from_baidu = match from {
        "auto" => "auto",
        "zh" => "zh",
        "en" => "en",
        "ja" => "jp",
        "ko" => "kor",
        "fr" => "fra",
        "de" => "de",
        "es" => "spa",
        "pt" => "pt",
        "ru" => "ru",
        "ar" => "ara",
        "it" => "it",
        "th" => "th",
        "vi" => "vie",
        other => other,
    };
    let to_baidu = match to {
        "zh" => "zh",
        "en" => "en",
        "ja" => "jp",
        "ko" => "kor",
        "fr" => "fra",
        "de" => "de",
        "es" => "spa",
        "pt" => "pt",
        "ru" => "ru",
        "ar" => "ara",
        "it" => "it",
        "th" => "th",
        "vi" => "vie",
        other => other,
    };
    let salt = chrono::Utc::now().timestamp_millis().to_string();
    let sign_str = format!("{}{}{}{}", app_id, text, salt, secret_key);
    let sign = format!("{:x}", md5_hash(sign_str.as_bytes()));
    let params = [
        ("q", text),
        ("from", from_baidu),
        ("to", to_baidu),
        ("appid", app_id),
        ("salt", &salt),
        ("sign", &sign),
    ];
    let resp = client
        .post("https://fanyi-api.baidu.com/api/trans/vip/translate")
        .form(&params)
        .send()
        .map_err(|e| format!("百度翻译请求失败: {}", e))?;
    let status = resp.status();
    let resp_text = resp.text().map_err(|e| format!("读取响应失败: {}", e))?;
    if !status.is_success() {
        return Err(format!("百度翻译错误 ({}): {}", status, resp_text));
    }
    let val: serde_json::Value = serde_json::from_str(&resp_text)
        .map_err(|e| format!("解析响应失败: {}", e))?;
    if let Some(err_code) = val["error_code"].as_str() {
        let err_msg = val["error_msg"].as_str().unwrap_or("未知错误");
        return Err(format!("百度翻译错误 ({}): {}", err_code, err_msg));
    }
    let results = val["trans_result"]
        .as_array()
        .ok_or_else(|| "翻译结果格式异常".to_string())?;
    let translated: Vec<&str> = results
        .iter()
        .filter_map(|r| r["dst"].as_str())
        .collect();
    if translated.is_empty() {
        Err("翻译结果为空".to_string())
    } else {
        Ok(translated.join("\n"))
    }
}

/// OpenAI / AI 翻译
fn translate_openai(
    client: &reqwest::blocking::Client,
    text: &str,
    from: &str,
    to: &str,
    endpoint: &str,
    api_key: &str,
    model: &str,
) -> Result<String, String> {
    if api_key.is_empty() {
        return Err("请在设置中填写 API Key".to_string());
    }
    let base = if endpoint.is_empty() { "https://api.openai.com/v1" } else { endpoint.trim_end_matches('/') };
    let url = format!("{}/chat/completions", base);
    let model_id = if model.is_empty() { "gpt-5.2" } else { model };

    let from_desc = if from == "auto" { "auto-detected language" } else { from };
    let system_prompt = format!(
        "You are a professional translator. Translate the following text from {} to {}. Only output the translation, no explanations.",
        from_desc, to,
    );
    let body = serde_json::json!({
        "model": model_id,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": text },
        ],
        "temperature": 0.3,
    });
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&body)
        .send()
        .map_err(|e| format!("AI 翻译请求失败: {}", e))?;
    let status = resp.status();
    let resp_text = resp.text().map_err(|e| format!("读取响应失败: {}", e))?;
    if !status.is_success() {
        return Err(format!("AI 翻译错误 ({}): {}", status, resp_text));
    }
    let val: serde_json::Value = serde_json::from_str(&resp_text)
        .map_err(|e| format!("解析响应失败: {}", e))?;
    val["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "翻译结果格式异常".to_string())
}

/// 简单 URL 编码
fn urlencoded(s: &str) -> String {
    let mut result = String::new();
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

/// 简易 MD5（用于百度翻译签名）
fn md5_hash(data: &[u8]) -> u128 {
    // 标准 MD5 实现
    let mut state: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
    let orig_len_bits = (data.len() as u64) * 8;

    // 填充
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&orig_len_bits.to_le_bytes());

    static S: [u32; 64] = [
        7,12,17,22, 7,12,17,22, 7,12,17,22, 7,12,17,22,
        5, 9,14,20, 5, 9,14,20, 5, 9,14,20, 5, 9,14,20,
        4,11,16,23, 4,11,16,23, 4,11,16,23, 4,11,16,23,
        6,10,15,21, 6,10,15,21, 6,10,15,21, 6,10,15,21,
    ];
    static K: [u32; 64] = [
        0xd76aa478,0xe8c7b756,0x242070db,0xc1bdceee,0xf57c0faf,0x4787c62a,0xa8304613,0xfd469501,
        0x698098d8,0x8b44f7af,0xffff5bb1,0x895cd7be,0x6b901122,0xfd987193,0xa679438e,0x49b40821,
        0xf61e2562,0xc040b340,0x265e5a51,0xe9b6c7aa,0xd62f105d,0x02441453,0xd8a1e681,0xe7d3fbc8,
        0x21e1cde6,0xc33707d6,0xf4d50d87,0x455a14ed,0xa9e3e905,0xfcefa3f8,0x676f02d9,0x8d2a4c8a,
        0xfffa3942,0x8771f681,0x6d9d6122,0xfde5380c,0xa4beea44,0x4bdecfa9,0xf6bb4b60,0xbebfbc70,
        0x289b7ec6,0xeaa127fa,0xd4ef3085,0x04881d05,0xd9d4d039,0xe6db99e5,0x1fa27cf8,0xc4ac5665,
        0xf4292244,0x432aff97,0xab9423a7,0xfc93a039,0x655b59c3,0x8f0ccc92,0xffeff47d,0x85845dd1,
        0x6fa87e4f,0xfe2ce6e0,0xa3014314,0x4e0811a1,0xf7537e82,0xbd3af235,0x2ad7d2bb,0xeb86d391,
    ];

    for chunk in msg.chunks(64) {
        let mut m = [0u32; 16];
        for (i, c) in chunk.chunks(4).enumerate() {
            m[i] = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        }
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                (a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]))
                    .rotate_left(S[i]),
            );
            a = temp;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }

    let mut digest = [0u8; 16];
    for (i, &val) in state.iter().enumerate() {
        digest[i * 4..i * 4 + 4].copy_from_slice(&val.to_le_bytes());
    }
    u128::from_be_bytes(digest.try_into().unwrap())
}

/// 翻译文本（Tauri 命令）
#[tauri::command]
pub async fn translate_text(
    text: String,
    from: String,
    to: String,
    provider: String,
    proxy_mode: String,
    proxy_url: String,
    deeplx_endpoint: Option<String>,
    google_api_key: Option<String>,
    baidu_app_id: Option<String>,
    baidu_secret_key: Option<String>,
    openai_endpoint: Option<String>,
    openai_api_key: Option<String>,
    openai_model: Option<String>,
) -> Result<String, String> {
    // 在线程池中执行阻塞 HTTP 请求
    tokio::task::spawn_blocking(move || {
        let client = build_client(&proxy_mode, &proxy_url)?;
        match provider.as_str() {
            "microsoft" => translate_microsoft(&client, &text, &from, &to),
            "deeplx" => translate_deeplx(
                &client, &text, &from, &to,
                &deeplx_endpoint.unwrap_or_default(),
            ),
            "google_free" => translate_google_free(&client, &text, &from, &to),
            "google_api" => translate_google_api(
                &client, &text, &from, &to,
                &google_api_key.unwrap_or_default(),
            ),
            "baidu" => translate_baidu(
                &client, &text, &from, &to,
                &baidu_app_id.unwrap_or_default(),
                &baidu_secret_key.unwrap_or_default(),
            ),
            "openai" => translate_openai(
                &client, &text, &from, &to,
                &openai_endpoint.unwrap_or_default(),
                &openai_api_key.unwrap_or_default(),
                &openai_model.unwrap_or_default(),
            ),
            other => Err(format!("不支持的翻译提供者: {}", other)),
        }
    })
    .await
    .map_err(|e| format!("翻译任务失败: {}", e))?
}

/// 将文本写入系统剪贴板（用于复制翻译结果）
/// record: true = 不暂停监控器，剪贴板变更会被记录为新条目
///         false = 暂停监控器后写入，不记录
#[tauri::command]
pub async fn write_text_to_clipboard(
    state: State<'_, Arc<AppState>>,
    text: String,
    record: Option<bool>,
) -> Result<(), String> {
    let state = state.inner().clone();
    super::run_blocking("write_text_to_clipboard", move || {
        let write_fn = || {
            let mut clipboard = arboard::Clipboard::new()
                .map_err(|e| format!("无法访问剪贴板: {}", e))?;
            clipboard
                .set_text(&text)
                .map_err(|e| format!("写入剪贴板失败: {}", e))?;
            Ok(())
        };

        if record.unwrap_or(false) {
            write_fn()
        } else {
            super::with_paused_monitor(&state, write_fn)
        }
    }).await
}

// ============ 翻译选中文字功能 ============

/// 暂存待翻译的选中文本
static SELECTION_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

static PENDING_TRANSLATE_TEXT: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// 获取系统当前选中的文字（通过模拟 Ctrl+C 读取剪贴板）
fn get_selected_text_from_system(state: &Arc<AppState>) -> Result<String, String> {
    #[cfg(windows)]
    {
        super::selection::selected_text(state)
    }
    #[cfg(not(windows))]
    {
        let _ = state;
        Err("当前平台未实现系统选中文字读取".into())
    }
}

/// 前端挂载后调用，获取暂存的待翻译文本
#[tauri::command]
pub async fn get_pending_translate_text() -> Result<String, String> {
    let text = std::mem::take(&mut *PENDING_TRANSLATE_TEXT.lock().unwrap());
    Ok(text)
}

/// 打开翻译选中文字结果窗口
#[tauri::command]
pub async fn open_translate_result_window(app: tauri::AppHandle, text: String) -> Result<(), String> {
    let _creation = super::WINDOW_CREATION.lock().await;
    super::run_blocking("open_translate_result_window", move || {
        let label = "translate-result";

        // Save before checking existence: the webview may exist before its listener.
        *PENDING_TRANSLATE_TEXT.lock().unwrap() = text.clone();

        if let Some(window) = app.get_webview_window(label) {
            let _ = window.emit("translate-result-update", &text);
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_always_on_top(true);
            let _ = window.set_focus();
            return Ok(());
        }

        crate::webview_runtime::ensure_runtime_current(&app)?;
        let creation_guard = crate::webview_runtime::WindowCreationGuard::start(&app, label);
        let page_load_guard = creation_guard.clone();
        let build_result = tauri::WebviewWindowBuilder::new(
            &app,
            label,
            tauri::WebviewUrl::App("/translate-result".into()),
        )
        .title("翻译选中文字")
        .inner_size(520.0, 420.0)
        .min_inner_size(360.0, 300.0)
        .decorations(false)
        .transparent(true)
        .shadow(true)
        .visible(false)
        .resizable(true)
        .always_on_top(true)
        .center()
        .on_page_load(move |window, payload| {
            page_load_guard.on_page_load(&window, &payload);
        })
        .build();
        match build_result {
            Ok(_) => {}
            Err(error) => {
                creation_guard.cancel();
                return Err(crate::webview_runtime::window_operation_error(
                    &app,
                    label,
                    "创建翻译结果窗口失败",
                    error,
                ));
            }
        }

        Ok(())
    }).await
}

/// 注册翻译选中文字快捷键
pub fn register_translate_selection_shortcut(app: &tauri::AppHandle) {
    let state = match app.try_state::<Arc<AppState>>() {
        Some(s) => s,
        None => return,
    };
    let settings_repo = database::SettingsRepository::new(&state.db);

    // 条目翻译总开关必须打开
    let translate_enabled = settings_repo
        .get("translate_enabled")
        .ok()
        .flatten()
        .map(|v| v == "true")
        .unwrap_or(false);
    if !translate_enabled {
        return;
    }

    let enabled = settings_repo
        .get("translate_selection_enabled")
        .ok()
        .flatten()
        .map(|v| v == "true")
        .unwrap_or(false);
    if !enabled {
        return;
    }

    let shortcut_str = match settings_repo.get("translate_selection_shortcut").ok().flatten() {
        Some(s) if !s.is_empty() => s,
        _ => return,
    };

    let registered = crate::hotkey::register(
        &shortcut_str,
        std::sync::Arc::new(|app, key_state| {
            if key_state == crate::hotkey::KeyState::Pressed {
                let Some(guard) = super::activity::ActivityGuard::acquire(&SELECTION_ACTIVE) else { return; };
                let app = app.clone();
                std::thread::spawn(move || {
                    let _guard = guard;
                    trigger_translate_selection(&app);
                });
            }
        }),
    );

    if !registered {
        tracing::warn!("翻译选中文字快捷键格式无效: {}", shortcut_str);
    } else {
        tracing::info!("翻译选中文字快捷键已注册: {}", shortcut_str);
    }
}

/// 注销翻译选中文字快捷键
pub fn unregister_translate_selection_shortcut(app: &tauri::AppHandle) {
    let state = match app.try_state::<Arc<AppState>>() {
        Some(s) => s,
        None => return,
    };
    let settings_repo = database::SettingsRepository::new(&state.db);

    if let Some(shortcut_str) = settings_repo.get("translate_selection_shortcut").ok().flatten() {
        if !shortcut_str.is_empty() {
            crate::hotkey::unregister(&shortcut_str);
        }
    }
}

/// 触发翻译选中文字流程
fn trigger_translate_selection(app: &tauri::AppHandle) {
    let state = match app.try_state::<Arc<AppState>>() {
        Some(s) => s,
        None => return,
    };

    match get_selected_text_from_system(&state) {
        Ok(text) if !text.trim().is_empty() => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = open_translate_result_window(app, text).await {
                    tracing::error!("打开翻译结果窗口失败: {}", e);
                }
            });
        }
        Ok(_) => {
            // 没有选中文字，发通知
            use tauri_plugin_notification::NotificationExt;
            let _ = app.notification()
                .builder()
                .title("翻译选中文字")
                .body("未检测到选中的文字")
                .show();
        }
        Err(e) => {
            tracing::error!("获取选中文字失败: {}", e);
            use tauri_plugin_notification::NotificationExt;
            let _ = app.notification().builder().title("翻译选中文字").body(&e).show();
        }
    }
}

/// 更新翻译选中文字快捷键（前端调用）
#[tauri::command]
pub async fn update_translate_selection_shortcut(
    app: tauri::AppHandle,
    new_shortcut: String,
) -> Result<(), String> {
    unregister_translate_selection_shortcut(&app);

    // 空字符串仅注销快捷键，不清除数据库中保存的值
    if new_shortcut.is_empty() {
        return Ok(());
    }

    let state = app.state::<Arc<AppState>>();
    let settings_repo = database::SettingsRepository::new(&state.db);
    settings_repo
        .set("translate_selection_shortcut", &new_shortcut)
        .map_err(|e| format!("保存快捷键失败: {}", e))?;

    let enabled = settings_repo
        .get("translate_selection_enabled")
        .ok()
        .flatten()
        .map(|v| v == "true")
        .unwrap_or(false);
    if enabled {
        register_translate_selection_shortcut(&app);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{microsoft_language_code, microsoft_translate_url, parse_microsoft_translation};

    #[test]
    fn microsoft_endpoint_uses_current_unauthenticated_route() {
        assert_eq!(
            microsoft_translate_url("auto", "zh"),
            "https://edge.microsoft.com/translate/translatetext?from=&to=zh-Hans&isEnterpriseClient=false"
        );
        assert_eq!(microsoft_language_code("zh-CN"), "zh-Hans");
        assert_eq!(microsoft_language_code("zh-TW"), "zh-Hant");
    }

    #[test]
    fn microsoft_response_parser_reads_translation_text() {
        let response = serde_json::json!([
            {
                "translations": [
                    { "text": "你好，世界！", "to": "zh-Hans" }
                ]
            }
        ]);
        assert_eq!(
            parse_microsoft_translation(&response).unwrap(),
            "你好，世界！"
        );
    }

    #[test]
    fn microsoft_response_parser_rejects_empty_or_invalid_results() {
        assert!(parse_microsoft_translation(&serde_json::json!([])).is_err());
        assert!(parse_microsoft_translation(&serde_json::json!([
            { "translations": [{ "text": "" }] }
        ])).is_err());
    }
}
