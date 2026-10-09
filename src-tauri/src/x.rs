use super::{expires_at, http_client, DirectPart, QualityOption, VideoInfo};
use regex::Regex;
use reqwest::header;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const MAX_CURL_RESPONSE: usize = 12 * 1024 * 1024;

#[derive(Debug, Clone)]
struct Variant {
    id: u32,
    url: String,
    bitrate: u64,
    width: u32,
    height: u32,
}

#[derive(Debug)]
struct Media {
    cover: String,
    duration_ms: u64,
    variants: Vec<Variant>,
}

pub(super) fn extract_status_id(input: &str) -> Option<String> {
    Regex::new(r"(?i)(?:x|twitter)\.com/[^/]+/status/(\d+)")
        .ok()?
        .captures(input)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_string())
}

fn find_first_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(text) = map.get(*key).and_then(Value::as_str) {
                    if !text.trim().is_empty() {
                        return Some(text.trim().to_string());
                    }
                }
            }
            map.values()
                .find_map(|child| find_first_string(child, keys))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|child| find_first_string(child, keys)),
        _ => None,
    }
}

fn dimensions_from_url(input: &str) -> (u32, u32) {
    Regex::new(r"/(\d{2,5})x(\d{2,5})/")
        .ok()
        .and_then(|regex| regex.captures(input))
        .and_then(|captures| {
            Some((
                captures.get(1)?.as_str().parse().ok()?,
                captures.get(2)?.as_str().parse().ok()?,
            ))
        })
        .unwrap_or((0, 0))
}

fn parse_variants(value: &Value) -> Vec<Variant> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    let mut variants = Vec::new();
    let mut seen = HashSet::new();
    for item in items {
        let content_type = item
            .get("content_type")
            .or_else(|| item.get("type"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(raw_url) = item
            .get("url")
            .or_else(|| item.get("src"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if !(content_type.contains("mp4") || raw_url.contains(".mp4"))
            || !seen.insert(raw_url.to_string())
        {
            continue;
        }
        let bitrate = item.get("bitrate").and_then(Value::as_u64).unwrap_or(0);
        let (width, height) = dimensions_from_url(raw_url);
        let id = if bitrate > 0 {
            bitrate.min(u32::MAX as u64) as u32
        } else {
            width.saturating_mul(height).max(1)
        };
        variants.push(Variant {
            id,
            url: raw_url.to_string(),
            bitrate,
            width,
            height,
        });
    }
    variants
}

fn collect_media(value: &Value, output: &mut Vec<Media>) {
    match value {
        Value::Object(map) => {
            if let Some(info) = map.get("video_info") {
                let variants = parse_variants(info.get("variants").unwrap_or(&Value::Null));
                if !variants.is_empty() {
                    output.push(Media {
                        cover: map
                            .get("media_url_https")
                            .or_else(|| map.get("media_url"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        duration_ms: info
                            .get("duration_millis")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                        variants,
                    });
                }
            }
            if let Some(video) = map.get("video") {
                let variants = parse_variants(video.get("variants").unwrap_or(&Value::Null));
                if !variants.is_empty() {
                    output.push(Media {
                        cover: video
                            .get("poster")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        duration_ms: video.get("durationMs").and_then(Value::as_u64).unwrap_or(0),
                        variants,
                    });
                }
            }
            for child in map.values() {
                collect_media(child, output);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_media(child, output);
            }
        }
        _ => {}
    }
}

fn quality_label(variant: &Variant) -> String {
    if variant.height > 0 {
        let bitrate = if variant.bitrate > 0 {
            format!(" · {} kbps", variant.bitrate / 1000)
        } else {
            String::new()
        };
        format!("{}P{bitrate}", variant.height)
    } else if variant.bitrate > 0 {
        format!("{} kbps", variant.bitrate / 1000)
    } else {
        "MP4 视频".into()
    }
}

fn video_from_json(
    payload: &Value,
    status_id: &str,
    preferred: Option<u32>,
) -> Result<VideoInfo, String> {
    let mut media = Vec::new();
    collect_media(payload, &mut media);
    let mut media = media
        .into_iter()
        .max_by_key(|item| {
            item.variants
                .iter()
                .map(|variant| {
                    (variant.width as u64 * variant.height as u64).max(variant.bitrate)
                })
                .max()
                .unwrap_or(0)
        })
        .ok_or_else(|| {
            "此请求的返回内容中没有找到 MP4 视频。请复制 Network 中 TweetResultByRestId 或 TweetDetail 请求的 cURL。".to_string()
        })?;
    media.variants.sort_by_key(|variant| {
        std::cmp::Reverse((variant.width as u64 * variant.height as u64).max(variant.bitrate))
    });
    let selected = preferred
        .and_then(|id| media.variants.iter().find(|variant| variant.id == id))
        .unwrap_or(&media.variants[0])
        .clone();
    let qualities = media
        .variants
        .iter()
        .map(|variant| QualityOption {
            id: variant.id,
            label: quality_label(variant),
            url: Some(variant.url.clone()),
        })
        .collect::<Vec<_>>();
    let raw_title = find_first_string(payload, &["full_text", "text"])
        .unwrap_or_else(|| format!("X 视频 {status_id}"));
    let title = raw_title.chars().take(120).collect::<String>();
    let owner =
        find_first_string(payload, &["screen_name", "name"]).unwrap_or_else(|| "X 用户".into());
    let cover = if media.cover.starts_with("http") {
        media.cover
    } else {
        find_first_string(
            payload,
            &["media_url_https", "poster", "profile_image_url_https"],
        )
        .unwrap_or_default()
    };
    let direct_url = selected.url.clone();
    Ok(VideoInfo {
        platform: "X / Twitter".into(),
        bvid: status_id.into(),
        title: title.clone(),
        page_title: title,
        owner,
        cover,
        duration: media.duration_ms / 1000,
        quality_id: selected.id,
        quality_label: quality_label(&selected),
        qualities,
        format: "mp4".into(),
        estimated_size: 0,
        expires_at: expires_at(&direct_url),
        direct_url: direct_url.clone(),
        parts: vec![DirectPart {
            order: 1,
            url: direct_url,
            size: 0,
            duration_ms: media.duration_ms,
        }],
    })
}

pub(super) async fn resolve_public(
    input: &str,
    preferred: Option<u32>,
) -> Result<VideoInfo, String> {
    let status_id =
        extract_status_id(input).ok_or_else(|| "无法识别 X/Twitter 动态编号".to_string())?;
    let client = http_client()?;
    let response = client
        .get("https://cdn.syndication.twimg.com/tweet-result")
        .query(&[("id", status_id.as_str()), ("lang", "zh-cn")])
        .send()
        .await
        .map_err(|error| format!("X_CURL_REQUIRED:读取 X 动态失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "X_CURL_REQUIRED:公开解析受到限制（{}）",
            response.status()
        ));
    }
    let payload = response
        .json::<Value>()
        .await
        .map_err(|_| "X_CURL_REQUIRED:X 返回了无法识别的数据".to_string())?;
    video_from_json(&payload, &status_id, preferred)
        .map_err(|error| format!("X_CURL_REQUIRED:{error}"))
}

pub(super) fn tokenize_curl(input: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(character) = chars.next() {
        match quote {
            Some('\'') => {
                if character == '\'' {
                    quote = None;
                } else {
                    current.push(character);
                }
            }
            Some('"') => {
                if character == '"' {
                    quote = None;
                } else if character == '\\' {
                    if let Some(next) = chars.next() {
                        if next != '\n' {
                            current.push(next);
                        }
                    }
                } else {
                    current.push(character);
                }
            }
            _ => match character {
                '\'' | '"' => quote = Some(character),
                '\\' => {
                    if let Some(next) = chars.next() {
                        if next != '\n' && next != '\r' {
                            current.push(next);
                        }
                    }
                }
                value if value.is_whitespace() => {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(character),
            },
        }
    }
    if quote.is_some() {
        return Err("cURL 中有未闭合的引号".into());
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn allowed_host(host: &str) -> bool {
    ["x.com", "twitter.com"]
        .iter()
        .any(|root| host == *root || host.ends_with(&format!(".{root}")))
}

fn parse_curl(input: &str) -> Result<(url::Url, HashMap<String, String>), String> {
    let tokens = tokenize_curl(input)?;
    if tokens.first().map(String::as_str) != Some("curl") {
        return Err("请粘贴以 curl 开头的完整请求".into());
    }
    let mut target = None;
    let mut headers = HashMap::new();
    let mut index = 1;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "--url" => {
                index += 1;
                target = tokens.get(index).cloned();
            }
            "-H" | "--header" => {
                index += 1;
                if let Some(raw) = tokens.get(index) {
                    if let Some((name, value)) = raw.split_once(':') {
                        let name = name.trim().to_ascii_lowercase();
                        if [
                            "accept",
                            "authorization",
                            "cookie",
                            "referer",
                            "user-agent",
                            "x-csrf-token",
                            "x-client-transaction-id",
                            "x-twitter-active-user",
                            "x-twitter-auth-type",
                            "x-twitter-client-language",
                        ]
                        .contains(&name.as_str())
                        {
                            headers.insert(name, value.trim().to_string());
                        }
                    }
                }
            }
            "-b" | "--cookie" => {
                index += 1;
                if let Some(cookie) = tokens.get(index) {
                    headers.insert("cookie".into(), cookie.clone());
                }
            }
            item if item.starts_with("https://") && target.is_none() => {
                target = Some(item.to_string());
            }
            _ => {}
        }
        index += 1;
    }
    let parsed = url::Url::parse(
        target
            .as_deref()
            .ok_or_else(|| "cURL 中没有找到请求地址".to_string())?,
    )
    .map_err(|_| "cURL 的请求地址无效".to_string())?;
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    if parsed.scheme() != "https" || !allowed_host(&host) {
        return Err("为保护设备，只允许重放 X/Twitter 的 HTTPS 请求".into());
    }
    Ok((parsed, headers))
}

pub(super) async fn resolve_with_curl(input: &str, curl_text: &str) -> Result<VideoInfo, String> {
    let status_id =
        extract_status_id(input).ok_or_else(|| "请先填写完整的 X/Twitter 动态链接".to_string())?;
    let (target, supplied_headers) = parse_curl(curl_text)?;
    let client = http_client()?;
    let mut request = client.get(target);
    for (name, value) in supplied_headers {
        let Ok(name) = header::HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(value) = header::HeaderValue::from_str(&value) else {
            continue;
        };
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("X 请求失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "X 请求未通过（{}），请重新复制最新 cURL",
            response.status()
        ));
    }
    if response.content_length().unwrap_or(0) > MAX_CURL_RESPONSE as u64 {
        return Err("X 返回内容过大，已停止处理".into());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("读取 X 返回内容失败：{error}"))?;
    if bytes.len() > MAX_CURL_RESPONSE {
        return Err("X 返回内容过大，已停止处理".into());
    }
    let payload = serde_json::from_slice::<Value>(&bytes)
        .map_err(|_| "该 cURL 返回的不是 JSON，请复制推文详情接口请求".to_string())?;
    video_from_json(&payload, &status_id, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_status_id() {
        assert_eq!(
            extract_status_id("https://x.com/onpe_aep/status/2063790473387593977/video/1"),
            Some("2063790473387593977".into())
        );
    }

    #[test]
    fn parses_curl_without_executing_it() {
        let sample = "curl --url 'https://x.com/i/api/graphql/example/TweetResultByRestId?variables=x' -H 'authorization: Bearer example' -b 'ct0=example'";
        let (target, headers) = parse_curl(sample).expect("valid X curl");
        assert_eq!(target.host_str(), Some("x.com"));
        assert_eq!(
            headers.get("cookie").map(String::as_str),
            Some("ct0=example")
        );
    }

    #[test]
    fn blocks_non_x_targets() {
        assert!(parse_curl("curl 'https://example.com/private'").is_err());
    }

    #[test]
    fn extracts_mp4_variants_from_tweet_json() {
        let payload = serde_json::json!({
            "legacy": {
                "full_text": "A test video",
                "extended_entities": { "media": [{
                    "media_url_https": "https://pbs.twimg.com/media/poster.jpg",
                    "video_info": {
                        "duration_millis": 42000,
                        "variants": [
                            { "content_type": "application/x-mpegURL", "url": "https://video.twimg.com/test.m3u8" },
                            { "content_type": "video/mp4", "bitrate": 832000, "url": "https://video.twimg.com/ext_tw_video/1/pu/vid/640x360/test.mp4" },
                            { "content_type": "video/mp4", "bitrate": 2176000, "url": "https://video.twimg.com/ext_tw_video/1/pu/vid/1280x720/test.mp4" }
                        ]
                    }
                }]}
            },
            "screen_name": "tester"
        });
        let video = video_from_json(&payload, "123", None).expect("video should parse");
        assert_eq!(video.platform, "X / Twitter");
        assert_eq!(video.quality_id, 2176000);
        assert_eq!(video.duration, 42);
        assert_eq!(video.qualities.len(), 2);
    }
}
