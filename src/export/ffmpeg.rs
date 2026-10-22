//! ffmpeg invocation: raw RGBA frames on stdin, optional audio cut from the source file.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const FADE_IN: f64 = 0.4;
const FADE_OUT: f64 = 0.8;

/// Audio input for a clip: source file, offset into it and clip length.
pub type AudioInput<'a> = (&'a Path, Duration, Duration);

/// Arguments (without the program name) that encode `fps` RGBA frames of `size` from stdin to `out`.
pub fn build_args(
    out: &Path,
    size: (u32, u32),
    fps: u32,
    audio: Option<AudioInput<'_>>,
) -> Vec<String> {
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        "-s",
    ]
    .map(str::to_owned)
    .to_vec();
    args.push(format!("{}x{}", size.0, size.1));
    args.extend([
        "-r".to_owned(),
        fps.to_string(),
        "-i".to_owned(),
        "-".to_owned(),
    ]);

    if let Some((path, start, dur)) = audio {
        let dur = dur.as_secs_f64();
        args.extend([
            "-ss".to_owned(),
            secs(start.as_secs_f64()),
            "-t".to_owned(),
            secs(dur),
            "-i".to_owned(),
            path.to_string_lossy().into_owned(),
            "-af".to_owned(),
            format!(
                "afade=t=in:st=0:d={FADE_IN},afade=t=out:st={}:d={FADE_OUT}",
                secs((dur - FADE_OUT).max(0.0))
            ),
            "-map".to_owned(),
            "0:v".to_owned(),
            "-map".to_owned(),
            "1:a".to_owned(),
        ]);
    }

    args.extend(
        [
            "-c:v", "libx264", "-preset", "fast", "-crf", "18", "-pix_fmt", "yuv420p",
        ]
        .map(str::to_owned),
    );
    if audio.is_some() {
        args.extend(["-c:a", "aac", "-b:a", "192k"].map(str::to_owned));
    }
    args.extend(["-movflags", "+faststart", "-shortest"].map(str::to_owned));
    args.push(out.to_string_lossy().into_owned());
    args
}

fn secs(s: f64) -> String {
    format!("{s:.3}")
}

/// A running ffmpeg process fed frames through its stdin.
pub struct Encoder {
    child: Child,
    stdin: Option<ChildStdin>,
    stderr: Option<JoinHandle<String>>,
}

impl Encoder {
    pub fn spawn(args: &[String]) -> Result<Self, String> {
        let mut child = Command::new("ffmpeg")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to start ffmpeg: {e}"))?;
        let stdin = child.stdin.take();
        // Drained on a thread so a chatty ffmpeg never blocks on a full pipe.
        let stderr = child.stderr.take().map(|mut err| {
            thread::spawn(move || {
                let mut text = String::new();
                let _ = err.read_to_string(&mut text);
                text
            })
        });
        Ok(Self {
            child,
            stdin,
            stderr,
        })
    }

    pub fn write_frame(&mut self, rgba: &[u8]) -> Result<(), String> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err("ffmpeg input already closed".to_owned());
        };
        if let Err(e) = stdin.write_all(rgba) {
            self.stdin = None;
            let _ = self.child.wait();
            return Err(self.error_text(&format!("ffmpeg stopped reading frames: {e}")));
        }
        Ok(())
    }

    /// Closes stdin and waits for ffmpeg to finish writing the file.
    pub fn finish(mut self) -> Result<(), String> {
        self.stdin = None;
        match self.child.wait() {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(self.error_text(&format!("ffmpeg exited with {status}"))),
            Err(e) => Err(format!("failed to wait for ffmpeg: {e}")),
        }
    }

    pub fn kill(mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// `fallback` followed by ffmpeg's last stderr line, if any.
    fn error_text(&mut self, fallback: &str) -> String {
        let stderr = self
            .stderr
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        match stderr.lines().rev().find(|l| !l.trim().is_empty()) {
            Some(line) => format!("{fallback}: {}", line.trim()),
            None => fallback.to_owned(),
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if self.stdin.take().is_some() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(args: &[String]) -> String {
        args.join(" ")
    }

    #[test]
    fn video_only_args() {
        let args = build_args(Path::new("/out/clip.mp4"), (1080, 1920), 30, None);
        assert_eq!(
            joined(&args),
            "-hide_banner -loglevel error -y -f rawvideo -pix_fmt rgba -s 1080x1920 -r 30 -i - \
             -c:v libx264 -preset fast -crf 18 -pix_fmt yuv420p \
             -movflags +faststart -shortest /out/clip.mp4"
        );
    }

    #[test]
    fn audio_args() {
        let audio = Path::new("/music/a b/song.flac");
        let args = build_args(
            Path::new("out.mp4"),
            (1920, 1080),
            60,
            Some((
                audio,
                Duration::from_millis(65_250),
                Duration::from_secs(30),
            )),
        );
        assert_eq!(
            joined(&args),
            "-hide_banner -loglevel error -y -f rawvideo -pix_fmt rgba -s 1920x1080 -r 60 -i - \
             -ss 65.250 -t 30.000 -i /music/a b/song.flac \
             -af afade=t=in:st=0:d=0.4,afade=t=out:st=29.200:d=0.8 -map 0:v -map 1:a \
             -c:v libx264 -preset fast -crf 18 -pix_fmt yuv420p -c:a aac -b:a 192k \
             -movflags +faststart -shortest out.mp4"
        );
        // The path stays one argument despite the space.
        assert!(args.contains(&"/music/a b/song.flac".to_owned()));
    }

    #[test]
    fn fade_out_never_starts_before_zero() {
        let args = build_args(
            Path::new("o.mp4"),
            (2, 2),
            30,
            Some((Path::new("a"), Duration::ZERO, Duration::from_millis(500))),
        );
        assert!(args
            .iter()
            .any(|a| a.ends_with("afade=t=out:st=0.000:d=0.8")));
    }
}
