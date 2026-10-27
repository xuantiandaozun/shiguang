"""Test the real session-only Bash completion, without connecting to a server."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def find_bash():
    explicit = os.environ.get("TERMINAL_TEST_BASH")
    if explicit:
        return explicit
    candidates = []
    if os.name == "nt":
        git = shutil.which("git")
        if git:
            candidates.append(Path(git).resolve().parent.parent / "bin/bash.exe")
        candidates.extend([Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "Git/bin/bash.exe", Path("D:/Git/bin/bash.exe")])
    else:
        found = shutil.which("bash")
        if found:
            candidates.append(Path(found))
    for candidate in candidates:
        if candidate.is_file():
            return str(candidate)
    raise RuntimeError("Bash 4+ is required; set TERMINAL_TEST_BASH to its path")


bash = find_bash()
source = (Path(__file__).resolve().parent.parent / "src-tauri/src/terminal_completion.bash").read_text(encoding="utf-8")


def run(body, directory):
    result = subprocess.run([bash, "--noprofile", "--norc", "-c", source + "\ncd -- \"$1\"\n" + body, "test", Path(directory).as_posix()], capture_output=True, timeout=15)
    if result.returncode:
        raise AssertionError(result.stderr.decode("utf-8", errors="replace"))
    return result.stdout


def complete(prefix, directory):
    # Prefix is test data passed as an argv item, never interpolated as shell code.
    result = subprocess.run([bash, "--noprofile", "--norc", "-c", source + '\ncd -- "$1"\nCOMP_WORDS=("$2"); COMP_CWORD=0; __shiguang_complete_local_command; printf "%s\\0" "${COMPREPLY[@]}"', "test", Path(directory).as_posix(), prefix], capture_output=True, timeout=15, check=True)
    return [value.decode("utf-8") for value in result.stdout.split(b"\0") if value]


with tempfile.TemporaryDirectory(prefix="shiguang-completion-") as temp:
    root = Path(temp)
    (root / "ruoyi-proxy-linux-spoke").write_text('#!/bin/sh\nprintf ran > "./EXECUTED_BY_COMPLETION"\n', encoding="utf-8")
    (root / "sgreport file.txt").write_text("text", encoding="utf-8")
    (root / "space-dir").mkdir()
    (root / "sgdanger;echo").write_text("text", encoding="utf-8")
    (root / "bash-local-file").write_text("text", encoding="utf-8")
    (root / "echo-local-file").write_text("text", encoding="utf-8")
    assert complete("ruo", root) == ["./ruoyi-proxy-linux-spoke"]
    assert complete("sgreport", root) == ["./sgreport file.txt"]
    assert complete("sgdanger", root) == ["./sgdanger;echo"]
    assert complete("space-", root) == ["./space-dir"]
    assert complete("does-not-exist-987654", root) == []
    assert complete("bash", root) == [], "real commands must keep native priority"
    assert complete("./ruo", root) == [], "explicit paths must keep native handling"
    assert complete("", root) == []
    (root / "child").mkdir()
    (root / "child/ruoyi-other").write_text("text", encoding="utf-8")
    assert complete("ruo", root / "child") == ["./ruoyi-other"], "use the actual cwd after cd"
    assert not (root / "EXECUTED_BY_COMPLETION").exists()

    major = int(run('printf "%s" "${BASH_VERSINFO[0]}"', root))
    if major >= 5:
        registered = run('__shiguang_setup_completion __TEST_MARKER__; builtin complete -p -I', root).decode()
        assert "-F __shiguang_complete_local_command" in registered
        assert "-o filenames" in registered and "-o bashdefault" in registered
        existing = run('my_initial_completion() { COMPREPLY=(native); }; builtin complete -I -F my_initial_completion; __shiguang_setup_completion __TEST_MARKER__; COMP_WORDS=(no_native_command_987654); COMP_CWORD=0; __shiguang_complete_local_command; printf "%s" "${COMPREPLY[*]}"', root).decode()
        assert existing == "native", "existing initial-word callbacks must remain usable"
        local_with_existing = run('my_initial_completion() { COMPREPLY=(native); }; builtin complete -I -F my_initial_completion; __shiguang_setup_completion __TEST_MARKER__; COMP_WORDS=(ruo); COMP_CWORD=0; __shiguang_complete_local_command; printf "%s" "${COMPREPLY[*]}"', root).decode()
        assert local_with_existing == "./ruoyi-proxy-linux-spoke"
        unusual = run('builtin complete -I -C "printf special"; __shiguang_setup_completion __TEST_MARKER__; builtin complete -p -I', root).decode()
        assert "-F __shiguang_complete_local_command" not in unusual and "printf special" in unusual
    parameters = run('my_parameter_completion() { COMPREPLY=(native); }; builtin complete -F my_parameter_completion cat; __shiguang_setup_completion __TEST_MARKER__; builtin complete -p cat', root).decode()
    assert "-F my_parameter_completion" in parameters
    history = run('set +o history; builtin history -c; builtin history -s "user_original_command"; builtin history -s "our_bootstrap___TEST_MARKER__"; __shiguang_setup_completion __TEST_MARKER__; builtin history', root).decode()
    assert "user_original_command" in history and "our_bootstrap" not in history

    custom_tab = run('builtin bind \'"\\C-i": "USER_TAB_MACRO"\' 2>/dev/null; __shiguang_setup_readline_completion; printf "%s\\n" "$__shiguang_completion_status"; builtin bind -s 2>/dev/null', root).decode()
    assert "custom" in custom_tab and "USER_TAB_MACRO" in custom_tab

    # Exercise the multiline quoted bootstrap used over SSH. Keep physical lines
    # below the PTY canonical-input limit. Ctrl+U clears input without executing it.
    quoted = "'" + source.replace("'", "'\"'\"'") + "'"
    environment = {**os.environ, "HISTFILE": (root / "isolated-history").as_posix(), "PS1": "SG_TEST> ", "TERM": "xterm-256color"}
    setups = {
        "automatic": "__shiguang_setup_completion '__READY_INTERACTIVE__'",
        "readline-emacs": "__shiguang_setup_readline_completion",
        "readline-vi": "__shiguang_setup_readline_completion",
    }
    for mode, setup in setups.items():
        bootstrap = " builtin eval " + quoted + "; " + setup + "; builtin printf '\\n__READY_INTERACTIVE__:%s\\n' \"$__shiguang_completion_status\"\n"
        assert max(len(line.encode()) for line in bootstrap.splitlines()) < 2048
        mode_prefix = "set -o vi\n" if mode == "readline-vi" else ""
        interactive = subprocess.run([bash, "--noprofile", "--norc", "-i"], input=(mode_prefix + bootstrap + "ruo\t\x15sgreport\t\x15sgdanger\t\x15ech\t\x15cat ruo\t\x15cd child\nruo\t\x15exit\n").encode(), cwd=temp, env=environment, capture_output=True, timeout=20, check=True)
        transcript = (interactive.stdout + interactive.stderr).decode("utf-8", errors="replace")
        assert "./ruoyi-proxy-linux-spoke" in transcript, mode
        assert "./ruoyi-other" in transcript, mode
        assert r"./sgreport\ file.txt" in transcript, mode
        assert r"./sgdanger\;echo" in transcript, mode
        assert "cat ruoyi-proxy-linux-spoke" in transcript, mode
        assert "./echo-local-file" not in transcript, "native commands must win over local files"
        assert "__READY_INTERACTIVE__" in interactive.stdout.decode(), mode
        if mode.startswith("readline") or major < 5:
            assert "__READY_INTERACTIVE__:readline" in interactive.stdout.decode(), mode
        assert not (root / "EXECUTED_BY_COMPLETION").exists()
        assert not (root / "child/EXECUTED_BY_COMPLETION").exists()
        if mode == "automatic" and (root / "isolated-history").exists():
            assert "__shiguang" not in (root / "isolated-history").read_text(encoding="utf-8", errors="replace"), "bootstrap must not pollute user history"

print(f"Bash {major} completion: ./ prefix, actual cwd, escaping, native command/argument completion, legacy Emacs/Vi readline, custom bindings and history preservation all passed.")
