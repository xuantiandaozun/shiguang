// All completion runs inside the actual interactive Shell, so cd, quoting and
// filenames stay server-side. No SFTP directory or client-side guessed cwd.
fn terminal_completion_bash_major(bytes: &[u8]) -> Option<u32> {
    String::from_utf8_lossy(bytes).lines().find_map(|line| {
        line.trim().strip_prefix("__SHIGUANG_BASH_MAJOR__")?.parse::<u32>().ok()
    })
}

fn terminal_completion_probe(session: &Session) -> (bool, &'static str) {
    session.set_timeout(1500);
    let result = (|| -> Result<Option<u32>> {
        let mut probe = session.channel_session()?;
        probe.exec("printf '\\n__SHIGUANG_BASH_MAJOR__%s\\n' \"${BASH_VERSINFO[0]-0}\"")?;
        let mut bytes = Vec::new();
        (&mut probe).take(4096).read_to_end(&mut bytes)?;
        let _ = probe.close();
        Ok(terminal_completion_bash_major(&bytes))
    })();
    session.set_timeout(10000);
    match result {
        Ok(Some(major)) if major >= 4 => (true, "checking"),
        Ok(_) => (false, "unsupported"),
        Err(error) => {
            log::warn!("终端补全 Shell 探测失败，保留原生补全: {error}");
            (false, "probe_failed")
        }
    }
}

fn terminal_completion_bootstrap(marker: &str) -> String {
    let quoted = quote(include_str!("terminal_completion.bash"));
    // Keep physical lines short: a PTY may still be in canonical mode during login
    // and Linux can discard input past its roughly 4 KiB per-line limit. Bash reads
    // this as one quoted multiline argument, then evaluates it in the current Shell.
    format!(" builtin eval {quoted}; __shiguang_setup_completion '{marker}'; builtin unset -f __shiguang_setup_completion; builtin printf '\\n{marker}:%s\\n' \"${{__shiguang_completion_status-unsupported}}\"\n")
}

fn terminal_completion_ready_output(bytes: &[u8], marker: &str) -> Option<(Vec<u8>, String)> {
    let prefix = format!("\n{marker}:");
    let start = bytes.windows(prefix.len()).position(|window| window == prefix.as_bytes())? + prefix.len();
    let end = bytes[start..].iter().position(|&byte| byte == b'\n')? + start;
    let status = std::str::from_utf8(&bytes[start..end]).ok()?.trim_end_matches('\r');
    if !["initial", "readline", "custom", "unsupported", "error"].contains(&status) { return None; }
    Some((bytes[end + 1..].to_vec(), status.to_string()))
}

fn terminal_read_completion_startup(channel: &mut ssh2::Channel, marker: &str) -> Result<(Vec<u8>, String)> {
    let started = Instant::now();
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0u8; 16384];
        match channel.read(&mut buffer) {
            Ok(n) if n > 0 => {
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(result) = terminal_completion_ready_output(&bytes, marker) { return Ok(result); }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
            Err(e) => return Err(e.into()),
            _ => (),
        }
        if channel.eof() { bail!("服务器在初始化交互终端时退出"); }
        if started.elapsed() > Duration::from_secs(4) || bytes.len() > 131072 {
            log::warn!("终端增强补全未收到就绪标记，保留原始输出");
            return Ok((bytes, "timeout".to_string()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod terminal_completion_tests {
    use super::*;
    #[test]
    fn detects_bash_four_and_five_without_requiring_initial_word_support() {
        assert_eq!(terminal_completion_bash_major(b"\n__SHIGUANG_BASH_MAJOR__4\n"), Some(4));
        assert_eq!(terminal_completion_bash_major(b"\r\n__SHIGUANG_BASH_MAJOR__5\r\n"), Some(5));
        assert_eq!(terminal_completion_bash_major(b"other shell"), None);
    }
    #[test]
    fn bootstrap_has_short_pty_lines_and_does_not_modify_startup_files() {
        let command = terminal_completion_bootstrap("__READY_TEST__");
        assert!(command.lines().count() > 1);
        assert!(command.lines().all(|line| line.len() < 2048));
        assert!(command.contains("__shiguang_setup_completion '__READY_TEST__'"));
        assert!(command.contains("${__shiguang_completion_status-unsupported}"));
        assert!(!command.contains(".bashrc"));
        assert!(!command.contains(".profile"));
        assert!(command.starts_with(' '));
    }
    #[test]
    fn echoed_or_partial_markers_do_not_signal_readiness() {
        let echo = b"builtin printf '\\n__READY_TEST__:%s\\n'\r\n";
        assert!(terminal_completion_ready_output(echo, "__READY_TEST__").is_none());
        let mut output = echo.to_vec();
        output.extend_from_slice(b"\r\n__READY_TEST__:readline\r");
        assert!(terminal_completion_ready_output(&output, "__READY_TEST__").is_none());
        output.extend_from_slice(b"\nroot@host:~# ");
        let ready = terminal_completion_ready_output(&output, "__READY_TEST__").unwrap();
        assert_eq!(ready.0, b"root@host:~# ");
        assert_eq!(ready.1, "readline");
        assert_eq!(terminal_completion_ready_output(b"\n__READY_TEST__:initial\nprompt", "__READY_TEST__").unwrap().0, b"prompt");
        assert!(terminal_completion_ready_output(b"\n__READY_TEST__:unknown\n", "__READY_TEST__").is_none());
    }
}
