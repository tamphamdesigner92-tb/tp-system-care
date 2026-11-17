//! WinSxS qua DISM; tiến trình servicing không được cưỡng bức dừng.

use std::sync::atomic::AtomicBool;

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    /// Estimate from DISM's rounded display, accounting for hard links.
    pub actual_size: u64,
    /// Included in actual_size; not reclaimable space.
    pub shared_size: u64,
    /// Included in actual_size; not a promise of reclaimable space.
    pub backups_size: u64,
    pub cache_size: u64,
    pub reclaimable_packages: u64,
    pub cleanup_recommended: bool,
    pub last_cleanup: String,
    pub output: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReport {
    pub exit_code: i32,
    pub reboot_required: bool,
    pub output: String,
}

/// Requires elevation. Analysis alone is cancellable and has a ten-minute deadline.
#[cfg(windows)]
pub fn analyze(cancel: &AtomicBool) -> Result<Analysis, String> {
    engine::analyze(cancel)
}

/// Giữ khóa công việc đến khi DISM thật sự kết thúc; không hủy cưỡng bức servicing.
#[cfg(windows)]
pub fn cleanup() -> Result<CleanupReport, String> {
    engine::cleanup()
}

#[cfg(not(windows))]
pub fn analyze(_cancel: &AtomicBool) -> Result<Analysis, String> {
    Err("component store servicing is only supported on Windows".into())
}

#[cfg(not(windows))]
pub fn cleanup() -> Result<CleanupReport, String> {
    Err("component store servicing is only supported on Windows".into())
}

#[cfg(any(windows, test))]
mod engine {
    use super::{Analysis, AtomicBool, CleanupReport};
    use std::io::{self, Read};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    const ANALYZE_ARGS: &[&str] = &[
        "/Online",
        "/Cleanup-Image",
        "/AnalyzeComponentStore",
        "/English",
    ];
    const CLEANUP_ARGS: &[&str] = &[
        "/Online",
        "/Cleanup-Image",
        "/StartComponentCleanup",
        "/NoRestart",
        "/English",
    ];
    const ANALYZE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
    const STREAM_LIMIT: usize = 128 * 1024;
    const OUTPUT_LIMIT: usize = 256 * 1024;
    const TRUNCATED: &str = "\n[DISM output truncated]\n";
    const SECTION: &str = "Component Store (WinSxS) information:";
    const SUCCESS: &str = "The operation completed successfully.";
    const LABELS: [&str; 8] = [
        "Actual Size of Component Store",
        "Shared with Windows",
        "Backups and Disabled Features",
        "Cache and Temporary Data",
        "Number of Reclaimable Packages",
        "Component Store Cleanup Recommended",
        "Date of Last Cleanup",
        "Windows Explorer Reported Size of Component Store",
    ];

    // A cleanup request cannot carry a cancellation token or deadline.
    #[derive(Clone, Copy)]
    enum Request<'a> {
        Analyze(&'a AtomicBool),
        Cleanup,
    }

    impl Request<'_> {
        fn args(self) -> &'static [&'static str] {
            match self {
                Self::Analyze(_) => ANALYZE_ARGS,
                Self::Cleanup => CLEANUP_ARGS,
            }
        }
    }

    #[derive(Debug)]
    struct ExitInfo {
        code: Option<i32>,
        description: String,
    }

    impl ExitInfo {
        fn summary(&self) -> String {
            match self.code {
                Some(code) => format!("DISM exited with code {code} (0x{:08X})", code as u32),
                None => format!("DISM exited without an exit code: {}", self.description),
            }
        }
    }

    #[derive(Default, Debug)]
    struct Capture {
        bytes: Vec<u8>,
        truncated: bool,
        error: Option<String>,
    }

    #[derive(Debug)]
    struct Observed {
        exit: ExitInfo,
        stopped: Option<&'static str>,
        observation_error: Option<String>,
    }

    struct ProcessOutput {
        observed: Observed,
        stdout: Capture,
        stderr: Capture,
    }

    trait Runner {
        fn run(&mut self, program: &Path, request: Request<'_>) -> Result<ProcessOutput, String>;
    }

    trait ChildControl {
        fn poll(&mut self) -> io::Result<Option<ExitInfo>>;
        fn wait(&mut self) -> io::Result<ExitInfo>;
        fn kill_read_only(&mut self) -> io::Result<()>;
    }

    fn observe_exit(
        child: &mut impl ChildControl,
        request: Request<'_>,
        mut elapsed: impl FnMut() -> Duration,
        mut idle: impl FnMut(),
    ) -> Observed {
        let mut observation_error = None;
        let mut stopped = None;
        loop {
            let result = match request {
                // An observation error does not prove servicing has stopped.
                Request::Cleanup => child.wait().map(Some),
                Request::Analyze(_) => child.poll(),
            };
            match result {
                Ok(Some(exit)) => {
                    return Observed {
                        exit,
                        stopped,
                        observation_error,
                    };
                }
                Ok(None) => {}
                Err(error) => {
                    observation_error.get_or_insert_with(|| error.to_string());
                    if matches!(request, Request::Analyze(_)) {
                        stopped.get_or_insert("read-only process observation failed");
                    }
                }
            }
            if let Request::Analyze(cancel) = request {
                if stopped.is_none() {
                    if cancel.load(Ordering::Relaxed) {
                        stopped = Some("analysis cancelled");
                    } else if elapsed() >= ANALYZE_TIMEOUT {
                        stopped = Some("analysis deadline exceeded");
                    }
                }
                if stopped.is_some() {
                    // Read-only cancellation still waits for confirmed exit.
                    if let Err(error) = child.kill_read_only() {
                        observation_error.get_or_insert_with(|| error.to_string());
                    }
                }
            }
            idle();
        }
    }

    fn capture(mut reader: impl Read) -> Capture {
        let mut result = Capture {
            bytes: Vec::with_capacity(STREAM_LIMIT),
            ..Capture::default()
        };
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    let keep = count.min(STREAM_LIMIT - result.bytes.len());
                    result.bytes.extend_from_slice(&buffer[..keep]);
                    result.truncated |= keep < count;
                    // Keep draining even after reaching the retention limit.
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    result.error = Some(error.to_string());
                    // Closing the failed reader prevents an undrained pipe deadlock.
                    break;
                }
            }
        }
        result
    }

    fn bounded_text(text: &str) -> String {
        if text.len() <= OUTPUT_LIMIT {
            return text.to_owned();
        }
        let mut end = OUTPUT_LIMIT - TRUNCATED.len();
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}{TRUNCATED}", &text[..end])
    }

    // BOM-less UTF-16 requires a recognizable alternating-zero prefix.
    // Unknown encodings and invalid Unicode fail closed; no lossy substitutions.
    fn decode(bytes: &[u8], truncated: bool) -> Result<String, String> {
        let (mut bytes, endian) = if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
            (rest, Some(true))
        } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
            (rest, Some(false))
        } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
            (rest, None)
        } else {
            let prefix = &bytes[..bytes.len().min(64)];
            let pairs = prefix.len() / 2;
            let ascii = |little: bool| {
                pairs >= 2
                    && prefix
                        .chunks_exact(2)
                        .filter(|pair| {
                            let (low, high) = if little {
                                (pair[0], pair[1])
                            } else {
                                (pair[1], pair[0])
                            };
                            high == 0 && low != 0 && low.is_ascii()
                        })
                        .count()
                        * 4
                        >= pairs * 3
            };
            let endian = if ascii(true) {
                Some(true)
            } else if ascii(false) {
                Some(false)
            } else {
                None
            };
            (bytes, endian)
        };
        let text = if let Some(little) = endian {
            if bytes.len() % 2 != 0 {
                if !truncated {
                    return Err("incomplete UTF-16 DISM output".into());
                }
                bytes = &bytes[..bytes.len() - 1];
            }
            let mut words: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| {
                    let pair = [pair[0], pair[1]];
                    if little {
                        u16::from_le_bytes(pair)
                    } else {
                        u16::from_be_bytes(pair)
                    }
                })
                .collect();
            if truncated
                && words
                    .last()
                    .is_some_and(|word| (0xD800..=0xDBFF).contains(word))
            {
                words.pop();
            }
            String::from_utf16(&words).map_err(|_| "invalid UTF-16 DISM output".to_owned())?
        } else {
            match std::str::from_utf8(bytes) {
                Ok(text) => text.to_owned(),
                Err(error) if truncated && error.error_len().is_none() => {
                    std::str::from_utf8(&bytes[..error.valid_up_to()])
                        .map_err(|_| "invalid UTF-8 DISM output".to_owned())?
                        .to_owned()
                }
                Err(_) => return Err("unsupported encoding or invalid UTF-8 DISM output".into()),
            }
        };
        if text.contains('\0') {
            return Err("unexpected NUL in decoded DISM output".into());
        }
        Ok(text)
    }

    fn render(stream: &Capture) -> String {
        let mut text = match decode(&stream.bytes, stream.truncated) {
            Ok(text) => text,
            Err(error) => format!("[unable to decode DISM output: {error}]"),
        };
        if stream.truncated {
            text.push_str(TRUNCATED);
        }
        if let Some(error) = &stream.error {
            text.push_str(&format!("\n[DISM output read error: {error}]\n"));
        }
        text
    }

    fn combined_output(result: &ProcessOutput) -> String {
        let mut text = render(&result.stdout);
        if !result.stderr.bytes.is_empty() || result.stderr.error.is_some() {
            text.push_str("\n[stderr]\n");
            text.push_str(&render(&result.stderr));
        }
        bounded_text(&text)
    }

    fn completed(result: &ProcessOutput) -> Result<(), String> {
        let reason = result.observed.stopped.map(str::to_owned).or_else(|| {
            result
                .observed
                .observation_error
                .as_ref()
                .map(|error| format!("process observation failed: {error}"))
        });
        if let Some(reason) = reason {
            return Err(format!(
                "{reason}; {}\n{}",
                result.observed.exit.summary(),
                combined_output(result)
            ));
        }
        Ok(())
    }

    fn checked_stream(stream: &Capture) -> Result<String, String> {
        if let Some(error) = &stream.error {
            return Err(format!("DISM output read failed: {error}"));
        }
        decode(&stream.bytes, stream.truncated)
    }

    fn analyze_with(
        program: &Path,
        cancel: &AtomicBool,
        runner: &mut impl Runner,
    ) -> Result<Analysis, String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("analysis cancelled before DISM was started".into());
        }
        let result = runner.run(program, Request::Analyze(cancel))?;
        completed(&result)?;
        let output = combined_output(&result);
        let error =
            |reason: String| format!("{reason}; {}\n{output}", result.observed.exit.summary());
        if result.observed.exit.code != Some(0) {
            return Err(error("component store analysis failed".into()));
        }
        // Truncated analysis cannot authorize mutation.
        if result.stdout.truncated || result.stderr.truncated {
            return Err(error(
                "component store analysis output exceeded its limit".into(),
            ));
        }
        let stdout = checked_stream(&result.stdout).map_err(&error)?;
        checked_stream(&result.stderr).map_err(&error)?;
        let mut analysis = parse_analysis(&stdout).map_err(error)?;
        analysis.output = output;
        Ok(analysis)
    }

    fn cleanup_with(program: &Path, runner: &mut impl Runner) -> Result<CleanupReport, String> {
        // Never trust a previously displayed/cached recommendation.
        let never_cancel = AtomicBool::new(false);
        let analysis = analyze_with(program, &never_cancel, runner)?;
        if !analysis.cleanup_recommended {
            return Err(
                "component store cleanup is not recommended by the current DISM analysis".into(),
            );
        }
        let result = runner.run(program, Request::Cleanup)?;
        completed(&result)?;
        let output = combined_output(&result);
        let error =
            |reason: String| format!("{reason}; {}\n{output}", result.observed.exit.summary());
        let exit_code = match result.observed.exit.code {
            Some(code @ (0 | 3010)) => code,
            _ => return Err(error("component store cleanup failed".into())),
        };
        // A capture error must not masquerade as observed cleanup success.
        checked_stream(&result.stdout).map_err(&error)?;
        checked_stream(&result.stderr).map_err(error)?;
        Ok(CleanupReport {
            exit_code,
            reboot_required: exit_code == 3010,
            output,
        })
    }

    fn unsigned(text: &str) -> Result<u64, String> {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!("invalid unsigned DISM number: {text:?}"));
        }
        text.parse()
            .map_err(|_| format!("DISM number out of range: {text:?}"))
    }

    fn size_bytes(text: &str) -> Result<u64, String> {
        let mut tokens = text.split_ascii_whitespace();
        let number = tokens.next().ok_or("missing DISM size")?;
        let unit = tokens.next().ok_or("missing DISM size unit")?;
        if tokens.next().is_some() {
            return Err(format!("unexpected text in DISM size: {text:?}"));
        }
        // DISM's rounded binary units give estimates, not exact byte counts.
        // Fractional bytes are truncated using checked integer arithmetic.
        let multiplier: u128 = match unit {
            "B" | "bytes" | "Bytes" => 1,
            "KB" => 1 << 10,
            "MB" => 1 << 20,
            "GB" => 1 << 30,
            "TB" => 1 << 40,
            _ => return Err(format!("unsupported DISM size unit: {unit:?}")),
        };
        let (whole, fraction) = match number.split_once('.') {
            Some((whole, fraction)) => {
                if fraction.is_empty() || fraction.len() > 9 {
                    return Err(format!("invalid DISM size precision: {number:?}"));
                }
                (whole, Some(fraction))
            }
            None => (number, None),
        };
        let whole = u128::from(unsigned(whole)?);
        let (fraction, scale) = match fraction {
            Some(fraction) => (
                u128::from(unsigned(fraction)?),
                10u128.pow(fraction.len() as u32),
            ),
            None => (0, 1),
        };
        let bytes = whole
            .checked_mul(scale)
            .and_then(|value| value.checked_add(fraction))
            .and_then(|value| value.checked_mul(multiplier))
            .ok_or_else(|| format!("DISM size out of range: {text:?}"))?
            / scale;
        u64::try_from(bytes).map_err(|_| format!("DISM size out of range: {text:?}"))
    }

    fn last_cleanup(text: &str) -> Result<String, String> {
        // Explicitly supported unavailable-date token; never synthesize a date.
        if text == "None" {
            return Ok(text.into());
        }
        let bytes = text.as_bytes();
        if bytes.len() != 19
            || bytes[4] != b'-'
            || bytes[7] != b'-'
            || bytes[10] != b' '
            || bytes[13] != b':'
            || bytes[16] != b':'
            || !bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| matches!(index, 4 | 7 | 10 | 13 | 16) || byte.is_ascii_digit())
        {
            return Err(format!("unsupported DISM cleanup date: {text:?}"));
        }
        // The ASCII layout check above makes these byte offsets safe.
        let year = unsigned(&text[..4])?;
        let month = unsigned(&text[5..7])?;
        let day = unsigned(&text[8..10])?;
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => 0,
        };
        if year == 0
            || day == 0
            || day > days
            || unsigned(&text[11..13])? > 23
            || unsigned(&text[14..16])? > 59
            || unsigned(&text[17..19])? > 59
        {
            return Err(format!("invalid DISM cleanup date: {text:?}"));
        }
        Ok(text.into())
    }

    fn parse_analysis(text: &str) -> Result<Analysis, String> {
        let mut values: [Option<&str>; 8] = [None; 8];
        let mut in_section = false;
        let mut found_section = false;
        let mut complete = false;
        for line in text.trim_start_matches('\u{feff}').split(['\r', '\n']) {
            let line = line.trim();
            if line == SECTION {
                if found_section {
                    return Err("duplicate DISM component store section".into());
                }
                found_section = true;
                in_section = true;
                continue;
            }
            if !in_section {
                if complete && !line.is_empty() {
                    return Err(format!(
                        "unexpected text after DISM analysis completion: {line:?}"
                    ));
                }
                if LABELS.iter().any(|label| line.starts_with(label)) {
                    return Err("DISM component store field outside its section".into());
                }
                continue;
            }
            if line.is_empty() {
                continue;
            }
            if line == SUCCESS {
                in_section = false;
                complete = true;
                continue;
            }
            let (label, value) = line
                .split_once(':')
                .ok_or_else(|| format!("unrecognized DISM analysis line: {line:?}"))?;
            let label = label.trim();
            let value = value.trim();
            let index = LABELS
                .iter()
                .position(|known| *known == label)
                .ok_or_else(|| format!("unrecognized DISM analysis field: {label:?}"))?;
            if value.is_empty() || values[index].replace(value).is_some() {
                return Err(format!("empty or duplicate DISM analysis field: {label}"));
            }
        }
        if !found_section || !complete {
            return Err("missing or incomplete DISM component store section".into());
        }
        let required = |index: usize| {
            values[index].ok_or_else(|| format!("missing DISM analysis field: {}", LABELS[index]))
        };
        // Explorer's hard-link-inflated total is not the component store's actual size.
        if let Some(explorer) = values[7] {
            size_bytes(explorer)?;
        }
        let cleanup_recommended = match required(5)? {
            "Yes" => true,
            "No" => false,
            other => return Err(format!("unknown DISM cleanup recommendation: {other:?}")),
        };
        Ok(Analysis {
            actual_size: size_bytes(required(0)?)?,
            shared_size: size_bytes(required(1)?)?,
            backups_size: size_bytes(required(2)?)?,
            cache_size: size_bytes(required(3)?)?,
            reclaimable_packages: unsigned(required(4)?)?,
            cleanup_recommended,
            last_cleanup: last_cleanup(required(6)?)?,
            output: bounded_text(text),
        })
    }

    fn dism_from_windows(windows: &Path) -> Result<PathBuf, String> {
        if !windows.is_absolute() {
            return Err("Windows known folder is not an absolute path".into());
        }
        Ok(windows.join("System32").join("dism.exe"))
    }

    #[cfg(windows)]
    fn authorized_program() -> Result<PathBuf, String> {
        use crate::winutil::{is_elevated, known_folder, Known};
        if !is_elevated() {
            return Err("needs_admin".into());
        }
        let windows = known_folder(Known::Windows).ok_or("Windows known folder is unavailable")?;
        dism_from_windows(&windows)
    }

    #[cfg(windows)]
    pub(super) fn analyze(cancel: &AtomicBool) -> Result<Analysis, String> {
        let program = authorized_program()?;
        analyze_with(&program, cancel, &mut NativeRunner)
    }

    #[cfg(windows)]
    pub(super) fn cleanup() -> Result<CleanupReport, String> {
        let program = authorized_program()?;
        cleanup_with(&program, &mut NativeRunner)
    }

    #[cfg(windows)]
    struct NativeRunner;

    #[cfg(windows)]
    impl ChildControl for std::process::Child {
        fn poll(&mut self) -> io::Result<Option<ExitInfo>> {
            self.try_wait().map(|status| {
                status.map(|status| ExitInfo {
                    code: status.code(),
                    description: status.to_string(),
                })
            })
        }

        fn wait(&mut self) -> io::Result<ExitInfo> {
            std::process::Child::wait(self).map(|status| ExitInfo {
                code: status.code(),
                description: status.to_string(),
            })
        }

        fn kill_read_only(&mut self) -> io::Result<()> {
            self.kill()
        }
    }

    #[cfg(windows)]
    fn reader_thread<R: Read + Send + 'static>(
        name: &str,
    ) -> io::Result<(std::sync::mpsc::Sender<R>, std::thread::JoinHandle<Capture>)> {
        let (sender, receiver) = std::sync::mpsc::channel::<R>();
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || receiver.recv().map(capture).unwrap_or_default())?;
        Ok((sender, thread))
    }

    #[cfg(windows)]
    impl Runner for NativeRunner {
        fn run(&mut self, program: &Path, request: Request<'_>) -> Result<ProcessOutput, String> {
            use std::os::windows::process::CommandExt;
            use std::process::{ChildStderr, ChildStdout, Command, Stdio};
            use std::time::Instant;

            if !program.is_absolute() {
                return Err("refusing to launch DISM through a relative path".into());
            }
            if let Request::Analyze(cancel) = request {
                if cancel.load(Ordering::Relaxed) {
                    return Err("analysis cancelled before DISM was started".into());
                }
            }
            // Create drain workers before spawn so failures cannot strand live servicing.
            let (out_sender, out_thread) = reader_thread::<ChildStdout>("dism-stdout")
                .map_err(|error| format!("DISM was not started: {error}"))?;
            let (err_sender, err_thread) = match reader_thread::<ChildStderr>("dism-stderr") {
                Ok(worker) => worker,
                Err(error) => {
                    drop(out_sender);
                    let _ = out_thread.join();
                    return Err(format!("DISM was not started: {error}"));
                }
            };
            let child = Command::new(program)
                .args(request.args())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .spawn();
            let mut child = match child {
                Ok(child) => child,
                Err(error) => {
                    drop(out_sender);
                    drop(err_sender);
                    let _ = out_thread.join();
                    let _ = err_thread.join();
                    return Err(format!("DISM was not started (no exit code): {error}"));
                }
            };
            let out_delivered = child
                .stdout
                .take()
                .is_some_and(|pipe| out_sender.send(pipe).is_ok());
            let err_delivered = child
                .stderr
                .take()
                .is_some_and(|pipe| err_sender.send(pipe).is_ok());
            drop(out_sender);
            drop(err_sender);
            let started = Instant::now();
            let mut observed = observe_exit(
                &mut child,
                request,
                || started.elapsed(),
                || std::thread::sleep(Duration::from_millis(100)),
            );
            // Never return early after spawn, including output setup failures.
            if !out_delivered || !err_delivered {
                observed
                    .observation_error
                    .get_or_insert_with(|| "DISM pipe setup failed".into());
            }
            let join = |thread: std::thread::JoinHandle<Capture>| {
                thread.join().unwrap_or_else(|_| Capture {
                    error: Some("DISM output reader panicked".into()),
                    ..Capture::default()
                })
            };
            Ok(ProcessOutput {
                observed,
                stdout: join(out_thread),
                stderr: join(err_thread),
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::collections::VecDeque;

        // Microsoft Learn's example, not output from a live servicing command.
        const FIXTURE: &str = "Deployment Image Servicing and Management tool\r\n\
Version: 10.0.XXXX.0\r\n\
Image Version: 10.0.XXXX.0\r\n\
[==========================100.0%==========================]\r\n\
Component Store (WinSxS) information:\r\n\
Windows Explorer Reported Size of Component Store : 4.98 GB\r\n\
Actual Size of Component Store : 4.88 GB\r\n\
    Shared with Windows : 4.38 GB\r\n\
    Backups and Disabled Features : 506.90 MB\r\n\
    Cache and Temporary Data : 279.52 KB\r\n\
Date of Last Cleanup : 2021-06-24 23:32:22\r\n\
Number of Reclaimable Packages : 0\r\n\
Component Store Cleanup Recommended : No\r\n\
The operation completed successfully.\r\n";

        fn recommended() -> String {
            FIXTURE
                .replace("Recommended : No", "Recommended : Yes")
                .replace("Packages : 0", "Packages : 3")
        }

        fn utf16(text: &str, little: bool, bom: bool) -> Vec<u8> {
            let mut bytes = Vec::new();
            if bom {
                bytes.extend(if little { [0xFF, 0xFE] } else { [0xFE, 0xFF] });
            }
            for word in text.encode_utf16() {
                bytes.extend(if little {
                    word.to_le_bytes()
                } else {
                    word.to_be_bytes()
                });
            }
            bytes
        }

        fn exit(code: i32) -> ExitInfo {
            ExitInfo {
                code: Some(code),
                description: format!("exit code: {code}"),
            }
        }

        fn process(text: &str, code: i32) -> ProcessOutput {
            ProcessOutput {
                observed: Observed {
                    exit: exit(code),
                    stopped: None,
                    observation_error: None,
                },
                stdout: Capture {
                    bytes: text.as_bytes().to_vec(),
                    ..Capture::default()
                },
                stderr: Capture::default(),
            }
        }

        fn program() -> PathBuf {
            #[cfg(windows)]
            let root = Path::new("D:\\Windows");
            #[cfg(not(windows))]
            let root = Path::new("/Windows");
            dism_from_windows(root).unwrap()
        }

        struct FakeRunner {
            results: VecDeque<Result<ProcessOutput, String>>,
            calls: Vec<(PathBuf, bool, Vec<String>)>,
        }

        impl FakeRunner {
            fn new(results: Vec<ProcessOutput>) -> Self {
                Self {
                    results: results.into_iter().map(Ok).collect(),
                    calls: Vec::new(),
                }
            }
        }

        impl Runner for FakeRunner {
            fn run(
                &mut self,
                program: &Path,
                request: Request<'_>,
            ) -> Result<ProcessOutput, String> {
                if let Request::Analyze(cancel) = request {
                    assert!(!cancel.load(Ordering::Relaxed));
                }
                self.calls.push((
                    program.to_owned(),
                    matches!(request, Request::Cleanup),
                    request.args().iter().map(|arg| (*arg).to_owned()).collect(),
                ));
                self.results
                    .pop_front()
                    .expect("unexpected process invocation")
            }
        }

        #[test]
        fn parses_official_fixture_without_inventing_reclaimable_bytes() {
            let result = parse_analysis(FIXTURE).unwrap();
            assert_eq!(result.actual_size, 5_239_860_101);
            assert_eq!(result.shared_size, 4_702_989_189);
            assert_eq!(result.backups_size, 531_523_174);
            assert_eq!(result.cache_size, 286_228);
            assert_eq!(result.reclaimable_packages, 0);
            assert!(!result.cleanup_recommended);
            assert_eq!(result.last_cleanup, "2021-06-24 23:32:22");
            assert_eq!(result.output, FIXTURE);
            let yes = parse_analysis(&recommended()).unwrap();
            assert!(yes.cleanup_recommended);
            assert_eq!(yes.reclaimable_packages, 3);
        }

        #[test]
        fn required_fields_cannot_be_missing_empty_or_duplicated() {
            for label in &LABELS[..7] {
                let line = FIXTURE
                    .lines()
                    .find(|line| line.trim().starts_with(label))
                    .unwrap();
                let missing = FIXTURE.replace(line, "");
                assert!(parse_analysis(&missing).is_err(), "missing {label}");
                let empty = FIXTURE.replace(line, &format!("{label} : "));
                assert!(parse_analysis(&empty).is_err(), "empty {label}");
                let duplicate = FIXTURE.replace(line, &format!("{line}\r\n{line}"));
                assert!(parse_analysis(&duplicate).is_err(), "duplicate {label}");
            }
        }

        #[test]
        fn unknown_fields_and_incomplete_sections_are_rejected() {
            for bad in [
                FIXTURE.replace("Shared with Windows", "Shared Windows"),
                FIXTURE.replace(
                    SUCCESS,
                    "New Unknown Field : 123\r\nThe operation completed successfully.",
                ),
                FIXTURE.replace(SUCCESS, "Error: 87"),
                FIXTURE.replace(SUCCESS, ""),
                FIXTURE.replace(SECTION, ""),
                format!("{FIXTURE}\r\n{FIXTURE}"),
                FIXTURE.replace("Recommended : No", "Recommended : Maybe"),
                FIXTURE.replace("Recommended : No", "Recommended : yes"),
                FIXTURE.replace("Packages : 0", "Packages : N/A"),
                FIXTURE.replace("Packages : 0", "Packages : -1"),
                FIXTURE.replace("Packages : 0", "Packages : 18446744073709551616"),
                FIXTURE.replace("4.98 GB", "4.98 mystery"),
                format!("{FIXTURE}\r\nUnexpected Field : 123"),
                format!("{FIXTURE}\r\nError: 87"),
                FIXTURE.replace(
                    "Windows Explorer Reported Size of Component Store : 4.98 GB",
                    "Windows Explorer Reported Size of Component Store : 4.98 GB\r\nWindows Explorer Reported Size of Component Store : 4.98 GB",
                ),
            ] {
                assert!(
                    parse_analysis(&bad).is_err(),
                    "unexpected acceptance: {bad}"
                );
            }
        }

        #[test]
        fn explorer_field_is_optional_but_validated_and_never_used_as_actual_size() {
            let text = FIXTURE.replace(
                "Windows Explorer Reported Size of Component Store : 4.98 GB\r\n",
                "",
            );
            assert_eq!(parse_analysis(&text).unwrap().actual_size, 5_239_860_101);
        }

        #[test]
        fn size_conversion_is_checked_and_has_explicit_units() {
            for (text, expected) in [
                ("0 bytes", 0),
                ("1 B", 1),
                ("42 Bytes", 42),
                ("1 KB", 1024),
                ("1.5 MB", 1_572_864),
                ("2 GB", 2_147_483_648),
                ("1 TB", 1_099_511_627_776),
                ("0.01 KB", 10),
                ("18446744073709551615 B", u64::MAX),
                ("\t1.25  KB ", 1280),
            ] {
                assert_eq!(size_bytes(text).unwrap(), expected, "{text}");
            }
            for text in [
                "",
                "1",
                "GB",
                "NaN GB",
                "inf GB",
                "-1 GB",
                "+1 GB",
                ".5 GB",
                "1. GB",
                "1e3 KB",
                "1,23 GB",
                "1,000 KB",
                "1.2.3 KB",
                "1 GB extra",
                "1 gb",
                "1 PB",
                "1 KiB",
                "1 byte",
                "1\u{a0}GB",
                "1.1234567890 GB",
                "18446744073709551616 B",
                "18446744073709551615 TB",
            ] {
                assert!(size_bytes(text).is_err(), "unexpected acceptance: {text}");
            }
        }

        #[test]
        fn dates_are_retained_verbatim_and_checked() {
            for text in [
                "None",
                "2024-02-29 23:59:59",
                "2000-02-29 00:00:00",
                "2021-06-24 23:32:22",
            ] {
                assert_eq!(last_cleanup(text).unwrap(), text);
            }
            for text in [
                "",
                "Unknown",
                "N/A",
                "Never",
                "2023-02-29 00:00:00",
                "1900-02-29 00:00:00",
                "0000-01-01 00:00:00",
                "2024-00-01 00:00:00",
                "2024-13-01 00:00:00",
                "2024-01-00 00:00:00",
                "2024-04-31 00:00:00",
                "2024-01-01 24:00:00",
                "2024-01-01 00:60:00",
                "2024-01-01 00:00:60",
                "06/24/2021 23:32:22",
                "2024-01-01T00:00:00",
                "2024-01-01 00:00:00 extra",
                "实际清理日期",
            ] {
                assert!(last_cleanup(text).is_err(), "unexpected acceptance: {text}");
            }
        }

        #[test]
        fn unicode_is_decoded_not_nul_stripped_or_lossily_replaced() {
            let text = format!("DISM resource: Tiếng Việt, 中文, \u{1f600}\r\n{FIXTURE}");
            assert_eq!(decode(text.as_bytes(), false).unwrap(), text);
            let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
            utf8_bom.extend_from_slice(text.as_bytes());
            assert_eq!(decode(&utf8_bom, false).unwrap(), text);
            for little in [true, false] {
                for bom in [true, false] {
                    let bytes = utf16(&text, little, bom);
                    let decoded = decode(&bytes, false).unwrap();
                    assert_eq!(decoded, text);
                    assert!(parse_analysis(&decoded).is_ok());
                }
            }
            for bytes in [
                vec![0xFF, 0xFE, 0x41],
                vec![0xFF, 0xFE, 0x00, 0xD8],
                vec![0xFE, 0xFF, 0xDC, 0x00],
                vec![0xFF],
                b"unexpected\0NUL".to_vec(),
            ] {
                assert!(decode(&bytes, false).is_err());
            }
        }

        #[test]
        fn only_a_truncated_final_unicode_character_can_be_dropped() {
            assert_eq!(decode(&[b'A', 0xE2, 0x82], true).unwrap(), "A");
            assert!(decode(&[b'A', 0xE2, 0x82], false).is_err());
            assert!(decode(&[b'A', 0xFF, b'B'], true).is_err());
            assert_eq!(
                decode(&[0xFF, 0xFE, b'A', 0, 0x3D, 0xD8], true).unwrap(),
                "A"
            );
            assert!(decode(&[0xFF, 0xFE, b'A', 0, 0x3D, 0xD8], false).is_err());
            assert!(decode(&[0xFF, 0xFE, 0, 0xDC, b'A', 0], true).is_err());
        }

        #[test]
        fn capture_is_bounded_but_continues_draining() {
            let bytes = vec![b'x'; STREAM_LIMIT * 4 + 13];
            let mut cursor = io::Cursor::new(bytes);
            let result = capture(&mut cursor);
            assert_eq!(cursor.position(), (STREAM_LIMIT * 4 + 13) as u64);
            assert_eq!(result.bytes.len(), STREAM_LIMIT);
            assert!(result.truncated);
            assert!(result.error.is_none());
            let exact = capture(io::Cursor::new(vec![b'x'; STREAM_LIMIT]));
            assert!(!exact.truncated);
            assert!(capture(io::Cursor::new(Vec::<u8>::new())).bytes.is_empty());
        }

        #[test]
        fn pipe_reader_retries_interruptions_and_reports_real_read_errors() {
            struct Reader {
                step: usize,
            }
            impl Read for Reader {
                fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                    self.step += 1;
                    match self.step {
                        1 => Err(io::ErrorKind::Interrupted.into()),
                        2 => {
                            buffer[..5].copy_from_slice(b"hello");
                            Ok(5)
                        }
                        _ => Err(io::Error::other("injected pipe read failure")),
                    }
                }
            }
            let result = capture(Reader { step: 0 });
            assert_eq!(result.bytes, b"hello");
            assert!(!result.truncated);
            assert!(result.error.as_ref().unwrap().contains("pipe read failure"));
            assert!(render(&result).contains("output read error"));
            assert!(checked_stream(&result).is_err());
        }

        #[test]
        fn output_is_bounded_at_a_unicode_boundary_with_a_truncation_marker() {
            let text = "中文\u{1f600}".repeat(OUTPUT_LIMIT);
            let result = bounded_text(&text);
            assert!(result.len() <= OUTPUT_LIMIT);
            assert!(result.ends_with(TRUNCATED));
            let mut process = process("", 0);
            process.stdout = capture(io::Cursor::new(utf16(&text, true, true)));
            process.stderr = capture(io::Cursor::new(text.into_bytes()));
            assert!(combined_output(&process).len() <= OUTPUT_LIMIT);
            assert!(combined_output(&process).contains("truncated"));
        }

        #[test]
        fn cleanup_always_reanalyzes_and_uses_only_the_fixed_safe_command() {
            let mut runner = FakeRunner::new(vec![
                process(&recommended(), 0),
                process("cleanup complete", 0),
            ]);
            let report = cleanup_with(&program(), &mut runner).unwrap();
            assert_eq!(report.exit_code, 0);
            assert!(!report.reboot_required);
            assert_eq!(runner.calls.len(), 2);
            assert_eq!(
                runner.calls[0],
                (
                    program(),
                    false,
                    ANALYZE_ARGS.iter().map(|arg| (*arg).to_owned()).collect()
                )
            );
            assert_eq!(
                runner.calls[1],
                (
                    program(),
                    true,
                    CLEANUP_ARGS.iter().map(|arg| (*arg).to_owned()).collect()
                )
            );
            assert_eq!(
                CLEANUP_ARGS,
                [
                    "/Online",
                    "/Cleanup-Image",
                    "/StartComponentCleanup",
                    "/NoRestart",
                    "/English"
                ]
            );
            for args in [ANALYZE_ARGS, CLEANUP_ARGS] {
                assert!(
                    !args.iter().any(|arg| arg.eq_ignore_ascii_case("/ResetBase")
                        || arg.eq_ignore_ascii_case("/SPSuperseded"))
                );
            }
        }

        #[test]
        fn cleanup_refuses_not_recommended_failed_or_malformed_fresh_analysis() {
            for result in [
                process(FIXTURE, 0),
                process(&recommended(), 87),
                process("unsupported output", 0),
                process(&recommended().replace("279.52 KB", "unknown KB"), 0),
                process(
                    &recommended().replace("Recommended : Yes", "Recommended : Unknown"),
                    0,
                ),
            ] {
                let mut runner = FakeRunner::new(vec![result]);
                assert!(cleanup_with(&program(), &mut runner).is_err());
                assert_eq!(runner.calls.len(), 1);
                assert!(!runner.calls[0].1);
            }
        }

        #[test]
        fn cleanup_handles_reboot_required_and_actual_failure_codes() {
            let mut runner = FakeRunner::new(vec![
                process(&recommended(), 0),
                process("reboot needed", 3010),
            ]);
            let report = cleanup_with(&program(), &mut runner).unwrap();
            assert_eq!(report.exit_code, 3010);
            assert!(report.reboot_required);
            for code in [87, 1641, 3017, -2_147_024_805] {
                let mut failed = process("servicing failure", code);
                failed.stderr.bytes = b"actual stderr diagnostic".to_vec();
                let mut runner = FakeRunner::new(vec![process(&recommended(), 0), failed]);
                let error = cleanup_with(&program(), &mut runner).unwrap_err();
                assert!(error.contains(&format!("code {code}")));
                assert!(error.contains(&format!("0x{:08X}", code as u32)));
                assert!(error.contains("actual stderr diagnostic"));
            }
        }

        #[test]
        fn cancelled_analysis_is_never_success_even_if_the_exit_code_is_zero() {
            let cancel = AtomicBool::new(true);
            let mut runner = FakeRunner::new(vec![]);
            assert!(analyze_with(&program(), &cancel, &mut runner).is_err());
            assert!(runner.calls.is_empty());
            let cancel = AtomicBool::new(false);
            let mut result = process(FIXTURE, 0);
            result.observed.stopped = Some("analysis cancelled");
            let mut runner = FakeRunner::new(vec![result]);
            let error = analyze_with(&program(), &cancel, &mut runner).unwrap_err();
            assert!(error.contains("cancelled"));
            assert!(error.contains("code 0"));
        }

        #[test]
        fn truncated_or_corrupted_analysis_cannot_authorize_cleanup() {
            for kind in 0..5 {
                let mut result = process(&recommended(), 0);
                match kind {
                    0 => result.stdout.truncated = true,
                    1 => result.stderr.truncated = true,
                    2 => result.stdout.error = Some("read failure".into()),
                    3 => result.stderr.bytes = vec![0xFF],
                    _ => result.observed.observation_error = Some("wait failure".into()),
                }
                let mut runner = FakeRunner::new(vec![result]);
                let error = cleanup_with(&program(), &mut runner).unwrap_err();
                assert!(error.contains("code 0"));
                assert_eq!(runner.calls.len(), 1);
            }
        }

        #[test]
        fn cleanup_retention_truncation_is_marked_but_read_errors_are_not_success() {
            let mut result = process("progress", 0);
            result.stdout.truncated = true;
            let mut runner = FakeRunner::new(vec![process(&recommended(), 0), result]);
            assert!(cleanup_with(&program(), &mut runner)
                .unwrap()
                .output
                .contains(TRUNCATED));
            for kind in 0..3 {
                let mut result = process("progress", 3010);
                match kind {
                    0 => result.stdout.error = Some("read failure".into()),
                    1 => result.stderr.bytes = vec![0xFF],
                    _ => result.observed.observation_error = Some("wait failure".into()),
                }
                let mut runner = FakeRunner::new(vec![process(&recommended(), 0), result]);
                let error = cleanup_with(&program(), &mut runner).unwrap_err();
                assert!(error.contains("code 3010"));
            }
        }

        #[test]
        fn analysis_supports_utf16_resources_and_preserves_stderr() {
            let text = format!("DISM resource: 中文\r\n{FIXTURE}");
            let mut result = process("", 0);
            result.stdout.bytes = utf16(&text, true, true);
            result.stderr.bytes = utf16("Thông tin bổ sung", false, true);
            let mut runner = FakeRunner::new(vec![result]);
            let report = analyze_with(&program(), &AtomicBool::new(false), &mut runner).unwrap();
            assert!(report.output.contains("中文"));
            assert!(report.output.contains("Thông tin bổ sung"));
            assert!(!report.output.contains('\0'));
        }

        #[test]
        fn missing_exit_code_and_spawn_errors_are_not_success() {
            let mut result = process("process ended", 0);
            result.observed.exit = ExitInfo {
                code: None,
                description: "signal 9".into(),
            };
            let mut runner = FakeRunner::new(vec![process(&recommended(), 0), result]);
            assert!(cleanup_with(&program(), &mut runner)
                .unwrap_err()
                .contains("signal 9"));
            let mut runner = FakeRunner::new(vec![]);
            runner.results.push_back(Err(
                "DISM was not started (no exit code): access denied".into()
            ));
            assert!(
                analyze_with(&program(), &AtomicBool::new(false), &mut runner)
                    .unwrap_err()
                    .contains("no exit code")
            );
        }

        struct FakeChild {
            exits: VecDeque<io::Result<Option<ExitInfo>>>,
            polls: usize,
            waits: usize,
            kills: usize,
            kill_fails: bool,
        }

        impl FakeChild {
            fn new(exits: Vec<io::Result<Option<ExitInfo>>>) -> Self {
                Self {
                    exits: exits.into(),
                    polls: 0,
                    waits: 0,
                    kills: 0,
                    kill_fails: false,
                }
            }
        }

        impl ChildControl for FakeChild {
            fn poll(&mut self) -> io::Result<Option<ExitInfo>> {
                self.polls += 1;
                self.exits.pop_front().expect("polling past real exit")
            }
            fn wait(&mut self) -> io::Result<ExitInfo> {
                self.waits += 1;
                self.exits
                    .pop_front()
                    .expect("waiting past real exit")?
                    .ok_or_else(|| io::Error::other("injected wait failure"))
            }
            fn kill_read_only(&mut self) -> io::Result<()> {
                self.kills += 1;
                if self.kill_fails {
                    Err(io::Error::other("injected kill failure"))
                } else {
                    Ok(())
                }
            }
        }

        #[test]
        fn servicing_wait_errors_do_not_release_wait_or_kill_the_process() {
            let mut child = FakeChild::new(vec![
                Err(io::Error::other("injected observation failure")),
                Ok(None),
                Ok(Some(exit(3010))),
            ]);
            let mut idles = 0;
            let observed = observe_exit(
                &mut child,
                Request::Cleanup,
                || panic!("cleanup must never consult a deadline"),
                || idles += 1,
            );
            assert_eq!(child.waits, 3);
            assert_eq!(child.polls, 0);
            assert_eq!(child.kills, 0);
            assert_eq!(idles, 2);
            assert_eq!(observed.exit.code, Some(3010));
            assert!(observed.observation_error.is_some());
            assert!(observed.stopped.is_none());
        }

        #[test]
        fn only_read_only_analysis_is_killed_on_cancellation_or_deadline() {
            for cancelled in [false, true] {
                let cancel = AtomicBool::new(cancelled);
                let mut child = FakeChild::new(vec![Ok(None), Ok(None), Ok(Some(exit(0)))]);
                let observed = observe_exit(
                    &mut child,
                    Request::Analyze(&cancel),
                    || ANALYZE_TIMEOUT,
                    || {},
                );
                assert_eq!(child.kills, 2);
                assert_eq!(child.waits, 0);
                assert_eq!(child.polls, 3);
                assert_eq!(observed.exit.code, Some(0));
                assert_eq!(
                    observed.stopped,
                    Some(if cancelled {
                        "analysis cancelled"
                    } else {
                        "analysis deadline exceeded"
                    })
                );
            }
        }

        #[test]
        fn failed_read_only_kill_still_waits_for_actual_exit() {
            let cancel = AtomicBool::new(true);
            let mut child = FakeChild::new(vec![Ok(None), Ok(None), Ok(Some(exit(87)))]);
            child.kill_fails = true;
            let observed = observe_exit(
                &mut child,
                Request::Analyze(&cancel),
                || Duration::ZERO,
                || {},
            );
            assert_eq!(child.kills, 2);
            assert_eq!(child.polls, 3);
            assert_eq!(observed.exit.code, Some(87));
            assert!(observed.observation_error.is_some());
        }

        #[test]
        fn read_only_observation_failure_cannot_be_reported_as_success() {
            let cancel = AtomicBool::new(false);
            let mut child = FakeChild::new(vec![
                Err(io::Error::other("injected poll failure")),
                Ok(Some(exit(0))),
            ]);
            let observed = observe_exit(
                &mut child,
                Request::Analyze(&cancel),
                || Duration::ZERO,
                || {},
            );
            assert_eq!(child.kills, 1);
            assert_eq!(child.polls, 2);
            assert_eq!(observed.exit.code, Some(0));
            assert_eq!(
                observed.stopped,
                Some("read-only process observation failed")
            );
            assert!(observed.observation_error.is_some());
        }

        #[test]
        fn normally_finishing_analysis_is_not_killed() {
            let cancel = AtomicBool::new(false);
            let mut child = FakeChild::new(vec![Ok(None), Ok(Some(exit(0)))]);
            let observed = observe_exit(
                &mut child,
                Request::Analyze(&cancel),
                || Duration::ZERO,
                || {},
            );
            assert_eq!(child.kills, 0);
            assert!(observed.stopped.is_none());
            assert!(observed.observation_error.is_none());
        }

        #[test]
        fn program_is_absolute_and_derived_only_from_the_windows_known_folder() {
            assert!(program().is_absolute());
            assert!(program().ends_with(Path::new("System32").join("dism.exe")));
            assert!(dism_from_windows(Path::new("Windows")).is_err());
        }

        #[test]
        fn serde_api_uses_exact_camel_case_field_names() {
            let value = serde_json::to_value(parse_analysis(FIXTURE).unwrap()).unwrap();
            let object = value.as_object().unwrap();
            assert_eq!(object.len(), 8);
            for key in [
                "actualSize",
                "sharedSize",
                "backupsSize",
                "cacheSize",
                "reclaimablePackages",
                "cleanupRecommended",
                "lastCleanup",
                "output",
            ] {
                assert!(object.contains_key(key), "missing {key}");
            }
            let value = serde_json::to_value(CleanupReport {
                exit_code: 3010,
                reboot_required: true,
                output: String::new(),
            })
            .unwrap();
            assert_eq!(value.as_object().unwrap().len(), 3);
            assert_eq!(value["exitCode"], 3010);
            assert_eq!(value["rebootRequired"], true);
            assert_eq!(value["output"], "");
        }

        #[cfg(not(windows))]
        #[test]
        fn non_windows_public_apis_are_unsupported() {
            assert!(super::super::analyze(&AtomicBool::new(false))
                .unwrap_err()
                .contains("only supported on Windows"));
            assert!(super::super::cleanup()
                .unwrap_err()
                .contains("only supported on Windows"));
        }
    }
}
