use futures_util::{stream, StreamExt};
use regex::Regex;
use reqwest::{header, Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncSeekExt, AsyncWriteExt, SeekFrom},
    sync::Mutex,
};

mod douyin;
mod x;

const USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/124 Safari/537.36";
const CHUNK_SIZE: u64 = 4 * 1024 * 1024;
const PARALLEL_CHUNKS: usize = 6;

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct QualityOption {
    id: u32,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DirectPart {
    order: u32,
    url: String,
    size: u64,
    duration_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VideoInfo {
    platform: String,
    bvid: String,
    title: String,
    page_title: String,
    owner: String,
    cover: String,
    duration: u64,
    quality_id: u32,
    quality_label: String,
    qualities: Vec<QualityOption>,
    format: String,
    estimated_size: u64,
    direct_url: String,
    parts: Vec<DirectPart>,
    expires_at: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    code: i32,
    message: Option<String>,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct ViewData {
    bvid: String,
    title: String,
    pic: String,
    duration: u64,
    owner: Owner,
    pages: Vec<Page>,
}

#[derive(Debug, Deserialize)]
struct Owner {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Page {
    cid: u64,
    part: String,
}

#[derive(Debug, Deserialize)]
struct PlayData {
    quality: u32,
    format: String,
    accept_quality: Vec<u32>,
    accept_description: Vec<String>,
    durl: Option<Vec<ApiPart>>,
}

#[derive(Debug, Deserialize)]
struct ApiPart {
    order: u32,
    length: u64,
    size: u64,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DownloadRequest {
    id: String,
    title: String,
    destination: String,
    parts: Vec<DirectPartInput>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DirectPartInput {
    url: String,
    size: u64,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    id: String,
    downloaded: u64,
    total: u64,
    percent: f64,
    speed_bytes: u64,
    status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadSummary {
    destination: String,
    bytes: u64,
}

fn http_client() -> Result<Client, String> {
    Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(6))
        .build()
        .map_err(|error| format!("无法初始化网络请求：{error}"))
}

fn extract_bvid(input: &str) -> Option<String> {
    Regex::new(r"(?i)(BV[0-9A-Za-z]{10})")
        .ok()?
        .captures(input)
        .and_then(|captures| captures.get(1))
        .map(|value| {
            let raw = value.as_str();
            format!("BV{}", &raw[2..])
        })
}

async fn normalize_bvid(client: &Client, input: &str) -> Result<String, String> {
    if let Some(bvid) = extract_bvid(input) {
        return Ok(bvid);
    }

    let parsed =
        url::Url::parse(input).map_err(|_| "请输入完整的 http 或 https 视频链接".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("仅支持 http 或 https 链接".into());
    }

    let response = client
        .get(parsed)
        .header(header::REFERER, "https://www.bilibili.com/")
        .send()
        .await
        .map_err(|error| format!("无法打开短链接：{error}"))?;

    extract_bvid(response.url().as_str())
        .ok_or_else(|| "暂时只支持包含 BV 号的 Bilibili 视频链接".into())
}

async fn fetch_view(client: &Client, bvid: &str) -> Result<ViewData, String> {
    let response = client
        .get("https://api.bilibili.com/x/web-interface/view")
        .query(&[("bvid", bvid)])
        .header(header::REFERER, "https://www.bilibili.com/")
        .send()
        .await
        .map_err(|error| format!("读取视频信息失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("视频服务返回异常：{error}"))?
        .json::<ApiResponse<ViewData>>()
        .await
        .map_err(|error| format!("无法识别视频信息：{error}"))?;

    if response.code != 0 {
        return Err(response
            .message
            .unwrap_or_else(|| "视频不存在或暂不可访问".into()));
    }
    response.data.ok_or_else(|| "视频信息为空".into())
}

async fn fetch_play(
    client: &Client,
    bvid: &str,
    cid: u64,
    quality: u32,
) -> Result<PlayData, String> {
    let response = client
        .get("https://api.bilibili.com/x/player/playurl")
        .query(&[
            ("bvid", bvid.to_string()),
            ("cid", cid.to_string()),
            ("qn", quality.to_string()),
            ("fnver", "0".into()),
            ("fnval", "0".into()),
            ("fourk", "1".into()),
            ("platform", "html5".into()),
        ])
        .header(
            header::REFERER,
            format!("https://www.bilibili.com/video/{bvid}"),
        )
        .send()
        .await
        .map_err(|error| format!("解析播放地址失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("播放服务返回异常：{error}"))?
        .json::<ApiResponse<PlayData>>()
        .await
        .map_err(|error| format!("无法识别播放地址：{error}"))?;

    if response.code != 0 {
        return Err(response
            .message
            .unwrap_or_else(|| "该视频暂时无法解析".into()));
    }
    response.data.ok_or_else(|| "没有找到可下载的视频流".into())
}

fn quality_name(id: u32) -> String {
    match id {
        127 => "8K 超高清",
        126 => "杜比视界",
        125 => "HDR 真彩",
        120 => "4K 超清",
        116 => "1080P 60帧",
        112 => "1080P 高码率",
        80 => "1080P 高清",
        74 => "720P 60帧",
        64 => "720P 高清",
        32 => "480P 清晰",
        16 => "360P 流畅",
        6 => "240P 极速",
        _ => "自动画质",
    }
    .to_string()
}

fn expires_at(url: &str) -> Option<u64> {
    let parsed = url::Url::parse(url).ok()?;
    for (key, value) in parsed.query_pairs() {
        if matches!(key.as_ref(), "deadline" | "expires" | "expire") {
            if let Ok(timestamp) = value.parse::<u64>() {
                return Some(timestamp);
            }
        }
    }
    None
}

async fn resolve(input: &str, preferred_quality: Option<u32>) -> Result<VideoInfo, String> {
    if douyin::is_link(input) {
        return douyin::resolve_public(input, preferred_quality).await;
    }
    if x::extract_status_id(input).is_some() {
        return x::resolve_public(input, preferred_quality).await;
    }
    let client = http_client()?;
    let bvid = normalize_bvid(&client, input.trim()).await?;
    let view = fetch_view(&client, &bvid).await?;
    let page = view
        .pages
        .first()
        .ok_or_else(|| "视频没有可播放的分P".to_string())?;
    let play = fetch_play(&client, &bvid, page.cid, preferred_quality.unwrap_or(127)).await?;
    let api_parts = play.durl.filter(|parts| !parts.is_empty()).ok_or_else(|| {
        "未获得音画合一的下载流；该视频可能需要登录、会员或受地区限制".to_string()
    })?;

    let qualities = play
        .accept_quality
        .iter()
        .enumerate()
        .map(|(index, id)| QualityOption {
            id: *id,
            label: play
                .accept_description
                .get(index)
                .cloned()
                .unwrap_or_else(|| quality_name(*id)),
            url: None,
        })
        .collect::<Vec<_>>();

    let parts = api_parts
        .into_iter()
        .map(|part| DirectPart {
            order: part.order,
            url: part.url,
            size: part.size,
            duration_ms: part.length,
        })
        .collect::<Vec<_>>();
    let direct_url = parts
        .first()
        .map(|part| part.url.clone())
        .unwrap_or_default();
    let estimated_size = parts.iter().map(|part| part.size).sum();
    let selected_label = qualities
        .iter()
        .find(|quality| quality.id == play.quality)
        .map(|quality| quality.label.clone())
        .unwrap_or_else(|| quality_name(play.quality));

    Ok(VideoInfo {
        platform: "Bilibili".into(),
        bvid: view.bvid,
        title: view.title,
        page_title: page.part.clone(),
        owner: view.owner.name,
        cover: view.pic,
        duration: view.duration,
        quality_id: play.quality,
        quality_label: selected_label,
        qualities,
        format: play.format,
        estimated_size,
        expires_at: expires_at(&direct_url),
        direct_url,
        parts,
    })
}

#[tauri::command]
async fn parse_video(input: String) -> Result<VideoInfo, String> {
    if input.trim().is_empty() {
        return Err("请先粘贴视频链接".into());
    }
    resolve(&input, None).await
}

#[tauri::command]
async fn parse_video_with_curl(input: String, curl_text: String) -> Result<VideoInfo, String> {
    if curl_text.trim().is_empty() {
        return Err("请先粘贴从浏览器复制的 cURL".into());
    }
    if douyin::is_link(&input) {
        return douyin::resolve_with_curl(&input, &curl_text).await;
    }
    x::resolve_with_curl(&input, &curl_text).await
}

#[tauri::command]
async fn resolve_quality(input: String, quality: u32) -> Result<VideoInfo, String> {
    resolve(&input, Some(quality)).await
}

fn safe_extension(path: &Path) -> Result<(), String> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(extension.as_str(), "mp4" | "flv") {
        Ok(())
    } else {
        Err("保存文件必须使用 .mp4 或 .flv 扩展名".into())
    }
}

fn source_headers(input: &str) -> (&'static str, &'static str) {
    if input.contains("twimg.com") {
        ("https://x.com/", "https://x.com")
    } else if url::Url::parse(input)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .map(|host| {
            [
                "douyinvod.com",
                "douyin.com",
                "iesdouyin.com",
                "amemv.com",
                "365yg.com",
                "bytecdn.cn",
            ]
            .iter()
            .any(|root| host == *root || host.ends_with(&format!(".{root}")))
        })
        .unwrap_or(false)
    {
        ("https://www.douyin.com/", "https://www.douyin.com")
    } else {
        ("https://www.bilibili.com/", "https://www.bilibili.com")
    }
}

async fn remote_size(client: &Client, url: &str, hinted: u64) -> Result<u64, String> {
    if hinted > 0 {
        return Ok(hinted);
    }
    let (referer, origin) = source_headers(url);
    let response = client
        .get(url)
        .header(header::RANGE, "bytes=0-0")
        .header(header::REFERER, referer)
        .header(header::ORIGIN, origin)
        .send()
        .await
        .map_err(|error| format!("无法读取文件大小：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("读取文件大小失败（{}）", response.status()));
    }
    if let Some(value) = response
        .headers()
        .get(header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
    {
        if let Some(total) = value
            .rsplit('/')
            .next()
            .and_then(|value| value.parse::<u64>().ok())
        {
            return Ok(total);
        }
    }
    response
        .content_length()
        .ok_or_else(|| "服务端没有提供文件大小，无法安全分片下载".into())
}

#[allow(clippy::too_many_arguments)]
async fn download_part(
    client: Client,
    file: Arc<Mutex<File>>,
    url: String,
    size: u64,
    base_offset: u64,
    progress: Arc<AtomicU64>,
    app: AppHandle,
    id: String,
    total: u64,
    started: Instant,
) -> Result<(), String> {
    let ranges = (0..size).step_by(CHUNK_SIZE as usize).map(|start| {
        let end = (start + CHUNK_SIZE - 1).min(size.saturating_sub(1));
        (start, end)
    });

    let mut chunks = stream::iter(ranges.map(|(start, end)| {
        let client = client.clone();
        let url = url.clone();
        async move {
            let (referer, origin) = source_headers(&url);
            let response = client
                .get(&url)
                .header(header::RANGE, format!("bytes={start}-{end}"))
                .header(header::REFERER, referer)
                .header(header::ORIGIN, origin)
                .send()
                .await
                .map_err(|error| format!("分片请求失败：{error}"))?;
            if response.status() != StatusCode::PARTIAL_CONTENT
                && !(start == 0 && response.status().is_success())
            {
                return Err(format!("下载服务拒绝了分片请求（{}）", response.status()));
            }
            let partial = response.status() == StatusCode::PARTIAL_CONTENT;
            let bytes = response
                .bytes()
                .await
                .map_err(|error| format!("读取分片失败：{error}"))?;
            let expected = if partial { end - start + 1 } else { size };
            if bytes.len() as u64 != expected || (!partial && size > CHUNK_SIZE) {
                return Err("服务端未正确返回请求的分片，已停止下载以避免文件损坏".into());
            }
            Ok::<_, String>((start, bytes))
        }
    }))
    .buffer_unordered(PARALLEL_CHUNKS);

    while let Some(chunk) = chunks.next().await {
        let (start, bytes) = chunk?;
        let received = bytes.len() as u64;
        {
            let mut target = file.lock().await;
            target
                .seek(SeekFrom::Start(base_offset + start))
                .await
                .map_err(|error| format!("定位写入位置失败：{error}"))?;
            target
                .write_all(&bytes)
                .await
                .map_err(|error| format!("写入文件失败：{error}"))?;
        }
        let downloaded = progress.fetch_add(received, Ordering::Relaxed) + received;
        let seconds = started.elapsed().as_secs_f64().max(0.1);
        let _ = app.emit(
            "download-progress",
            DownloadProgress {
                id: id.clone(),
                downloaded,
                total,
                percent: (downloaded as f64 / total.max(1) as f64 * 100.0).min(100.0),
                speed_bytes: (downloaded as f64 / seconds) as u64,
                status: "downloading".into(),
            },
        );
    }
    Ok(())
}

#[tauri::command]
async fn download_video(
    app: AppHandle,
    request: DownloadRequest,
) -> Result<DownloadSummary, String> {
    if request.parts.is_empty() {
        return Err("没有可下载的视频分片".into());
    }
    let path = Path::new(&request.destination);
    safe_extension(path)?;
    if path.exists() {
        return Err("目标文件已经存在，请选择新的文件名".into());
    }
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            return Err("目标文件夹不存在".into());
        }
    }

    let client = http_client()?;
    let mut sized_parts = Vec::with_capacity(request.parts.len());
    for part in &request.parts {
        let size = remote_size(&client, &part.url, part.size).await?;
        sized_parts.push((part.clone(), size));
    }
    let total = sized_parts.iter().map(|(_, size)| *size).sum::<u64>();
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
        .await
        .map_err(|error| format!("无法创建文件：{error}"))?;
    file.set_len(total)
        .await
        .map_err(|error| format!("无法预分配文件：{error}"))?;

    let file = Arc::new(Mutex::new(file));
    let progress = Arc::new(AtomicU64::new(0));
    let started = Instant::now();
    let mut base_offset = 0;
    for (part, size) in sized_parts {
        if let Err(error) = download_part(
            client.clone(),
            file.clone(),
            part.url,
            size,
            base_offset,
            progress.clone(),
            app.clone(),
            request.id.clone(),
            total,
            started,
        )
        .await
        {
            drop(file);
            let _ = tokio::fs::remove_file(path).await;
            return Err(error);
        }
        base_offset += size;
    }

    file.lock()
        .await
        .flush()
        .await
        .map_err(|error| format!("刷新文件失败：{error}"))?;
    let _ = app.emit(
        "download-progress",
        DownloadProgress {
            id: request.id,
            downloaded: total,
            total,
            percent: 100.0,
            speed_bytes: (total as f64 / started.elapsed().as_secs_f64().max(0.1)) as u64,
            status: "completed".into(),
        },
    );

    let _ = request.title;
    Ok(DownloadSummary {
        destination: request.destination,
        bytes: total,
    })
}

#[tauri::command]
fn current_unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            parse_video,
            parse_video_with_curl,
            resolve_quality,
            download_video,
            current_unix_time
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bvid_from_full_url() {
        assert_eq!(
            extract_bvid("https://www.bilibili.com/video/BV1UVQ6B4Efy?spm_id_from=333"),
            Some("BV1UVQ6B4Efy".into())
        );
    }

    #[test]
    fn rejects_unrelated_text() {
        assert_eq!(extract_bvid("not a video"), None);
    }

    #[tokio::test]
    #[ignore = "requires live access to Bilibili"]
    async fn live_parse_example_video() {
        let info = resolve(
            "https://www.bilibili.com/video/BV1UVQ6B4Efy?spm_id_from=333.788.videopod.sections",
            None,
        )
        .await
        .expect("example video should resolve");
        assert_eq!(info.bvid, "BV1UVQ6B4Efy");
        assert!(!info.direct_url.is_empty());
        assert!(!info.parts.is_empty());
    }
}
