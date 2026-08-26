# Ying-Liu（映流）

一款使用 Tauri 2、React 19、TypeScript 和 Rust 构建的本地视频链接解析与分片下载工具。

## 当前能力

- 解析 Bilibili BV 视频链接和 `b23.tv` 短链
- 展示标题、封面、UP 主、时长、清晰度与文件大小
- 生成带有效期的媒体直链，并支持复制
- Rust 后端按 4 MB 分片、最多 6 路并行下载
- 自选保存位置、实时进度与本地下载记录
- macOS DMG / `.app` 和 Windows NSIS Setup / 便携 `.exe` 构建流程

> 仅应用于你拥有版权或已获授权保存的内容。可用画质、登录要求和地区限制由视频平台决定；媒体直链通常会在数小时后失效。

## 本地开发

需要 Node.js 20+、pnpm、Rust stable，以及 [Tauri 2 系统依赖](https://v2.tauri.app/start/prerequisites/)。

```bash
pnpm install
pnpm tauri dev
```

仅构建前端：

```bash
pnpm build
```

## 打包与 GitHub Release

macOS 必须在 macOS 上构建：

```bash
pnpm build:mac
```

产物包括 DMG 安装版和 `.app` 应用包；将 `.app` 压缩后即可作为免安装便携版。

Windows 推荐在 Windows 或项目自带的 GitHub Actions 工作流中构建：

```powershell
pnpm build:windows
Compress-Archive -Path src-tauri\target\release\ying-liu.exe -DestinationPath Ying-Liu-windows-portable.zip
```

NSIS 会生成 `*-setup.exe` 安装包，Rust release 可执行文件会被打包成绿色便携版。便携版依赖 Windows 10/11 自带的 WebView2；安装版会在缺少时下载 WebView2 引导程序。

项目在推送 `v*` 标签时会自动构建 macOS ARM64、macOS Intel 和 Windows x64，并把安装版、便携版统一上传到 GitHub Releases：

```bash
# 同步 package.json、Cargo.toml、tauri.conf.json 版本，测试、提交并创建标签
pnpm release:tag 0.2.0

# 推送 master 和标签；标签会触发 GitHub Release
pnpm release:push
```

也可以在 GitHub 的 **Actions → Release desktop packages → Run workflow** 手动构建。手动构建不会创建 Release，产物可在对应 Actions 运行页面下载。

## 实现说明

解析端调用 Bilibili 公开网页接口获取视频信息和音画合一的渐进式媒体地址。下载请求由 Rust 发起，以便携带来源请求头、绕开 WebView 跨域限制并稳定写入大文件。多媒体段会按顺序写入同一个目标文件。

当前版本默认处理视频的第一个分 P，不读取浏览器 Cookie，因此登录专享、会员、版权或地区受限画质不会绕过平台限制。
