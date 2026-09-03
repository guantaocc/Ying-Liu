import { useEffect, useMemo, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save } from "@tauri-apps/plugin-dialog";
import {
  ArrowDownToLine,
  Check,
  ChevronDown,
  CircleHelp,
  Clock3,
  Copy,
  Download,
  Film,
  Gauge,
  History,
  Link2,
  LoaderCircle,
  Play,
  RotateCcw,
  Scissors,
  Settings2,
  ShieldCheck,
  Sparkles,
  WandSparkles,
  Zap,
} from "lucide-react";
import { Toaster, toast } from "sonner";
import "./App.css";

type QualityOption = { id: number; label: string; url?: string };
type DirectPart = { order: number; url: string; size: number; durationMs: number };

type VideoInfo = {
  platform: string;
  bvid: string;
  title: string;
  pageTitle: string;
  owner: string;
  cover: string;
  duration: number;
  qualityId: number;
  qualityLabel: string;
  qualities: QualityOption[];
  format: string;
  estimatedSize: number;
  directUrl: string;
  parts: DirectPart[];
  expiresAt?: number;
};

type Progress = {
  id: string;
  downloaded: number;
  total: number;
  percent: number;
  speedBytes: number;
  status: "downloading" | "completed";
};

type DownloadRecord = {
  id: string;
  title: string;
  cover: string;
  destination: string;
  size: number;
  createdAt: number;
};

const EXAMPLE_URL =
  "https://www.bilibili.com/video/BV1UVQ6B4Efy?spm_id_from=333.788.videopod.sections";

function formatBytes(bytes: number) {
  if (!bytes) return "未知大小";
  const units = ["B", "KB", "MB", "GB"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / 1024 ** index).toFixed(index > 1 ? 1 : 0)} ${units[index]}`;
}

function formatDuration(seconds: number) {
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const rest = seconds % 60;
  return [hours, minutes, rest]
    .filter((_, index) => hours > 0 || index > 0)
    .map((part) => String(part).padStart(2, "0"))
    .join(":");
}

function safeFileName(title: string) {
  return title.replace(/[\\/:*?"<>|]/g, " ").replace(/\s+/g, " ").trim().slice(0, 90) || "video";
}

function readHistory(): DownloadRecord[] {
  try {
    return JSON.parse(localStorage.getItem("yingliu-downloads") || "[]") as DownloadRecord[];
  } catch {
    return [];
  }
}

function App() {
  const [url, setUrl] = useState(EXAMPLE_URL);
  const [video, setVideo] = useState<VideoInfo | null>(null);
  const [isParsing, setIsParsing] = useState(false);
  const [isChangingQuality, setIsChangingQuality] = useState(false);
  const [progress, setProgress] = useState<Progress | null>(null);
  const [history, setHistory] = useState<DownloadRecord[]>(readHistory);
  const [showDetails, setShowDetails] = useState(false);
  const [showCurlFallback, setShowCurlFallback] = useState(false);
  const [curlText, setCurlText] = useState("");
  const [isParsingCurl, setIsParsingCurl] = useState(false);

  const isDownloading = progress?.status === "downloading";
  const expiryText = useMemo(() => {
    if (!video?.expiresAt) return "临时直链";
    const minutes = Math.max(0, Math.floor((video.expiresAt * 1000 - Date.now()) / 60000));
    return `约 ${minutes} 分钟后失效`;
  }, [video]);

  useEffect(() => {
    if (!isTauri()) return;
    const unlisten = listen<Progress>("download-progress", ({ payload }) => setProgress(payload));
    return () => {
      void unlisten.then((dispose) => dispose());
    };
  }, []);

  useEffect(() => {
    localStorage.setItem("yingliu-downloads", JSON.stringify(history));
  }, [history]);

  async function parseLink() {
    if (!url.trim()) {
      toast.error("请先粘贴一个视频链接");
      return;
    }
    setIsParsing(true);
    setVideo(null);
    setProgress(null);
    setShowCurlFallback(false);
    try {
      const result = await invoke<VideoInfo>("parse_video", { input: url.trim() });
      setVideo(result);
      toast.success("解析完成，直链已生成");
    } catch (error) {
      const message = String(error);
      if (message.includes("X_CURL_REQUIRED:")) {
        setShowCurlFallback(true);
        toast.info("该 X 视频需要登录请求，已打开本机 cURL 重试", { duration: 5000 });
      } else {
        toast.error(message, { duration: 5000 });
      }
    } finally {
      setIsParsing(false);
    }
  }

  async function parseWithCurl() {
    if (!curlText.trim()) {
      toast.error("请先粘贴从浏览器复制的 cURL");
      return;
    }
    setIsParsingCurl(true);
    try {
      const result = await invoke<VideoInfo>("parse_video_with_curl", {
        input: url.trim(),
        curlText,
      });
      setVideo(result);
      setShowCurlFallback(false);
      setCurlText("");
      toast.success("X 视频解析完成，登录信息未保存");
    } catch (error) {
      toast.error(String(error), { duration: 6500 });
    } finally {
      setIsParsingCurl(false);
    }
  }

  async function changeQuality(quality: number) {
    if (!video || quality === video.qualityId) return;
    const localQuality = video.qualities.find((item) => item.id === quality);
    if (video.platform.startsWith("X") && localQuality?.url) {
      setVideo({
        ...video,
        qualityId: quality,
        qualityLabel: localQuality.label,
        directUrl: localQuality.url,
        parts: video.parts.map((part, index) =>
          index === 0 ? { ...part, url: localQuality.url!, size: 0 } : part
        ),
        estimatedSize: 0,
      });
      toast.success(`已切换为 ${localQuality.label}`);
      return;
    }
    setIsChangingQuality(true);
    try {
      const result = await invoke<VideoInfo>("resolve_quality", { input: url.trim(), quality });
      setVideo(result);
      toast.success(`已切换为 ${result.qualityLabel}`);
    } catch (error) {
      toast.error(String(error));
    } finally {
      setIsChangingQuality(false);
    }
  }

  async function copyDirectLink() {
    if (!video) return;
    try {
      await navigator.clipboard.writeText(video.directUrl);
      toast.success("直链已复制");
    } catch {
      toast.error("复制失败，请手动选择链接");
    }
  }

  async function startDownload() {
    if (!video || isDownloading) return;
    const extension = video.format.toLowerCase().includes("flv") ? "flv" : "mp4";
    const destination = await save({
      title: "保存视频",
      defaultPath: `${safeFileName(video.title)}.${extension}`,
      filters: [{ name: extension.toUpperCase(), extensions: [extension] }],
    });
    if (!destination) return;

    const id = crypto.randomUUID();
    setProgress({ id, downloaded: 0, total: video.estimatedSize, percent: 0, speedBytes: 0, status: "downloading" });
    try {
      await invoke("download_video", {
        request: {
          id,
          title: video.title,
          destination,
          parts: video.parts.map(({ url: partUrl, size }) => ({ url: partUrl, size })),
        },
      });
      const record: DownloadRecord = {
        id,
        title: video.title,
        cover: video.cover,
        destination,
        size: video.estimatedSize,
        createdAt: Date.now(),
      };
      setHistory((items) => [record, ...items].slice(0, 20));
      toast.success("视频已保存到本地");
    } catch (error) {
      setProgress(null);
      toast.error(String(error), { duration: 6000 });
    }
  }

  function reset() {
    setVideo(null);
    setProgress(null);
    setUrl("");
    setCurlText("");
    setShowCurlFallback(false);
  }

  return (
    <>
      <Toaster position="top-center" richColors closeButton />
      <div className="app-shell">
        <aside className="sidebar">
        <div className="brand">
          <span className="brand-mark"><Scissors size={19} strokeWidth={2.4} /></span>
          <span>映流</span>
        </div>

        <nav className="nav-list" aria-label="应用导航">
          <a className="nav-item active" href="#parser"><WandSparkles size={17} />链接解析</a>
          <a className="nav-item" href="#downloads"><History size={17} />下载记录{history.length > 0 && <b>{history.length}</b>}</a>
        </nav>

        <div className="sidebar-spacer" />
        <div className="privacy-note"><ShieldCheck size={16} /><span>解析与下载均在<br />你的设备上完成</span></div>
        <button className="nav-item plain" type="button"><Settings2 size={17} />偏好设置</button>
        <button className="nav-item plain" type="button"><CircleHelp size={17} />使用帮助</button>
        <div className="version">映流 0.2.0 · Tauri 2</div>
        </aside>

        <main className="workspace">
        <header className="topbar">
          <div className="crumb"><span>工具箱</span><i>/</i><strong>链接解析</strong></div>
          <div className="engine-status"><span /><span>本地引擎已就绪</span></div>
        </header>

        <div className="content" id="parser">
          <section className="hero">
            <div className="eyebrow"><Sparkles size={14} />专注、快速、无广告</div>
            <h1>把喜欢的视频，<br /><em>好好留在这里。</em></h1>
            <p>粘贴 Bilibili 或 X/Twitter 视频链接，生成临时直链并分片下载到本地。</p>
          </section>

          <section className={`parser-card ${isParsing ? "is-loading" : ""}`}>
            <form
              className="url-form"
              onSubmit={(event) => {
                event.preventDefault();
                void parseLink();
              }}
            >
              <div className="url-input-wrap">
                <Link2 size={19} />
                <input
                  aria-label="视频链接"
                  value={url}
                  onChange={(event) => setUrl(event.target.value)}
                  placeholder="粘贴 Bilibili 或 X/Twitter 视频链接…"
                  spellCheck={false}
                  disabled={isParsing || isDownloading}
                />
                {url && !isParsing && <button className="clear-button" type="button" onClick={() => setUrl("")} aria-label="清空">×</button>}
              </div>
              <button className="primary-button" type="submit" disabled={isParsing || isDownloading}>
                {isParsing ? <LoaderCircle className="spin" size={18} /> : <Zap size={18} fill="currentColor" />}
                {isParsing ? "正在解析" : "开始解析"}
              </button>
            </form>
            <div className="parser-meta">
              <span><Check size={13} />支持 Bilibili、b23.tv 与 X/Twitter 动态链接</span>
              <button type="button" onClick={() => setUrl(EXAMPLE_URL)}>填入示例</button>
            </div>
          </section>

          {showCurlFallback && !isParsing && (
            <section className="curl-fallback" aria-live="polite">
              <div className="curl-heading">
                <div className="curl-icon"><ShieldCheck size={19} /></div>
                <div>
                  <strong>此 X 视频需要登录请求</strong>
                  <p>在浏览器开发者工具的 Network 中找到 <code>TweetResultByRestId</code> 或 <code>TweetDetail</code>，右键选择 Copy as cURL。</p>
                </div>
              </div>
              <textarea
                value={curlText}
                onChange={(event) => setCurlText(event.target.value)}
                placeholder="在这里粘贴以 curl 开头的完整请求…"
                aria-label="X 推文详情 cURL"
                spellCheck={false}
                disabled={isParsingCurl}
              />
              <div className="curl-actions">
                <span><ShieldCheck size={13} />仅在内存中使用，不执行、不保存</span>
                <button type="button" onClick={() => void parseWithCurl()} disabled={isParsingCurl}>
                  {isParsingCurl ? <LoaderCircle className="spin" size={16} /> : <Zap size={16} />}
                  {isParsingCurl ? "正在重试" : "使用 cURL 重试"}
                </button>
              </div>
              <p className="curl-warning">你提供的 <code>avatar_content</code> 请求不含推文视频数据，请复制上面两类“推文详情”请求。请勿把 cURL 分享给其他人。</p>
            </section>
          )}

          {isParsing && (
            <section className="analysis-state" aria-live="polite">
              <div className="analysis-icon"><Film size={22} /></div>
              <div><strong>正在读取视频信息</strong><p>获取标题、封面和可用清晰度…</p></div>
              <div className="analysis-dots"><i /><i /><i /></div>
            </section>
          )}

          {video && !isParsing && (
            <section className="result-section" aria-live="polite">
              <div className="section-heading"><span><Check size={15} />解析完成</span><button type="button" onClick={reset}><RotateCcw size={14} />解析新链接</button></div>
              <article className="result-card">
                <div className="cover-wrap">
                  {video.cover ? <img src={video.cover} alt="视频封面" /> : <div className="cover-placeholder"><Film size={30} /></div>}
                  <span className="duration"><Play size={11} fill="currentColor" />{formatDuration(video.duration)}</span>
                </div>
                <div className="video-info">
                  <div className="platform-row"><span className={`platform-badge ${video.platform.startsWith("X") ? "x" : ""}`}>{video.platform}</span><code>{video.bvid}</code></div>
                  <h2>{video.title}</h2>
                  <p className="author">{video.platform.startsWith("X") ? "发布者" : "UP 主"} · {video.owner}{video.pageTitle !== video.title && ` · ${video.pageTitle}`}</p>

                  <div className="format-row">
                    <label>
                      <span>下载画质</span>
                      <div className="select-wrap">
                        <select
                          value={video.qualityId}
                          onChange={(event) => void changeQuality(Number(event.target.value))}
                          disabled={isChangingQuality || isDownloading}
                        >
                          {video.qualities.map((quality) => <option value={quality.id} key={quality.id}>{quality.label}</option>)}
                        </select>
                        {isChangingQuality ? <LoaderCircle className="spin" size={15} /> : <ChevronDown size={15} />}
                      </div>
                    </label>
                    <div className="size-block"><span>预计大小</span><strong>{formatBytes(video.estimatedSize)}</strong></div>
                    <div className="size-block"><span>传输方式</span><strong>6 线程分片</strong></div>
                  </div>
                </div>
              </article>

              <article className="direct-card">
                <div className="direct-header">
                  <div><Link2 size={17} /><div><strong>已生成临时直链</strong><span>{expiryText} · 需要来源请求头</span></div></div>
                  <span className="live-dot"><i />可用</span>
                </div>
                <div className="direct-row"><input readOnly value={video.directUrl} aria-label="生成的临时直链" /><button type="button" onClick={() => void copyDirectLink()}><Copy size={15} />复制</button></div>
                <button className="details-toggle" type="button" onClick={() => setShowDetails((value) => !value)}>
                  查看链接详情 <ChevronDown size={14} className={showDetails ? "open" : ""} />
                </button>
                {showDetails && (
                  <div className="details-panel">
                    <span>格式 <strong>{video.format.toUpperCase()}</strong></span>
                    <span>媒体段 <strong>{video.parts.length}</strong></span>
                    <span>网络分片 <strong>{video.estimatedSize ? Math.ceil(video.estimatedSize / (4 * 1024 * 1024)) : "下载时检测"}</strong></span>
                  </div>
                )}
              </article>

              {progress && (
                <article className="progress-card">
                  <div className="progress-copy">
                    <div className="progress-icon">{progress.status === "completed" ? <Check size={19} /> : <ArrowDownToLine size={19} />}</div>
                    <div><strong>{progress.status === "completed" ? "下载完成" : "正在分片下载"}</strong><span>{formatBytes(progress.downloaded)} / {formatBytes(progress.total)} · {formatBytes(progress.speedBytes)}/s</span></div>
                    <b>{Math.round(progress.percent)}%</b>
                  </div>
                  <div className="progress-track"><i style={{ width: `${progress.percent}%` }} /></div>
                </article>
              )}

              <button className="download-button" type="button" onClick={() => void startDownload()} disabled={isDownloading || isChangingQuality}>
                {isDownloading ? <LoaderCircle className="spin" size={19} /> : <Download size={19} />}
                {isDownloading ? "正在下载，请稍候" : "选择位置并下载"}
              </button>
              <p className="rights-note">请仅下载你拥有版权或已获授权保存的内容。</p>
            </section>
          )}

          <section className="downloads-section" id="downloads">
            <div className="section-title"><div><History size={18} /><div><h3>最近下载</h3><p>仅保存在这台设备上</p></div></div>{history.length > 0 && <button type="button" onClick={() => setHistory([])}>清空记录</button>}</div>
            {history.length === 0 ? (
              <div className="empty-history"><Clock3 size={20} /><span>下载完成的视频会出现在这里</span></div>
            ) : (
              <div className="history-list">
                {history.map((item) => (
                  <div className="history-item" key={item.id}>
                    <img src={item.cover} alt="" />
                    <div><strong>{item.title}</strong><span>{new Date(item.createdAt).toLocaleString("zh-CN")} · {formatBytes(item.size)}</span></div>
                    <Check size={17} />
                  </div>
                ))}
              </div>
            )}
          </section>

          <footer><span><Gauge size={14} />4 MB 分片 · 最多 6 路并发</span><span>本地处理 · 不上传视频</span></footer>
        </div>
        </main>
      </div>
    </>
  );
}

export default App;
