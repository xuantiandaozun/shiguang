# 本地构建并发布

默认不再通过推送标签启动 GitHub 全量编译。GitHub 仅使用仓库 Secret 对本地上传的安装包签名，并生成 `latest.json`，保持设置中的自动更新可用。无需把签名私钥下载到本机。

1. 同步修改 `package.json` / `package-lock.json` / `src-tauri/tauri.conf.json` / `src-tauri/Cargo.toml` / `src-tauri/Cargo.lock` 的应用版本。
2. 运行 `npm run release:local`。安装包在 `src-tauri/target/release/bundle/nsis/`。此配置不生成本地更新签名，因此无需本机私钥。
3. 检查变更、测试，提交并推送源码；创建 `app-v版本号` 标签并推送。标签应指向此安装包对应的源码提交。
4. 运行 `pwsh scripts/publish-draft.ps1 -NotesFile 发布说明路径`。脚本会把安装包复制成 `ShiGuang_版本_x64-setup.exe` 再上传草稿（Tauri 原始文件名带中文「拾光_」，GitHub 上传时会丢弃中文字符，变成 `_0.1.8_x64-setup.exe`，下载的人会以为文件是坏的），同时打印 SHA256。它只改上传副本名，不影响安装目录和开始菜单标题。
5. 复制脚本输出的 `gh workflow run release.yml --ref 源码分支 -f mode=sign-local -f tag=app-v版本号 -f sha256=哈希` 并执行。
6. 工作流会检查版本、标签提交、草稿状态和 SHA256，然后签名，上传 `.sig` 和 `latest.json`，最后公开 Release；安装包文件名含非 ASCII 字符时会直接报错拒绝继续。
7. 确认工作流成功，检查 Release 的安装包、签名和 `latest.json` 均存在。

备用：在 Actions 的「发布 Windows 安装包」手动选择 `mode=build`，可用 GitHub 完整编译并发布。

签名 Secret 仍使用 `TAURI_SIGNING_PRIVATE_KEY`、`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`，不要提交私钥或输出密钥内容。更新签名不是 Windows Authenticode，未购买代码签名证书时 Windows 仍可能显示未知发布者。
