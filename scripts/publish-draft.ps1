# 上传草稿 Release：统一把 NSIS 安装包改成 ASCII 文件名再上传。
# 背景：productName 是「拾光」，Tauri 产出的安装包叫「拾光_0.1.8_x64-setup.exe」，
# 上传到 GitHub 后中文字符会被丢弃，变成「_0.1.8_x64-setup.exe」，下载者看到会很困惑。
# 此脚本只改上传用的副本名，不影响安装目录、开始菜单名称等展示。
#
# 用法：
#   pwsh scripts/publish-draft.ps1                       # 自动读 package.json 版本
#   pwsh scripts/publish-draft.ps1 -NotesFile notes.md   # 指定发布说明
#
# 脚本只创建草稿，不会公开 Release；签名由 release.yml 完成。

[CmdletBinding()]
param(
  [string]$Version = '',
  [string]$NotesFile = '',
  [string]$Branch = 'main'
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

if (-not $Version) {
  $Version = (Get-Content package.json -Raw | ConvertFrom-Json).version
}
$confVersion = (Get-Content src-tauri/tauri.conf.json -Raw | ConvertFrom-Json).version
if ($confVersion -ne $Version) {
  throw "版本不一致：package.json=$Version，tauri.conf.json=$confVersion"
}

$tag = "app-v$Version"

$installers = @(
  Get-ChildItem 'src-tauri/target/release/bundle/nsis' -Filter '*-setup.exe' -File -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending
)
if ($installers.Count -eq 0) {
  throw '找不到安装包，请先运行 npm run release:local'
}
$source = $installers[0]

$targetName = "ShiGuang_${Version}_x64-setup.exe"
$staging = Join-Path $repoRoot 'src-tauri/target/release/bundle/nsis/upload'
New-Item -ItemType Directory -Force $staging | Out-Null
$target = Join-Path $staging $targetName
Copy-Item $source.FullName $target -Force

$hash = (Get-FileHash $target -Algorithm SHA256).Hash.ToLower()

git rev-parse -q --verify "refs/tags/$tag" | Out-Null
if ($LASTEXITCODE -ne 0) {
  throw "本地不存在标签 $tag，请先创建标签并推送"
}
git ls-remote --exit-code --tags origin $tag | Out-Null
if ($LASTEXITCODE -ne 0) {
  throw "标签 $tag 还没推送到 origin"
}
$tagCommit = (git rev-list -n 1 $tag).Trim()
$headCommit = (git rev-parse HEAD).Trim()
if ($tagCommit -ne $headCommit) {
  Write-Warning "标签 $tag 指向 $tagCommit，当前 HEAD 是 $headCommit；确认安装包确实对应标签提交后再继续"
}

$ghArgs = @('release', 'create', $tag, $target, '--draft', '--verify-tag', '--title', "拾光 v$Version")
if ($NotesFile) {
  $ghArgs += @('--notes-file', $NotesFile)
}
& gh @ghArgs
if ($LASTEXITCODE -ne 0) {
  throw '创建草稿 Release 失败'
}

Write-Host ''
Write-Host "安装包：$target"
Write-Host "SHA256：$hash"
Write-Host ''
Write-Host '下一步（签名并公开）：'
Write-Host "gh workflow run release.yml --ref $Branch -f mode=sign-local -f tag=$tag -f sha256=$hash"
