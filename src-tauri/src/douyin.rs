use super::{expires_at, http_client, DirectPart, QualityOption, VideoInfo};
use reqwest::{header, Client};
use serde_json::Value;
use url::Url;

fn allowed_host(host: &str) -> bool {
    ["douyin.com", "iesdouyin.com"]
        .iter()
        .any(|root| host == *root || host.ends_with(&format!(".{root}")))
}

fn input_url(input: &str) -> Option<Url> {
    let re = regex::Regex::new(r#"https?://[^\s<>"）)]+"#).ok()?;
    let url = Url::parse(re.find(input)?.as_str()).ok()?;
    allowed_host(url.host_str()?).then_some(url)
}

pub(super) fn is_link(input: &str) -> bool {
    input_url(input).is_some()
}

fn video_id(url: &Url) -> Option<String> {
    let id = url
        .query_pairs()
        .find(|(key, _)| key == "modal_id" || key == "aweme_id")
        .map(|(_, value)| value.into_owned())
        .or_else(|| {
            let segments: Vec<_> = url.path_segments()?.collect();
            segments
                .windows(2)
                .find(|s| s[0] == "video" || s[0] == "note")
                .map(|s| s[1].to_string())
        })?;
    (id.len() >= 15 && id.len() <= 25 && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
}

async fn normalize(client: &Client, input: &str) -> Result<String, String> {
    let url = input_url(input).ok_or("请输入完整的抖音视频链接")?;
    if let Some(id) = video_id(&url) {
        return Ok(id);
    }
    if url.host_str() != Some("v.douyin.com") {
        return Err("抖音主页链接需包含 modal_id，或粘贴单条作品链接".into());
    }
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "无法展开抖音分享短链")?;
    if !allowed_host(response.url().host_str().unwrap_or_default()) {
        return Err("分享短链跳转到了非抖音地址".into());
    }
    video_id(response.url()).ok_or_else(|| "分享链接未指向单条抖音视频".into())
}

fn find_aweme<'a>(value: &'a Value, id: &str) -> Option<&'a Value> {
    if value.get("aweme_id").and_then(Value::as_str) == Some(id) && value.get("video").is_some() {
        return Some(value);
    }
    match value {
        Value::Object(map) => map.values().find_map(|v| find_aweme(v, id)),
        Value::Array(array) => array.iter().find_map(|v| find_aweme(v, id)),
        _ => None,
    }
}

fn media_url(value: &Value) -> Option<String> {
    value
        .get("url_list")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .find(|raw| {
            Url::parse(raw)
                .map(|u| matches!(u.scheme(), "http" | "https"))
                .unwrap_or(false)
        })
        .map(str::to_string)
}

fn from_json(payload: &Value, id: &str, preferred: Option<u32>) -> Result<VideoInfo, String> {
    let aweme = find_aweme(payload, id).ok_or("返回内容没有该视频，请复制该作品的详情请求")?;
    let video = &aweme["video"];
    let mut streams = Vec::new();
    if let Some(rates) = video["bit_rate"].as_array() {
        for rate in rates {
            if let Some(url) = media_url(&rate["play_addr"]) {
                let height = rate["play_addr"]["height"]
                    .as_u64()
                    .or(video["height"].as_u64())
                    .unwrap_or(0);
                let bitrate = rate["bit_rate"].as_u64().unwrap_or(0);
                streams.push((
                    height,
                    bitrate,
                    url,
                    rate["play_addr"]["data_size"].as_u64().unwrap_or(0),
                ));
            }
        }
    }
    if let Some(url) = media_url(&video["play_addr"]) {
        streams.push((
            video["height"].as_u64().unwrap_or(0),
            0,
            url,
            video["play_addr"]["data_size"].as_u64().unwrap_or(0),
        ));
    }
    streams.sort_by(|a, b| (b.0, b.1).cmp(&(a.0, a.1)));
    streams.dedup_by(|a, b| a.2 == b.2);
    if streams.is_empty() {
        return Err("该作品没有可下载的视频流，暂不支持图集或直播".into());
    }
    let qualities: Vec<_> = streams
        .iter()
        .enumerate()
        .map(|(i, s)| QualityOption {
            id: i as u32 + 1,
            label: if s.0 > 0 {
                format!(
                    "{}P · {}",
                    s.0,
                    if s.1 > 0 {
                        format!("{} kbps", s.1 / 1000)
                    } else {
                        "默认".into()
                    }
                )
            } else {
                "原始画质".into()
            },
            url: Some(s.2.clone()),
        })
        .collect();
    let selected = preferred
        .and_then(|p| qualities.iter().position(|q| q.id == p))
        .unwrap_or(0);
    let stream = &streams[selected];
    let title = aweme["desc"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("抖音视频")
        .to_string();
    let duration_ms = video["duration"]
        .as_u64()
        .or(aweme["duration"].as_u64())
        .unwrap_or(0);
    Ok(VideoInfo {
        platform: "抖音".into(),
        bvid: id.into(),
        page_title: title.clone(),
        title,
        owner: aweme["author"]["nickname"]
            .as_str()
            .unwrap_or("未知作者")
            .into(),
        cover: media_url(&video["cover"])
            .or_else(|| media_url(&video["origin_cover"]))
            .unwrap_or_default(),
        duration: duration_ms / 1000,
        quality_id: qualities[selected].id,
        quality_label: qualities[selected].label.clone(),
        qualities,
        format: "mp4".into(),
        estimated_size: stream.3,
        direct_url: stream.2.clone(),
        expires_at: expires_at(&stream.2),
        parts: vec![DirectPart {
            order: 1,
            url: stream.2.clone(),
            size: stream.3,
            duration_ms,
        }],
    })
}

fn from_html(html: &str, id: &str, preferred: Option<u32>) -> Option<VideoInfo> {
    let router =
        regex::Regex::new(r"(?s)(?:window\.)?_ROUTER_DATA\s*=\s*(\{.*?\})\s*;?\s*</script>")
            .ok()?;
    if let Some(c) = router.captures(html) {
        if let Ok(value) = serde_json::from_str::<Value>(&c[1]) {
            if let Ok(info) = from_json(&value, id, preferred) {
                return Some(info);
            }
        }
    }
    let render =
        regex::Regex::new(r#"(?s)<script[^>]*id=["']RENDER_DATA["'][^>]*>(.*?)</script>"#).ok()?;
    let data = render.captures(html)?;
    // RENDER_DATA is percent-encoded JSON, not form data (a literal '+' must survive).
    let decoded =
        url::form_urlencoded::parse(format!("data={}", data[1].replace('+', "%2B")).as_bytes())
            .next()?
            .1
            .into_owned();
    from_json(
        &serde_json::from_str::<Value>(&decoded).ok()?,
        id,
        preferred,
    )
    .ok()
}

pub(super) async fn resolve_public(
    input: &str,
    preferred: Option<u32>,
) -> Result<VideoInfo, String> {
    let client = http_client()?;
    let id = normalize(&client, input).await?;
    for target in [
        format!("https://www.iesdouyin.com/share/video/{id}/"),
        format!("https://www.douyin.com/video/{id}"),
    ] {
        if let Ok(response) = client
            .get(target)
            .header(header::REFERER, "https://www.douyin.com/")
            .send()
            .await
        {
            if let Ok(html) = response.text().await {
                if let Some(info) = from_html(&html, &id, preferred) {
                    return Ok(info);
                }
            }
        }
    }
    if let Ok(response) = client
        .get("https://www.douyin.com/aweme/v1/web/aweme/detail/")
        .query(&[("aweme_id", &id)])
        .header(header::REFERER, "https://www.douyin.com/")
        .send()
        .await
    {
        if let Ok(payload) = response.json::<Value>().await {
            if let Ok(info) = from_json(&payload, &id, preferred) {
                return Ok(info);
            }
        }
    }
    Err("DOUYIN_CURL_REQUIRED: 抖音要求登录或验证，请在浏览器打开该视频后复制 aweme/detail 请求的 cURL 重试".into())
}

pub(super) async fn resolve_with_curl(input: &str, curl: &str) -> Result<VideoInfo, String> {
    let client = http_client()?;
    let id = normalize(&client, input).await?;
    let tokens = super::x::tokenize_curl(curl)?;
    if tokens.first().map(String::as_str) != Some("curl") {
        return Err("请粘贴以 curl 开头的完整请求".into());
    }
    let mut target = None;
    let mut headers = Vec::new();
    let mut i = 1;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "--url" => {
                i += 1;
                target = tokens.get(i).cloned();
            }
            "-H" | "--header" => {
                i += 1;
                if let Some((name, value)) = tokens.get(i).and_then(|s| s.split_once(':')) {
                    if ["cookie", "user-agent", "referer", "accept"]
                        .contains(&name.trim().to_ascii_lowercase().as_str())
                    {
                        headers.push((name.trim().to_string(), value.trim().to_string()));
                    }
                }
            }
            "-b" | "--cookie" => {
                i += 1;
                if let Some(value) = tokens.get(i) {
                    headers.push(("cookie".into(), value.clone()));
                }
            }
            raw if raw.starts_with("https://") && target.is_none() => target = Some(raw.into()),
            _ => {}
        }
        i += 1;
    }
    let target = Url::parse(target.as_deref().ok_or("cURL 中没有请求地址")?)
        .map_err(|_| "cURL 请求地址无效")?;
    if target.scheme() != "https"
        || target.host_str() != Some("www.douyin.com")
        || target.port_or_known_default() != Some(443)
        || target.path() != "/aweme/v1/web/aweme/detail/"
        || !target.username().is_empty()
        || target.password().is_some()
    {
        return Err("仅允许重试 www.douyin.com 的 HTTPS 视频详情接口".into());
    }
    if !target
        .query_pairs()
        .any(|(k, v)| k == "aweme_id" && v == id)
    {
        return Err("cURL 的视频 ID 与输入链接不一致".into());
    }
    // Never forward login credentials through redirects.
    let client = Client::builder()
        .user_agent(super::USER_AGENT)
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "无法初始化请求")?;
    let mut request = client.get(target);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|_| "抖音详情请求失败，请重新复制最新 cURL")?;
    if !response.status().is_success() {
        return Err("抖音详情请求未通过，请在浏览器完成验证后重试".into());
    }
    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "读取抖音详情失败")?;
        if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
            return Err("详情响应过大".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let payload = serde_json::from_slice::<Value>(&bytes)
        .map_err(|_| "详情接口未返回 JSON，请重新复制最新 cURL")?;
    from_json(&payload, &id, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "7693074423943920063";
    #[test]
    fn supported_urls() {
        for raw in [
            format!("https://www.douyin.com/user/self?modal_id={ID}&showTab=record"),
            format!("https://www.douyin.com/video/{ID}"),
            format!("https://www.iesdouyin.com/share/video/{ID}/"),
        ] {
            assert_eq!(video_id(&input_url(&raw).unwrap()).as_deref(), Some(ID));
        }
        assert!(is_link("分享文案 https://v.douyin.com/abc/ 复制打开抖音"));
        assert!(!is_link("https://douyin.com.evil.test/video/123"));
        assert!(video_id(&input_url("https://www.douyin.com/user/self").unwrap()).is_none());
    }
    #[test]
    fn parses_matching_video_and_rejects_other_works() {
        let payload = serde_json::json!({"aweme_detail": {"aweme_id": ID, "desc": "示例", "author": {"nickname": "作者"}, "video": {"duration": 12345, "height": 1080, "play_addr": {"url_list": ["https://v.example/video.mp4"], "data_size": 123}, "cover": {"url_list": ["https://p.example/cover.jpg"]}}}});
        let info = from_json(&payload, ID, None).unwrap();
        assert_eq!(info.duration, 12);
        assert_eq!(info.estimated_size, 123);
        assert!(from_json(&payload, "1111111111111111111", None).is_err());
        let html = format!("<script>window._ROUTER_DATA = {payload};</script>");
        assert_eq!(from_html(&html, ID, None).unwrap().title, "示例");
        let encoded: String =
            url::form_urlencoded::byte_serialize(payload.to_string().as_bytes()).collect();
        assert!(from_html(
            &format!("<script id=\"RENDER_DATA\">{encoded}</script>"),
            ID,
            None
        )
        .is_some());
    }
    #[tokio::test]
    async fn rejects_unsafe_or_mismatched_curl_before_request() {
        let input = format!("https://www.douyin.com/video/{ID}");
        for target in [
            format!("https://www.douyin.com.evil.test/aweme/v1/web/aweme/detail/?aweme_id={ID}"),
            format!("https://www.douyin.com:444/aweme/v1/web/aweme/detail/?aweme_id={ID}"),
            format!("https://www.douyin.com/unrelated?aweme_id={ID}"),
            "https://www.douyin.com/aweme/v1/web/aweme/detail/?aweme_id=1111111111111111111".into(),
        ] {
            assert!(resolve_with_curl(&input, &format!("curl '{target}'"))
                .await
                .is_err());
        }
    }

    #[test]
    fn selects_quality_and_rejects_image_posts() {
        let payload = serde_json::json!({"aweme_id": ID, "video": {"bit_rate": [
            {"bit_rate": 1000000, "play_addr": {"height": 720, "url_list": ["https://example.test/720.mp4"]}},
            {"bit_rate": 2000000, "play_addr": {"height": 1080, "url_list": ["https://example.test/1080.mp4"]}}
        ]}});
        let info = from_json(&payload, ID, None).unwrap();
        assert_eq!(info.direct_url, "https://example.test/1080.mp4");
        assert_eq!(
            from_json(&payload, ID, Some(2)).unwrap().direct_url,
            "https://example.test/720.mp4"
        );
        assert!(from_json(
            &serde_json::json!({"aweme_id": ID, "video": {}, "images": []}),
            ID,
            None
        )
        .is_err());
    }

    #[tokio::test]
    #[ignore = "requires live Douyin access"]
    async fn live_example() {
        let result = resolve_public(
            &format!("https://www.douyin.com/user/self?modal_id={ID}"),
            None,
        )
        .await;
        match result {
            Ok(info) => {
                println!("Live Douyin: resolved video {}", info.bvid);
                assert!(!info.direct_url.is_empty());
            }
            Err(error) => {
                println!("Live Douyin: browser verification required");
                assert!(error.starts_with("DOUYIN_CURL_REQUIRED:"), "{error}");
            }
        }
    }
}
