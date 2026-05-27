pub fn parse_byte_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty size".to_string());
    }

    let (num_str, multiplier) = if s.ends_with('K') || s.ends_with('k') {
        (&s[..s.len() - 1], 1024u64)
    } else if s.ends_with('M') || s.ends_with('m') {
        (&s[..s.len() - 1], 1024 * 1024)
    } else if s.ends_with('G') || s.ends_with('g') {
        (&s[..s.len() - 1], 1024 * 1024 * 1024)
    } else {
        (s, 1u64)
    };

    let num: f64 = num_str
        .parse()
        .map_err(|e| format!("invalid size '{s}': {e}"))?;
    Ok((num * multiplier as f64) as u64)
}

pub(crate) fn copy_to_clipboard(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("nothing to copy".to_string());
    }

    #[cfg(target_os = "macos")]
    {
        copy_with_command("pbcopy", &[], text)
    }

    #[cfg(target_os = "linux")]
    {
        copy_with_command("wl-copy", &[], text)
            .or_else(|_| copy_with_command("xclip", &["-selection", "clipboard"], text))
    }

    #[cfg(target_os = "windows")]
    {
        copy_with_command("clip", &[], text)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = text;
        Err("clipboard command is not available on this platform".to_string())
    }
}

fn copy_with_command(command: &str, args: &[&str], text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{command} failed to start: {e}"))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("{command} write failed: {e}"))?;
    }
    let status = child
        .wait()
        .map_err(|e| format!("{command} wait failed: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{command} exited with status {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_byte_size() {
        assert_eq!(parse_byte_size("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_byte_size("20M").unwrap(), 20 * 1024 * 1024);
        assert_eq!(parse_byte_size("500K").unwrap(), 500 * 1024);
        assert_eq!(parse_byte_size("1G").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_byte_size("1024").unwrap(), 1024);
        assert_eq!(
            parse_byte_size("1.5M").unwrap(),
            (1.5 * 1024.0 * 1024.0) as u64
        );
    }
}
