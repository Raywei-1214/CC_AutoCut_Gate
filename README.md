# 创剪本地上传助手 Gate 仓库

这是从主仓裁出来的最小公有发版仓库，只保留桌面助手打包、启动和 GitHub Release 所需文件。

## 生成方式

请在主仓执行：

```bash
pnpm agent:desktop:sync:gate
```

## 保留内容

- `apps/local-upload-agent`
- `.github/workflows/local-upload-agent-desktop.yml`
- `.github/workflows/local-upload-agent-desktop-release.yml`
- `scripts/local-upload-agent-desktop-*`
- 最小根配置：`package.json` / `pnpm-lock.yaml` / `pnpm-workspace.yaml` / `biome.json`

## 发布流程

1. 在主仓完成桌面助手改动
2. 回到主仓执行 `pnpm agent:desktop:sync:gate`
3. 进入 Gate 仓库检查差异并提交推送
4. 在 Gate 仓库打 tag：`local-upload-agent-v0.1.0`

## 注意

- 不要把主业务代码和任何凭据放进这个仓库
- 这个仓库只负责桌面助手安装包构建与发版
