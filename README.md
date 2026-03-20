# 创剪本地上传助手调试仓库

这是从主仓裁出来的最小公有调试仓库，只保留桌面助手打包、启动和发版所需文件。

## 保留内容

- `apps/local-upload-agent`
- `.github/workflows/local-upload-agent-desktop.yml`
- `.github/workflows/local-upload-agent-desktop-release.yml`
- `scripts/local-upload-agent-desktop-*`

## 使用方式

```bash
pnpm install
pnpm agent:desktop:check
pnpm agent:desktop:build
```

## 发布

- 推送 tag：`local-upload-agent-v0.1.0`
- 会触发 GitHub Actions 构建 macOS / Windows 产物

## 注意

- 这个仓库用于公开调试桌面助手问题，不要放主业务代码和任何凭据
- 公开前请只保留桌面助手相关文件
