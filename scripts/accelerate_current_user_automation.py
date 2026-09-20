from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shutil
import sqlite3
from pathlib import Path


MARKER = "<!-- jev-acceleration-v1 -->"

BROWSER_SKILLS = {
    "领英发动态",
    "头条微头条发布",
    "browser-react-controlled-interaction",
    "reddit-browse-home-feed",
    "reddit-to-toutiao-weitoutiao",
    "toutiao-comment-manage-zh",
    "toutiao-publish-zh",
    "toutiao-style-by-data",
    "x发帖防幽灵文本",
    "xianyu-goofish-publish",
    "xianyu-goofish-seller-publish",
    "后台功能梳理法",
}

CLI_SKILLS = {
    "aliyun-cli-control",
    "aliyun-free-cert-apply-download-zh",
    "daily-report-feishu-base",
    "feishu-task-table-ops",
    "lark-cli-feishu-control",
    "oss-domain-cert-renew-zh",
}

BROWSER_BLOCK = f"""
{MARKER}
## Jey 智能加速

- 连续两个以上的普通、低风险页面步骤（导航、展开、滚动、查找、填写非敏感文本）优先合并为一次 `browser_fast_loop`，给出明确 `goal`；用户已经提供的普通字段可放进 `inputs`。
- `browser_fast_loop` 返回 `need_main` 时，主模型读取当前快照；需要时按本技能原有方法用 `browser_evaluate`、截图或专用选择器补足页面证据，然后再次调用快速循环。不要因为一次低置信度就永久改成主模型逐步点击。
- 本技能中注明必须使用 Shadow DOM、React/Vue 受控组件、ProseMirror、模拟粘贴、文件输入框或特定 JavaScript 的步骤，继续使用原有方案；Jey 快速循环不能替代这些机制。
- 发布、发送、评论、删除、付款、申请、覆盖数据等敏感或不可逆动作不进入快速循环。执行前由主模型核对目标、内容、按钮状态和授权，执行后读回页面证据确认结果。
- 已知准确 URL、一次性 `browser_evaluate`/`browser_read` 能直接完成的步骤直接执行即可；不要为了使用 Jey 增加额外往返。
""".strip()

CLI_BLOCK = f"""
{MARKER}
## 快速、低上下文调用

- 外部 CLI 优先使用 `run_command`/`run_command_background` 的 `argv`，程序名和每个参数各占一项；不要把整条命令拼成字符串。结构化长输入放 `stdin` 或 `files`。
- 要求 CLI 输出 JSON；让工具自动解析，并用 `json_pointer` 只返回当前决策需要的子集。不要把完整列表、日志或凭证重新灌入对话。
- 安装、批处理、等待签发、轮询远端状态等耗时步骤使用 `run_command_background` 且 `await=true`，或随后使用 `await_task`；不要用 `check_task` 消耗轮次轮询。
- 旧正文中若要求为普通 CLI 调用先创建 `.ps1`、再套 `powershell -Command`，以本节为准：只有确实需要 PowerShell 对象、管道或循环时才使用 `command` + `shell=powershell`，动态值放 `script_args`。
- Jey 当前不负责自由生成 CLI 命令。主模型或本技能给出确定命令；尽量把结果整理为少量明确状态（完成、继续等待、修正参数、需要主模型），为后续有限决策加速保留结构。
""".strip()

WORKFLOW_ACCELERATION = """【Jey 智能加速规则】
- 已知准确 URL、一次性 browser_evaluate/browser_read 能完成的步骤直接执行，不为使用 Jey 增加往返。
- 遇到连续两个以上普通、低风险的页面交互，优先调用 browser_fast_loop，并给出明确目标及用户已提供的非敏感 inputs。
- browser_fast_loop 返回 need_main 时，主模型读取当前页面并补充候选或特殊控件处理，然后再次交给 Jey；不要直接退化成全程逐步点击。
- 发布、发送、删除、覆盖技能等敏感动作由主模型在核对内容和授权后执行；Jey只辅助到达页面和处理低风险步骤。
"""


def insert_after_frontmatter(text: str, block: str) -> str:
    if MARKER in text:
        return text
    if text.startswith("---"):
        end = text.find("\n---", 3)
        if end >= 0:
            cut = end + 4
            return text[:cut] + "\n\n" + block + "\n\n" + text[cut:].lstrip("\n")
    return block + "\n\n" + text


def backup(db_path: Path, skills_dir: Path, backup_root: Path) -> Path:
    stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    target = backup_root / f"jev-acceleration-{stamp}"
    target.mkdir(parents=True, exist_ok=False)
    if skills_dir.is_dir():
        shutil.copytree(skills_dir, target / "skills")
    source = sqlite3.connect(db_path)
    destination = sqlite3.connect(target / "deskhelper.db")
    try:
        source.backup(destination)
    finally:
        destination.close()
        source.close()
    return target


def update_skills(skills_dir: Path) -> list[str]:
    changed: list[str] = []
    for name, block in [(name, BROWSER_BLOCK) for name in BROWSER_SKILLS] + [
        (name, CLI_BLOCK) for name in CLI_SKILLS
    ]:
        path = skills_dir / name / "SKILL.md"
        if not path.is_file():
            continue
        old = path.read_text(encoding="utf-8")
        new = insert_after_frontmatter(old, block)
        if new != old:
            path.write_text(new, encoding="utf-8", newline="\n")
            changed.append(name)
    return changed


def update_workflows(db_path: Path) -> list[int]:
    con = sqlite3.connect(db_path, timeout=30)
    changed: list[int] = []
    try:
        con.execute("begin immediate")
        rows = con.execute("select id, prompt from automation_workflows").fetchall()
        for workflow_id, prompt in rows:
            if "【Jey 智能加速规则】" in prompt:
                continue
            con.execute(
                "update automation_workflows set prompt=?, updated_at=datetime('now','localtime') where id=?",
                (WORKFLOW_ACCELERATION + "\n" + prompt, workflow_id),
            )
            changed.append(workflow_id)
        con.commit()
    except Exception:
        con.rollback()
        raise
    finally:
        con.close()
    return changed


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--app-dir", required=True)
    args = parser.parse_args()
    app_dir = Path(args.app_dir).resolve()
    db_path = app_dir / "deskhelper.db"
    skills_dir = app_dir / "skills"
    backup_dir = backup(db_path, skills_dir, app_dir / "file_backups")
    skills = update_skills(skills_dir)
    workflows = update_workflows(db_path)
    print(json.dumps({
        "backup": str(backup_dir),
        "updated_skills": skills,
        "updated_workflow_ids": workflows,
    }, ensure_ascii=False))


if __name__ == "__main__":
    main()
