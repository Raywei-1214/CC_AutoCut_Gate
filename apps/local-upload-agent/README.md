# 创剪本地上传助手（Tauri 安装版）

这是安装版本地上传助手的工程骨架，目标平台：

- macOS
- Windows

当前已经具备这些可用能力：

- `apps/local-upload-agent` 独立工作区
- Tauri 桌面壳
- Rust 侧 `127.0.0.1:17777` 本地接口
- 原生文件选择器
- 上传任务创建 / 查询 / 取消
- 4 路并发分片上传到 Zeabur
- 已支持 GCloud 助手模式：本地助手后台调用 `gcloud storage cp` 上传到 GCS，并自动导入为站内 asset
- 分片失败重试与远端状态回查
- 默认后台常驻、登录后自动启动、关闭即隐藏
- 任务列表、查看日志、导出日志
- 内置自动更新检查 / 安装入口
- 状态面板 UI
- 根目录快捷脚本：
  - `pnpm agent:desktop:dev`
  - `pnpm agent:desktop:check`
  - `pnpm agent:desktop:build`
  - `pnpm agent:desktop:build:debug`

当前还未产品化收尾的部分：

- 真正的安装包签名
- GitHub Release 首次密钥注入与发布

## 本地开发

前置条件：

- Node.js / pnpm
- Rust toolchain
- Tauri 运行环境

启动：

```bash
pnpm install
bash ./scripts/local-upload-agent-desktop-dev.sh
```

检查：

```bash
bash ./scripts/local-upload-agent-desktop-check.sh
```

打包：

```bash
bash ./scripts/local-upload-agent-desktop-build.sh
```

调试包：

```bash
bash ./scripts/local-upload-agent-desktop-build-debug.sh
```

CI 构建：

- GitHub Actions: [local-upload-agent-desktop.yml](/Users/yanwei/CC_AutoCut/.github/workflows/local-upload-agent-desktop.yml)
- Release / 自动更新： [local-upload-agent-desktop-release.yml](/Users/yanwei/CC_AutoCut/.github/workflows/local-upload-agent-desktop-release.yml)
- Secrets 清单： [local-upload-agent-release-secrets.md](/Users/yanwei/CC_AutoCut/docs/agent/solutions/local-upload-agent-release-secrets.md)
- 一键准备命令：`pnpm agent:desktop:release:setup`
- GCS 静态托管发布：`pnpm agent:desktop:publish:gcs`
- 自动产出：
  - macOS `portable zip`
  - macOS `.dmg`
  - Windows `setup.exe`
  - Windows `.msi`
- 约定 tag：
  - `local-upload-agent-v0.1.0`
  - 推 tag 后会自动跑内测发版并上传 macOS `portable zip` / `.dmg` 与 Windows `setup.exe` / `.msi` 资产

当前仓库如果在 `Releases` 页面里还是空的，说明还没完成第一次正式发版；这时不会有可下载的 macOS `portable zip` / `.dmg` 或 Windows `setup.exe` / `.msi` 资产。

当前默认发版流已切到“无签名内测模式”：

- 不导入 Apple / Windows 证书
- 不生成自动更新用的 `latest.json`
- 产出并上传 macOS `portable zip` / `.dmg` 与 Windows `setup.exe` / `.msi`
- 适合先把安装包放到 GitHub Release 给内测使用

## 使用方式

- 安装完成后，助手默认以后台常驻模式运行，不会每次都主动弹出主窗口
- 用户需要时可从：
  - Windows 系统托盘
  - macOS 菜单栏 / 状态栏
  打开主窗口查看状态
- 登录系统后会自动启动助手，网页可直接检测 `127.0.0.1:17777`
- 如果需要手动打开调试面板，可通过托盘菜单选择“显示窗口”

如果自动更新改走 GCS 静态托管而不是 GitHub Release：

- 构建时必须把 `CHUANGCUT_AGENT_UPDATER_ENDPOINT` 指向真实可访问的 `latest.json`
- 可以用 `pnpm agent:desktop:publish:gcs` 把 `.app.tar.gz`、`.msi.zip`、签名和 `latest.json` 一起上传
- Windows 自动更新读取的是 `.msi.zip`，不是 `.msi`

## 发布所需 Secrets

### 自动更新

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
- `TAURI_UPDATER_PUBLIC_KEY`

### macOS 签名 / 公证

- `APPLE_CERTIFICATE`
- `APPLE_CERTIFICATE_PASSWORD`
- `KEYCHAIN_PASSWORD`
- `APPLE_ID`
- `APPLE_PASSWORD`
- `APPLE_TEAM_ID`
- `APPLE_SIGNING_IDENTITY`

### Windows 签名

- `WINDOWS_CERTIFICATE`
- `WINDOWS_CERTIFICATE_PASSWORD`
- `WINDOWS_CERTIFICATE_THUMBPRINT`
- `WINDOWS_DIGEST_ALGORITHM`
- `WINDOWS_TIMESTAMP_URL`

## 迁移路线

建议按这个顺序继续：

1. 补齐 GitHub Secrets
2. 推 `local-upload-agent-v*` tag 跑正式签名发布
3. 视需要补更完整的托盘任务面板
