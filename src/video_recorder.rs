//! Video recording module using ffmpeg subprocess
//!
//! Records video frames to a temp file, audio to another temp file,
//! then muxes them together when stopping.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

/// Default frame rate for PAL C64 (50 Hz)
pub const DEFAULT_FPS: u32 = 50;

/// Default audio sample rate - Ultimate64 PAL rate
pub const DEFAULT_SAMPLE_RATE: u32 = 47983;

/// Default audio channels (stereo)
pub const DEFAULT_CHANNELS: u32 = 2;

/// Video recorder using ffmpeg subprocess with audio support
pub struct VideoRecorder {
    // Video encoding (live)
    ffmpeg_video: Option<Child>,
    video_pipe: Option<std::process::ChildStdin>,

    // Audio written to temp file (muxed at end)
    audio_file: Option<BufWriter<File>>,
    audio_temp_path: PathBuf,

    // Recording state
    temp_video_path: PathBuf,
    final_output_path: PathBuf,
    frame_count: u64,
    audio_sample_count: u64,
    width: u32,
    height: u32,
    fps: u32,
    sample_rate: u32,
    channels: u32,
    audio_enabled: bool,
}

impl VideoRecorder {
    /// Initialize (verify ffmpeg is available)
    pub fn init() -> Result<(), String> {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| "ffmpeg not found. Please install ffmpeg.".to_string())?;
        Ok(())
    }

    /// Create a new recorder (not yet recording)
    pub fn new() -> Self {
        Self {
            ffmpeg_video: None,
            video_pipe: None,
            audio_file: None,
            audio_temp_path: PathBuf::new(),
            temp_video_path: PathBuf::new(),
            final_output_path: PathBuf::new(),
            frame_count: 0,
            audio_sample_count: 0,
            width: 0,
            height: 0,
            fps: DEFAULT_FPS,
            sample_rate: DEFAULT_SAMPLE_RATE,
            channels: DEFAULT_CHANNELS,
            audio_enabled: false,
        }
    }

    /// Start recording with audio support
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
        if self.ffmpeg_video.is_some() {
            return Err("Already recording".to_string());
        }

        self.width = width;
        self.height = height;
        self.final_output_path = output_path.clone();
        self.fps = fps;
        self.sample_rate = sample_rate;
        self.channels = channels;
        self.frame_count = 0;
        self.audio_sample_count = 0;
        self.audio_enabled = audio_enabled;

        // Create temp paths
        let temp_dir = std::env::temp_dir();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();

        self.temp_video_path = temp_dir.join(format!("u64_video_{}.mp4", timestamp));
        self.audio_temp_path = temp_dir.join(format!("u64_audio_{}.raw", timestamp));

        // Start video encoding to temp file
        let video_output = if audio_enabled {
            // If audio enabled, write to temp file (will mux later)
            self.temp_video_path.clone()
        } else {
            // No audio, write directly to final output
            output_path
        };

        let mut child = Command::new("ffmpeg")
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
                video_output.to_str().unwrap(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to start ffmpeg: {}", e))?;

        self.video_pipe = child.stdin.take();
        self.ffmpeg_video = Some(child);

        // Set up audio file if enabled
        if audio_enabled {
            let audio_file = File::create(&self.audio_temp_path)
                .map_err(|e| format!("Failed to create audio temp file: {}", e))?;
            self.audio_file = Some(BufWriter::with_capacity(1024 * 1024, audio_file)); // 1MB buffer

            log::info!(
                "Started recording to {:?} ({}x{} @ {} fps, audio: {}Hz {}ch)",
                self.final_output_path,
                width,
                height,
                fps,
                sample_rate,
                channels
            );
        } else {
            log::info!(
                "Started recording to {:?} ({}x{} @ {} fps, no audio)",
                self.final_output_path,
                width,
                height,
                fps
            );
        }

        Ok(())
    }

    /// Write an RGBA video frame
    pub fn write_frame_rgba(&mut self, rgba_data: &[u8]) -> Result<(), String> {
        let pipe = self.video_pipe.as_mut().ok_or("Not recording")?;

        let expected_size = (self.width * self.height * 4) as usize;
        if rgba_data.len() != expected_size {
            return Err(format!(
                "Invalid RGBA buffer size: expected {}, got {}",
                expected_size,
                rgba_data.len()
            ));
        }

        pipe.write_all(rgba_data)
            .map_err(|e| format!("Failed to write video frame: {}", e))?;

        self.frame_count += 1;

        if self.frame_count % 500 == 0 {
            log::debug!("Recorded {} video frames", self.frame_count);
        }

        Ok(())
    }

    /// Write audio samples (f32 normalized -1.0 to 1.0, interleaved stereo)
    pub fn write_audio_f32(&mut self, samples: &[f32]) -> Result<(), String> {
        if !self.audio_enabled {
            return Ok(());
        }

        let file = self.audio_file.as_mut().ok_or("Audio not recording")?;

        // Convert f32 samples to i16 bytes (little-endian)
        for &s in samples {
            let clamped = s.clamp(-1.0, 1.0);
            let i16_val = (clamped * 32767.0) as i16;
            file.write_all(&i16_val.to_le_bytes())
                .map_err(|e| format!("Failed to write audio: {}", e))?;
        }

        self.audio_sample_count += samples.len() as u64 / self.channels as u64;

        Ok(())
    }

    /// Check if audio recording is enabled
    pub fn is_audio_enabled(&self) -> bool {
        self.audio_enabled
    }

    /// Stop recording and finalize the file
    pub fn stop(&mut self) -> Result<String, String> {
        // Close video pipe to signal EOF
        self.video_pipe = None;

        // Flush and close audio file
        if let Some(mut audio_file) = self.audio_file.take() {
            audio_file.flush().ok();
        }

        // Wait for video ffmpeg to finish
        if let Some(process) = self.ffmpeg_video.take() {
            let output = process
                .wait_with_output()
                .map_err(|e| format!("Failed to wait for ffmpeg: {}", e))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log::error!("ffmpeg video error: {}", stderr);
                self.cleanup_temp_files();
                return Err(format!("ffmpeg failed: {}", stderr));
            }
        }

        let final_path = self.final_output_path.to_string_lossy().to_string();

        // If audio was enabled, mux video and audio together
        if self.audio_enabled && self.audio_sample_count > 0 {
            log::info!("Muxing video and audio...");

            let mux_result = Command::new("ffmpeg")
                .args([
                    "-y",
                    // Video input
                    "-i",
                    self.temp_video_path.to_str().unwrap(),
                    // Audio input (raw PCM)
                    "-f",
                    "s16le",
                    "-ar",
                    &self.sample_rate.to_string(),
                    "-ac",
                    &self.channels.to_string(),
                    "-i",
                    self.audio_temp_path.to_str().unwrap(),
                    // Copy video stream
                    "-c:v",
                    "copy",
                    // Encode audio
                    "-c:a",
                    "aac",
                    "-ar",
                    "48000",
                    "-b:a",
                    "192k",
                    // Use shortest stream
                    "-shortest",
                    "-movflags",
                    "+faststart",
                    self.final_output_path.to_str().unwrap(),
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|e| format!("Failed to run mux: {}", e))?;

            self.cleanup_temp_files();

            if !mux_result.status.success() {
                let stderr = String::from_utf8_lossy(&mux_result.stderr);
                log::error!("ffmpeg mux error: {}", stderr);
                return Err(format!("Muxing failed: {}", stderr));
            }

            let video_duration = self.frame_count as f64 / self.fps as f64;
            let audio_duration = self.audio_sample_count as f64 / self.sample_rate as f64;
            log::info!(
                "Recording complete: {} frames ({:.2}s video), {:.2}s audio -> {}",
                self.frame_count,
                video_duration,
                audio_duration,
                final_path
            );
        } else {
            let video_duration = self.frame_count as f64 / self.fps as f64;
            log::info!(
                "Recording complete: {} frames ({:.2}s) -> {}",
                self.frame_count,
                video_duration,
                final_path
            );
        }

        self.frame_count = 0;
        self.audio_sample_count = 0;
        self.audio_enabled = false;

        Ok(final_path)
    }

    fn cleanup_temp_files(&self) {
        let _ = fs::remove_file(&self.temp_video_path);
        let _ = fs::remove_file(&self.audio_temp_path);
    }

    pub fn is_recording(&self) -> bool {
        self.ffmpeg_video.is_some()
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }
}

impl Default for VideoRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for VideoRecorder {
    fn drop(&mut self) {
        if self.ffmpeg_video.is_some() {
            if let Err(e) = self.stop() {
                log::error!("Error stopping recorder on drop: {}", e);
            }
        }
        self.cleanup_temp_files();
    }
}

pub type SharedRecorder = Arc<Mutex<VideoRecorder>>;

pub fn create_shared_recorder() -> SharedRecorder {
    Arc::new(Mutex::new(VideoRecorder::new()))
}

pub fn generate_recording_path() -> Result<PathBuf, String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("Time error: {}", e))?
        .as_secs();

    let base_dir = dirs::video_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));

    let output_dir = base_dir.join("Ultimate64");

    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    let filename = format!("u64_recording_{}.mp4", timestamp);
    Ok(output_dir.join(filename))
}
