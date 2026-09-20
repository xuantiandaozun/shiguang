# 发布与手动更新

拾光只会在用户打开设置并点击“检查更新”时访问 GitHub Releases，不会在后台检查，也不会强制更新。

## 首次配置 GitHub

在仓库 `xuantiandaozun/shiguang` 的 **Settings → Secrets and variables → Actions** 中添加：

- `TAURI_SIGNING_PRIVATE_KEY`：`C:\Users\user\.tauri\shiguang-updater.key` 的完整内容。
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`：`C:\Users\user\.tauri\shiguang-updater-password.txt` 的完整内容。

私钥和密码不得提交到 Git。它们丢失后，已经安装的旧版本将无法验证新更新包，应另行离线备份。

## 发布新版本

1. 同步修改 `package.json`、`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json` 中的版本号，使用语义化版本，例如 `0.1.3`。
2. 完成检查并提交代码。
3. 创建并推送标签：`git tag app-v0.1.3`、`git push origin app-v0.1.3`。
4. GitHub Actions 会构建 NSIS 安装包、签名更新包，并发布 Release 与 `latest.json`。

也可以在 GitHub 的 Actions 页面手动运行“发布 Windows 安装包”，但同一版本只能发布一次。
