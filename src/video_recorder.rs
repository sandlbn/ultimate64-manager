//! Video recording module using ffmpeg subprocess
//!
//! Frame-synchronized recording: audio is recorded in sync with video frames
//! to ensure perfect A/V sync without drift.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

/// Default frame rate for PAL C64 (50 Hz)
pub const DEFAULT_FPS: u32 = 50;

/// Ultimate64 PAL audio sample rate
pub const AUDIO_SAMPLE_RATE_PAL: u32 = 47983;

/// Default audio channels (stereo)
pub const DEFAULT_CHANNELS: u32 = 2;

/// Cached ffmpeg path
static FFMPEG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Find ffmpeg binary
fn find_ffmpeg() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let common_paths: &[&str] = &[
        "/opt/homebrew/bin/ffmpeg",
        "/usr/local/bin/ffmpeg",
        "/opt/local/bin/ffmpeg",
    ];

    #[cfg(target_os = "linux")]
    let common_paths: &[&str] = &[
        "/usr/bin/ffmpeg",
        "/usr/local/bin/ffmpeg",
        "/snap/bin/ffmpeg",
    ];

    #[cfg(target_os = "windows")]
    let common_paths: &[&str] = &[
        "C:\\Program Files\\ffmpeg\\bin\\ffmpeg.exe",
        "C:\\ffmpeg\\bin\\ffmpeg.exe",
    ];

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let common_paths: &[&str] = &["/usr/bin/ffmpeg", "/usr/local/bin/ffmpeg"];

    // Try PATH first
    #[cfg(unix)]
    if let Ok(output) = Command::new("which").arg("ffmpeg").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }

    #[cfg(windows)]
    if let Ok(output) = Command::new("where").arg("ffmpeg").output() {
        if output.status.success() {
            if let Some(path) = String::from_utf8_lossy(&output.stdout).lines().next() {
                return Some(PathBuf::from(path.trim()));
            }
        }
    }

    // Check common paths
    for path in common_paths {
        let p = PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }

    // Try direct execution
    if Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
    {
        return Some(PathBuf::from("ffmpeg"));
    }

    None
}

fn get_ffmpeg_path() -> Result<&'static PathBuf, String> {
    FFMPEG_PATH
        .get_or_init(find_ffmpeg)
        .as_ref()
        .ok_or_else(|| "ffmpeg not found".to_string())
}

// ============================================================================
// Audio Buffer for Recording - Separate from playback jitter buffer
// ============================================================================

/// Thread-safe audio sample buffer for recording
/// Audio thread pushes samples, video thread pulls them frame-synchronized
pub struct RecordingAudioBuffer {
    samples: VecDeque<f32>,
    total_samples_received: u64,
    last_left: f32, // For interpolation on underrun
    last_right: f32,
}

impl RecordingAudioBuffer {
    pub fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(48000 * 2 * 2), // 2 seconds buffer
            total_samples_received: 0,
            last_left: 0.0,
            last_right: 0.0,
        }
    }

    /// Push audio samples (called from audio network thread)
    pub fn push_samples(&mut self, samples: &[f32]) {
        const MAX_BUFFER: usize = 48000 * 2 * 5; // 5 seconds max

        if self.samples.len() + samples.len() > MAX_BUFFER {
            let to_drop = (self.samples.len() + samples.len()) - MAX_BUFFER;
            for _ in 0..to_drop {
                self.samples.pop_front();
            }
        }

        self.samples.extend(samples.iter().copied());
        self.total_samples_received += samples.len() as u64;

        // Update last samples for interpolation
        if samples.len() >= 2 {
            self.last_left = samples[samples.len() - 2];
            self.last_right = samples[samples.len() - 1];
        }
    }

    /// Pull exactly N samples for a video frame
    /// On underrun: repeats last known sample instead of silence (avoids pops)
    pub fn pull_samples(&mut self, count: usize) -> Vec<f32> {
        let mut result = Vec::with_capacity(count);

        let available = self.samples.len();

        if available >= count {
            // Normal case: enough samples
            for _ in 0..count {
                result.push(self.samples.pop_front().unwrap());
            }
            // Update last samples
            if count >= 2 {
                self.last_left = result[count - 2];
                self.last_right = result[count - 1];
            }
        } else {
            // Underrun: take what we have, then repeat last sample (stereo pairs)
            for _ in 0..available {
                result.push(self.samples.pop_front().unwrap());
            }

            // Fill remaining with last known samples (avoids pops)
            let remaining = count - available;
            for i in 0..remaining {
                if i % 2 == 0 {
                    result.push(self.last_left);
                } else {
                    result.push(self.last_right);
                }
            }

            if remaining > 0 {
                log::trace!(
                    "Audio underrun: needed {}, had {}, filled {}",
                    count,
                    available,
                    remaining
                );
            }
        }

        result
    }

    /// Get current buffer level (in stereo frames)
    pub fn buffer_frames(&self) -> usize {
        self.samples.len() / 2
    }

    /// Check if buffer has minimum samples for recording
    pub fn is_ready(&self, min_frames: usize) -> bool {
        self.samples.len() >= min_frames * 2
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.last_left = 0.0;
        self.last_right = 0.0;
    }
}

pub type SharedRecordingAudioBuffer = Arc<Mutex<RecordingAudioBuffer>>;

pub fn create_recording_audio_buffer() -> SharedRecordingAudioBuffer {
    Arc::new(Mutex::new(RecordingAudioBuffer::new()))
}

// ============================================================================
// Frame-Synchronized Video Recorder
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderState {
    Idle,
    Recording,
    Finalizing,
}

/// Frame-synchronized video recorder
///
/// Records video and audio with perfect sync by pulling audio samples
/// for each video frame based on the frame rate.
pub struct VideoRecorder {
    ffmpeg_path: PathBuf,
    ffmpeg_video: Option<Child>,
    video_pipe: Option<std::process::ChildStdin>,
    audio_file: Option<BufWriter<File>>,
    audio_temp_path: PathBuf,
    temp_video_path: PathBuf,
    final_output_path: PathBuf,

    // Recording stats
    frame_count: u64,
    audio_samples_written: u64,

    // Video settings
    width: u32,
    height: u32,
    fps: u32,

    // Audio settings
    sample_rate: u32,
    channels: u32,
    audio_enabled: bool,

    // Frame sync: how many audio samples per video frame
    samples_per_frame: f64,
    audio_sample_accumulator: f64,

    state: RecorderState,
}

impl VideoRecorder {
    pub fn init() -> Result<(), String> {
        let path = get_ffmpeg_path()?;
        let output = Command::new(path)
            .arg("-version")
            .output()
            .map_err(|e| format!("ffmpeg error: {}", e))?;

        if output.status.success() {
            if let Some(line) = String::from_utf8_lossy(&output.stdout).lines().next() {
                log::info!("Using {}", line);
            }
            Ok(())
        } else {
            Err("ffmpeg not working".to_string())
        }
    }

    pub fn new() -> Result<Self, String> {
        let ffmpeg_path = get_ffmpeg_path()?.clone();

        Ok(Self {
            ffmpeg_path,
            ffmpeg_video: None,
            video_pipe: None,
            audio_file: None,
            audio_temp_path: PathBuf::new(),
            temp_video_path: PathBuf::new(),
            final_output_path: PathBuf::new(),
            frame_count: 0,
            audio_samples_written: 0,
            width: 0,
            height: 0,
            fps: DEFAULT_FPS,
            sample_rate: AUDIO_SAMPLE_RATE_PAL,
            channels: DEFAULT_CHANNELS,
            audio_enabled: false,
            samples_per_frame: 0.0,
            audio_sample_accumulator: 0.0,
            state: RecorderState::Idle,
        })
    }

    /// Start recording with audio
    pub fn start_with_audio(
        &mut self,
        output_path: PathBuf,
        width: u32,
        height: u32,
        fps: u32,
        sample_rate: u32,
        channels: u32,
        audio_enabled: bool,
    ) -> Result<(), String> {
        if self.state != RecorderState::Idle {
            return Err("Already recording".to_string());
        }

        if width == 0 || height == 0 || fps == 0 {
            return Err("Invalid parameters".to_string());
        }

        self.width = width;
        self.height = height;
        self.fps = fps;
        self.sample_rate = sample_rate;
        self.channels = channels;
        self.audio_enabled = audio_enabled;
        self.final_output_path = output_path;
        self.frame_count = 0;
        self.audio_samples_written = 0;

        // Calculate samples per frame for sync
        // e.g., 47983 Hz / 50 fps = 959.66 mono samples per frame
        // For stereo: 959.66 * 2 = 1919.32 samples per frame
        self.samples_per_frame = (sample_rate as f64 / fps as f64) * channels as f64;
        self.audio_sample_accumulator = 0.0;

        log::info!(
            "Samples per frame: {:.2} ({} Hz / {} fps * {} ch)",
            self.samples_per_frame,
            sample_rate,
            fps,
            channels
        );

        // Create temp paths
        let temp_dir = std::env::temp_dir();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let pid = std::process::id();

        self.temp_video_path = temp_dir.join(format!("u64_vid_{}_{}.mp4", pid, ts));
        self.audio_temp_path = temp_dir.join(format!("u64_aud_{}_{}.raw", pid, ts));

        // Video output path
        let video_output = if audio_enabled {
            &self.temp_video_path
        } else {
            &self.final_output_path
        };

        if let Some(parent) = video_output.parent() {
            fs::create_dir_all(parent).ok();
        }

        // Start ffmpeg for video
        let mut child = Command::new(&self.ffmpeg_path)
            .args([
                "-y",
                "-f",
                "rawvideo",
                "-pixel_format",
                "rgba",
                "-video_size",
                &format!("{}x{}", width, height),
                "-framerate",
                &fps.to_string(),
                "-i",
                "-",
                "-c:v",
                "libx264",
                "-preset",
                "fast",
                "-crf",
                "23",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
            ])
            .arg(video_output)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to start ffmpeg: {}", e))?;

        self.video_pipe = child.stdin.take();
        self.ffmpeg_video = Some(child);

        // Setup audio file if enabled
        if audio_enabled {
            let f = File::create(&self.audio_temp_path)
                .map_err(|e| format!("Failed to create audio file: {}", e))?;
            self.audio_file = Some(BufWriter::with_capacity(1024 * 1024, f));
        }

        self.state = RecorderState::Recording;

        log::info!(
            "Recording started: {}x{} @ {} fps, audio: {}",
            width,
            height,
            fps,
            audio_enabled
        );

        Ok(())
    }

    /// Write a video frame with synchronized audio
    ///
    /// This is the key method: for each video frame, we also write the
    /// corresponding audio samples to maintain perfect sync.
    pub fn write_frame_with_audio(
        &mut self,
        rgba_data: &[u8],
        audio_buffer: &SharedRecordingAudioBuffer,
    ) -> Result<(), String> {
        if self.state != RecorderState::Recording {
            return Err("Not recording".to_string());
        }

        // Write video frame
        let pipe = self.video_pipe.as_mut().ok_or("No video pipe")?;

        let expected = (self.width * self.height * 4) as usize;
        if rgba_data.len() != expected {
            return Err(format!(
                "Bad frame size: {} vs {}",
                rgba_data.len(),
                expected
            ));
        }

        pipe.write_all(rgba_data)
            .map_err(|e| format!("Video write error: {}", e))?;

        self.frame_count += 1;

        // Write synchronized audio
        if self.audio_enabled {
            // Calculate how many samples to write for this frame
            // Use accumulator to handle fractional samples correctly
            self.audio_sample_accumulator += self.samples_per_frame;
            let samples_to_write = self.audio_sample_accumulator.floor() as usize;
            self.audio_sample_accumulator -= samples_to_write as f64;

            // Pull samples from the recording buffer
            let samples = if let Ok(mut buf) = audio_buffer.lock() {
                buf.pull_samples(samples_to_write)
            } else {
                vec![0.0; samples_to_write] // Silence on lock failure
            };

            // Write to audio file
            if let Some(ref mut file) = self.audio_file {
                for &s in &samples {
                    let i16_val = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                    file.write_all(&i16_val.to_le_bytes())
                        .map_err(|e| format!("Audio write error: {}", e))?;
                }
                self.audio_samples_written += samples.len() as u64;
            }
        }

        if self.frame_count % 500 == 0 {
            log::debug!(
                "Recording: {} frames, {} audio samples",
                self.frame_count,
                self.audio_samples_written
            );
        }

        Ok(())
    }

    /// Write video frame only (no audio sync)
    pub fn write_frame_rgba(&mut self, rgba_data: &[u8]) -> Result<(), String> {
        if self.state != RecorderState::Recording {
            return Err("Not recording".to_string());
        }

        let pipe = self.video_pipe.as_mut().ok_or("No video pipe")?;

        let expected = (self.width * self.height * 4) as usize;
        if rgba_data.len() != expected {
            return Err(format!(
                "Bad frame size: {} vs {}",
                rgba_data.len(),
                expected
            ));
        }

        pipe.write_all(rgba_data)
            .map_err(|e| format!("Video write error: {}", e))?;

        self.frame_count += 1;
        Ok(())
    }

    /// Stop recording and finalize
    pub fn stop(&mut self) -> Result<String, String> {
        if self.state != RecorderState::Recording {
            return Err("Not recording".to_string());
        }

        self.state = RecorderState::Finalizing;

        // Close pipes
        self.video_pipe = None;
        if let Some(mut f) = self.audio_file.take() {
            f.flush().ok();
        }

        // Wait for video encoder
        if let Some(proc) = self.ffmpeg_video.take() {
            let output = proc
                .wait_with_output()
                .map_err(|e| format!("ffmpeg wait error: {}", e))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                self.cleanup();
                self.state = RecorderState::Idle;
                return Err(format!(
                    "Video encoding failed: {}",
                    stderr.lines().last().unwrap_or("unknown")
                ));
            }
        }

        // Mux if audio was recorded
        if self.audio_enabled && self.audio_samples_written > 0 {
            log::info!(
                "Muxing {} video frames with {} audio samples...",
                self.frame_count,
                self.audio_samples_written
            );

            let result = Command::new(&self.ffmpeg_path)
                .args([
                    "-y",
                    "-i",
                    self.temp_video_path.to_str().unwrap(),
                    "-f",
                    "s16le",
                    "-ar",
                    &self.sample_rate.to_string(),
                    "-ac",
                    &self.channels.to_string(),
                    "-i",
                    self.audio_temp_path.to_str().unwrap(),
                    "-c:v",
                    "copy",
                    "-c:a",
                    "aac",
                    "-ar",
                    "48000",
                    "-b:a",
                    "192k",
                    "-shortest",
                    "-movflags",
                    "+faststart",
                ])
                .arg(&self.final_output_path)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|e| format!("Mux error: {}", e))?;

            self.cleanup();

            if !result.status.success() {
                let stderr = String::from_utf8_lossy(&result.stderr);
                self.state = RecorderState::Idle;
                return Err(format!(
                    "Mux failed: {}",
                    stderr.lines().last().unwrap_or("unknown")
                ));
            }
        }

        let path = self.final_output_path.to_string_lossy().to_string();
        let video_secs = self.frame_count as f64 / self.fps as f64;
        let audio_secs =
            self.audio_samples_written as f64 / (self.sample_rate as f64 * self.channels as f64);

        log::info!(
            "Recording complete: {:.1}s video, {:.1}s audio -> {}",
            video_secs,
            audio_secs,
            path
        );

        self.frame_count = 0;
        self.audio_samples_written = 0;
        self.audio_enabled = false;
        self.state = RecorderState::Idle;

        Ok(path)
    }

    pub fn cancel(&mut self) {
        self.video_pipe = None;
        self.audio_file = None;

        if let Some(mut p) = self.ffmpeg_video.take() {
            let _ = p.kill();
            let _ = p.wait();
        }

        self.cleanup();

        if self.final_output_path.exists() {
            let _ = fs::remove_file(&self.final_output_path);
        }

        self.state = RecorderState::Idle;
        log::info!("Recording cancelled");
    }

    fn cleanup(&self) {
        let _ = fs::remove_file(&self.temp_video_path);
        let _ = fs::remove_file(&self.audio_temp_path);
    }

    pub fn is_recording(&self) -> bool {
        self.state == RecorderState::Recording
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }
}

impl Default for VideoRecorder {
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| Self {
            ffmpeg_path: PathBuf::from("ffmpeg"),
            ffmpeg_video: None,
            video_pipe: None,
            audio_file: None,
            audio_temp_path: PathBuf::new(),
            temp_video_path: PathBuf::new(),
            final_output_path: PathBuf::new(),
            frame_count: 0,
            audio_samples_written: 0,
            width: 0,
            height: 0,
            fps: DEFAULT_FPS,
            sample_rate: AUDIO_SAMPLE_RATE_PAL,
            channels: DEFAULT_CHANNELS,
            audio_enabled: false,
            samples_per_frame: 0.0,
            audio_sample_accumulator: 0.0,
            state: RecorderState::Idle,
        })
    }
}

impl Drop for VideoRecorder {
    fn drop(&mut self) {
        if self.state == RecorderState::Recording {
            self.cancel();
        }
        self.cleanup();
    }
}

pub type SharedRecorder = Arc<Mutex<VideoRecorder>>;

pub fn create_shared_recorder() -> SharedRecorder {
    Arc::new(Mutex::new(VideoRecorder::default()))
}

pub fn generate_recording_path() -> Result<PathBuf, String> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();

    let base = dirs::video_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));

    let dir = base.join("Ultimate64");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    Ok(dir.join(format!("u64_recording_{}.mp4", ts)))
}
