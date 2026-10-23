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

/// Encoder settings of an export.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quality {
    /// Close to the rendered frames, for keeping or uploading where the platform re-encodes.
    #[default]
    High,
    /// Capped bitrate that fits messenger limits, such as a 30 s 1080×1920 WhatsApp status
    /// under 16 MB.
    Small,
}

/// Peak video bitrate of [`Quality::Small`] at 1080×1920, in kbit/s; other sizes scale with
/// their pixel count.
const SMALL_MAX_KBPS: u64 = 3800;
/// Typical video bitrates at 1080×1920, in kbit/s, for size estimates.
const HIGH_TYPICAL_KBPS: u64 = 3500;
const SMALL_TYPICAL_KBPS: u64 = 1600;

impl Quality {
    pub const ALL: [Quality; 2] = [Quality::High, Quality::Small];

    pub fn display_name(self) -> &'static str {
        match self {
            Quality::High => "High quality",
            Quality::Small => "Small file",
        }
    }

    fn crf(self) -> u8 {
        match self {
            Quality::High => 19,
            Quality::Small => 24,
        }
    }

    fn audio_kbps(self) -> u64 {
        match self {
            Quality::High => 192,
            Quality::Small => 128,
        }
    }

    /// Video bitrate cap for frames of `size`, in kbit/s.
    fn max_kbps(self, size: (u32, u32)) -> Option<u64> {
        (self == Quality::Small).then(|| scale_kbps(SMALL_MAX_KBPS, size))
    }

    /// Rough file size in bytes of a clip of `secs` seconds at `size`.
    pub fn estimate_bytes(self, size: (u32, u32), secs: f64, audio: bool) -> u64 {
        let video = match self {
            Quality::High => HIGH_TYPICAL_KBPS,
            Quality::Small => SMALL_TYPICAL_KBPS,
        };
        let audio = if audio { self.audio_kbps() } else { 0 };
        ((scale_kbps(video, size) + audio) as f64 * secs * 1000.0 / 8.0) as u64
    }
}

/// `kbps` at 1080×1920 scaled to the pixel count of `size`.
fn scale_kbps(kbps: u64, (w, h): (u32, u32)) -> u64 {
    kbps * u64::from(w) * u64::from(h) / (1080 * 1920)
}

/// Arguments (without the program name) that encode `fps` RGBA frames of `size` from stdin to
/// `out` at `quality`.
///
/// Frames are sRGB, so they are converted to limited-range BT.709 YUV explicitly and tagged as
/// such; players assume BT.709 for HD video, and an untagged BT.601 conversion shifts colours.
/// `setparams` sets the primaries and transfer on the frames, as the encoder takes them from the
/// frames rather than from the output options once a filter has run.
/// Full chroma interpolation keeps coloured text edges sharp, and the auto-variance AQ spends
/// more bits on dark gradients, which otherwise flatten into bands.
pub fn build_args(
    out: &Path,
    size: (u32, u32),
    fps: u32,
    quality: Quality,
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
            "-vf",
            "scale=out_color_matrix=bt709:out_range=tv:flags=lanczos+accurate_rnd+full_chroma_int,\
             format=yuv420p,setparams=color_primaries=bt709:color_trc=bt709",
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-tune",
            "film",
            "-crf",
        ]
        .map(str::to_owned),
    );
    args.push(quality.crf().to_string());
    if let Some(max) = quality.max_kbps(size) {
        args.extend([
            "-maxrate".to_owned(),
            format!("{max}k"),
            "-bufsize".to_owned(),
            format!("{}k", max * 2),
        ]);
    }
    args.extend([
        "-x264-params".to_owned(),
        "aq-mode=3".to_owned(),
        "-g".to_owned(),
        (fps * 2).to_string(),
        "-bf".to_owned(),
        "3".to_owned(),
    ]);
    args.extend(
        [
            "-pix_fmt",
            "yuv420p",
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-color_range",
            "tv",
        ]
        .map(str::to_owned),
    );
    if audio.is_some() {
        args.extend(["-c:a", "aac", "-b:a"].map(str::to_owned));
        args.push(format!("{}k", quality.audio_kbps()));
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

    const VIDEO: &str = "-vf scale=out_color_matrix=bt709:out_range=tv:\
                         flags=lanczos+accurate_rnd+full_chroma_int,format=yuv420p,\
                         setparams=color_primaries=bt709:color_trc=bt709 \
                         -c:v libx264 -preset medium -tune film";
    const COLOUR: &str = "-x264-params aq-mode=3";
    const TAGS: &str = "-pix_fmt yuv420p -colorspace bt709 -color_primaries bt709 \
                        -color_trc bt709 -color_range tv";

    #[test]
    fn video_only_args() {
        let args = build_args(
            Path::new("/out/clip.mp4"),
            (1080, 1920),
            30,
            Quality::High,
            None,
        );
        assert_eq!(
            joined(&args),
            format!(
                "-hide_banner -loglevel error -y -f rawvideo -pix_fmt rgba -s 1080x1920 -r 30 -i - \
                 {VIDEO} -crf 19 {COLOUR} -g 60 -bf 3 {TAGS} \
                 -movflags +faststart -shortest /out/clip.mp4"
            )
        );
    }

    #[test]
    fn audio_args() {
        let audio = Path::new("/music/a b/song.flac");
        let args = build_args(
            Path::new("out.mp4"),
            (1920, 1080),
            60,
            Quality::High,
            Some((
                audio,
                Duration::from_millis(65_250),
                Duration::from_secs(30),
            )),
        );
        assert_eq!(
            joined(&args),
            format!(
                "-hide_banner -loglevel error -y -f rawvideo -pix_fmt rgba -s 1920x1080 -r 60 -i - \
                 -ss 65.250 -t 30.000 -i /music/a b/song.flac \
                 -af afade=t=in:st=0:d=0.4,afade=t=out:st=29.200:d=0.8 -map 0:v -map 1:a \
                 {VIDEO} -crf 19 {COLOUR} -g 120 -bf 3 {TAGS} -c:a aac -b:a 192k \
                 -movflags +faststart -shortest out.mp4"
            )
        );
        // The path stays one argument despite the space.
        assert!(args.contains(&"/music/a b/song.flac".to_owned()));
    }

    #[test]
    fn small_file_args() {
        let audio = Path::new("a.flac");
        let args = build_args(
            Path::new("out.mp4"),
            (1080, 1920),
            30,
            Quality::Small,
            Some((audio, Duration::ZERO, Duration::from_secs(30))),
        );
        let joined = joined(&args);
        assert!(
            joined.contains(&format!(
                "{VIDEO} -crf 24 -maxrate 3800k -bufsize 7600k {COLOUR} -g 60 -bf 3 {TAGS} \
                 -c:a aac -b:a 128k -movflags +faststart"
            )),
            "{joined}"
        );
        // The cap scales with the pixel count.
        let square = build_args(Path::new("o.mp4"), (1080, 1080), 30, Quality::Small, None);
        assert!(joined_has(&square, "-maxrate 2137k -bufsize 4274k"));
        let high = build_args(Path::new("o.mp4"), (1080, 1080), 30, Quality::High, None);
        assert!(!high.contains(&"-maxrate".to_owned()));
    }

    fn joined_has(args: &[String], part: &str) -> bool {
        joined(args).contains(part)
    }

    #[test]
    fn small_file_fits_a_whatsapp_status() {
        let cap = Quality::Small.max_kbps((1080, 1920)).unwrap();
        // Capped video plus audio over 30 s, with 3% container overhead, under 16 MB.
        let bytes = (cap + 128) as f64 * 1000.0 / 8.0 * 30.0 * 1.03;
        assert!(bytes < 16.0 * 1024.0 * 1024.0, "{bytes}");
        let high = Quality::High.estimate_bytes((1080, 1920), 30.0, true);
        let small = Quality::Small.estimate_bytes((1080, 1920), 30.0, true);
        assert!(small < high);
        assert_eq!(
            Quality::Small.estimate_bytes((1080, 1920), 10.0, false),
            SMALL_TYPICAL_KBPS * 10 * 1000 / 8
        );
    }

    #[test]
    fn fade_out_never_starts_before_zero() {
        let args = build_args(
            Path::new("o.mp4"),
            (2, 2),
            30,
            Quality::High,
            Some((Path::new("a"), Duration::ZERO, Duration::from_millis(500))),
        );
        assert!(args
            .iter()
            .any(|a| a.ends_with("afade=t=out:st=0.000:d=0.8")));
    }
}
