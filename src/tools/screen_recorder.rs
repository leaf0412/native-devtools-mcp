//! Screen recording: captures the frontmost app's window at a configurable
//! frame rate (default 5fps), writing timestamped JPEG frames to an output directory.
//!
//! Follows the same lifecycle pattern as [`super::hover_tracker::HoverTracker`]:
//! a background task captures frames, a shared buffer stores metadata, and the
//! caller drains or stops the session.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::now_millis;

/// Metadata for a single recorded frame.
#[derive(Debug, Clone, Serialize)]
pub struct RecordedFrame {
    /// Absolute Unix timestamp in milliseconds
    pub timestamp_ms: u64,
    /// Path to the JPEG file on disk
    pub path: String,
    /// App that owned the captured window
    pub app_name: String,
    /// Window ID that was captured
    pub window_id: u32,
    /// Screen-space origin of the window (points)
    pub origin_x: f64,
    pub origin_y: f64,
    /// Backing scale factor (pixels per point)
    pub scale: f64,
    /// Pixel dimensions
    pub pixel_width: u32,
    pub pixel_height: u32,
}

/// Active screen recording session.
pub struct ScreenRecorder {
    frames: Arc<Mutex<Vec<RecordedFrame>>>,
    task_handle: JoinHandle<()>,
    cancel: CancellationToken,
}

impl ScreenRecorder {
    pub fn new(
        frames: Arc<Mutex<Vec<RecordedFrame>>>,
        task_handle: JoinHandle<()>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            frames,
            task_handle,
            cancel,
        }
    }

    /// Check if the background task has finished (due to max_duration or error).
    pub fn is_finished(&self) -> bool {
        self.task_handle.is_finished()
    }

    /// Drain all buffered frame metadata, returning them and clearing the buffer.
    pub fn drain_frames(&self) -> Vec<RecordedFrame> {
        let mut frames = self.frames.lock().unwrap();
        frames.drain(..).collect()
    }

    /// Cancel recording, await task shutdown, then drain remaining frames.
    pub async fn cancel_and_drain(mut self) -> Vec<RecordedFrame> {
        self.cancel.cancel();
        if tokio::time::timeout(std::time::Duration::from_secs(2), &mut self.task_handle)
            .await
            .is_err()
        {
            self.task_handle.abort();
        }
        // Mark as already stopped so Drop doesn't cancel again
        self.cancel = CancellationToken::new();
        let mut buf = self.frames.lock().unwrap();
        buf.drain(..).collect()
    }
}

impl Drop for ScreenRecorder {
    fn drop(&mut self) {
        if !self.cancel.is_cancelled() && !self.task_handle.is_finished() {
            tracing::warn!(
                "ScreenRecorder dropped without stop_recording — cancelling background task"
            );
            self.cancel.cancel();
            self.task_handle.abort();
        }
    }
}

/// Start the screen recording background task.
///
/// Each tick captures the frontmost app's window via `CGWindowListCreateImage`,
/// encodes to JPEG on the blocking pool, writes to disk, and pushes a
/// `RecordedFrame` to the shared buffer.
///
/// Stops when `cancel` is triggered or `max_duration_ms` elapses.
pub fn start_recording(
    frames: Arc<Mutex<Vec<RecordedFrame>>>,
    cancel: CancellationToken,
    output_dir: PathBuf,
    fps: u32,
    max_duration_ms: u32,
) -> JoinHandle<()> {
    let fps = fps.clamp(1, 30);
    tokio::spawn(async move {
        let start = Instant::now();
        let max_duration = std::time::Duration::from_millis(max_duration_ms as u64);
        let tick_interval = std::time::Duration::from_millis(1000 / fps as u64);

        let mut interval = tokio::time::interval(tick_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Cache PID→app_name only. Window ID is resolved fresh each tick
        // since the user may switch windows within the same app.
        let mut app_name_cache: std::collections::HashMap<i32, String> =
            std::collections::HashMap::new();
        let output_dir: Arc<Path> = Arc::from(output_dir.as_path());

        loop {
            interval.tick().await;

            if cancel.is_cancelled() || start.elapsed() >= max_duration {
                return;
            }

            let app_cache_snapshot = app_name_cache.clone();
            let dir = output_dir.clone();

            // All OS calls (frontmost_pid, list_windows, CGWindowListCreateImage)
            // run inside spawn_blocking so they have a proper thread context
            // for Objective-C/AppKit APIs (autorelease pools, etc.).
            let result = tokio::task::spawn_blocking(move || {
                capture_frontmost_frame(&app_cache_snapshot, &dir)
            })
            .await;

            match result {
                Ok(Ok((frame, pid, app_name))) => {
                    app_name_cache.insert(pid, app_name);
                    frames.lock().unwrap().push(frame);
                }
                Ok(Err(e)) => {
                    tracing::debug!("Frame capture failed: {e}");
                }
                Err(e) => {
                    tracing::debug!("Frame capture task panicked: {e}");
                }
            }
        }
    })
}

/// Capture a single frame from the frontmost app's window.
///
/// Determines the frontmost app from `CGWindowListCopyWindowInfo` (which
/// returns windows in front-to-back stacking order) rather than
/// `NSWorkspace.frontmostApplication`, because NSWorkspace requires run
/// loop events to stay current and our CLI server never processes them.
///
/// Returns the frame, the frontmost PID, and the app name (for cache update).
fn capture_frontmost_frame(
    app_name_cache: &std::collections::HashMap<i32, String>,
    output_dir: &Path,
) -> Result<(RecordedFrame, i32, String), String> {
    #[cfg(target_os = "macos")]
    {
        use crate::macos;

        // list_windows() returns windows in front-to-back order via
        // CGWindowListCopyWindowInfo.  The first layer-0 window is the
        // frontmost user app — no NSWorkspace needed.
        let windows =
            macos::window::list_windows().map_err(|e| format!("Failed to list windows: {e}"))?;
        let win = windows
            .iter()
            .find(|w| w.layer == 0)
            .ok_or_else(|| "No layer-0 window found".to_string())?;

        let window_id = win.id;
        let pid = win.owner_pid as i32;
        let app_name = app_name_cache
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| win.owner_name.clone());

        let timestamp_ms = now_millis();
        let (jpeg_data, meta) = macos::screenshot::capture_window_cg_jpeg(window_id)
            .map_err(|e| format!("Capture failed: {e}"))?;

        let filename = format!("frame_{timestamp_ms}.jpg");
        let path = output_dir.join(&filename);
        std::fs::write(&path, &jpeg_data).map_err(|e| format!("Failed to write frame: {e}"))?;

        Ok((
            RecordedFrame {
                timestamp_ms,
                path: path.to_string_lossy().to_string(),
                app_name: app_name.clone(),
                window_id,
                origin_x: meta.origin_x,
                origin_y: meta.origin_y,
                scale: meta.scale,
                pixel_width: meta.pixel_width,
                pixel_height: meta.pixel_height,
            },
            pid,
            app_name,
        ))
    }

    #[cfg(target_os = "windows")]
    {
        use crate::windows as win_platform;
        use ::windows::Win32::Foundation::HWND;
        use ::windows::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };

        // Use GetForegroundWindow directly instead of enumerating all windows —
        // much cheaper at 5fps (avoids OpenProcess + QueryFullProcessImageName per window).
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd == HWND::default() {
            return Err("No foreground window".to_string());
        }

        let window_id = hwnd.0 as usize as u32;
        let mut raw_pid: u32 = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut raw_pid));
        }
        let pid = raw_pid as i32;

        let app_name = app_name_cache.get(&pid).cloned().unwrap_or_else(|| {
            // First time seeing this PID — resolve directly instead of enumerating all apps
            crate::windows::app::get_process_name(raw_pid).unwrap_or_default()
        });

        let timestamp_ms = now_millis();
        let (jpeg_data, meta) = win_platform::screenshot::capture_window_jpeg(window_id)
            .map_err(|e| format!("Capture failed: {e}"))?;

        let filename = format!("frame_{timestamp_ms}.jpg");
        let path = output_dir.join(&filename);
        std::fs::write(&path, &jpeg_data).map_err(|e| format!("Failed to write frame: {e}"))?;

        Ok((
            RecordedFrame {
                timestamp_ms,
                path: path.to_string_lossy().to_string(),
                app_name: app_name.clone(),
                window_id,
                origin_x: meta.origin_x,
                origin_y: meta.origin_y,
                scale: meta.scale,
                pixel_width: meta.pixel_width,
                pixel_height: meta.pixel_height,
            },
            pid,
            app_name,
        ))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (app_name_cache, output_dir);
        Err("Screen recording is only supported on macOS and Windows".to_string())
    }
}

// ============================================================================
// MCP tool handlers. Each wraps the screen-recording state machine with its
// name, schema, and availability. `start_recording` is always visible (so the
// user can begin a session); `stop_recording` is gated `WhenRecording`,
// mirroring the deleted `get_recording_tools` + the recording gate in
// `get_tools`. Schema JSON moved verbatim from that getter; call bodies copied
// verbatim from the deleted `call_tool` arms, with `ctx.screen_recorder` /
// `ctx.peer` replacing `self.screen_recorder` / `context.peer`. Every
// `notify_tool_list_changed` is preserved so the visible tool set mutates on
// start/stop.
// ============================================================================

use crate::tools::registry::{
    json_to_object, parse_string_field, to_json_pretty, Availability, ToolContext, ToolHandler,
};
use rmcp::{
    model::{CallToolResult, Content, Tool},
    Error as McpError,
};

/// `start_recording` — always visible so a session can be started. Fires
/// `notify_tool_list_changed` so `stop_recording` becomes visible once recording.
pub struct StartRecording;

#[async_trait::async_trait]
impl ToolHandler for StartRecording {
    fn name(&self) -> &'static str {
        "start_recording"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "start_recording",
            "Start recording the frontmost app's window at ~5fps. Writes timestamped JPEG frames to the specified output directory. Use stop_recording to end the session and get the frame list.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {
                    "output_dir": {
                        "type": "string",
                        "description": "Directory to write JPEG frames to (created if needed)"
                    },
                    "fps": {
                        "type": "integer",
                        "description": "Frames per second (default: 5)",
                        "default": 5
                    },
                    "max_duration_ms": {
                        "type": "integer",
                        "description": "Auto-stop after this many milliseconds (default: 60000 = 1 min)",
                        "default": 60000
                    }
                },
                "required": ["output_dir"]
            }))),
        )
    }

    async fn call(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<CallToolResult, McpError> {
        // Check if a previous recording auto-stopped (max_duration elapsed).
        // If so, drain remaining frames (log count) and clear stale state.
        {
            let guard = ctx.screen_recorder.read().await;
            if let Some(recorder) = guard.as_ref() {
                if recorder.is_finished() {
                    let stale_count = recorder.drain_frames().len();
                    if stale_count > 0 {
                        tracing::warn!(
                            "Discarding {stale_count} frames from auto-stopped recording \
                             (stop_recording was not called)"
                        );
                    }
                    drop(guard);
                    ctx.screen_recorder.write().await.take();
                    let _ = ctx.peer.notify_tool_list_changed().await;
                } else {
                    return Ok(CallToolResult::error(vec![Content::text(
                        "Recording is already active. Use stop_recording to end the current session first.",
                    )]));
                }
            }
        }

        let output_dir = parse_string_field(&args, "output_dir")?;
        let fps = args.get("fps").and_then(|v| v.as_u64()).unwrap_or(5) as u32;
        let max_duration_ms = args
            .get("max_duration_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(60_000)
            .clamp(1_000, u32::MAX as u64) as u32;

        let output_path = std::path::PathBuf::from(&output_dir);
        if let Err(e) = std::fs::create_dir_all(&output_path) {
            return Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to create output directory: {e}"
            ))]));
        }
        // Probe-write to fail fast if the directory is not writable.
        match tempfile::tempfile_in(&output_path) {
            Ok(_) => {} // drops and deletes automatically
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Output directory is not writable: {e}"
                ))]));
            }
        }

        let frames = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cancel = tokio_util::sync::CancellationToken::new();

        let task_handle = crate::tools::screen_recorder::start_recording(
            frames.clone(),
            cancel.clone(),
            output_path,
            fps,
            max_duration_ms,
        );

        let recorder =
            crate::tools::screen_recorder::ScreenRecorder::new(frames, task_handle, cancel);
        *ctx.screen_recorder.write().await = Some(recorder);
        let _ = ctx.peer.notify_tool_list_changed().await;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Recording started ({fps}fps, max: {max_duration_ms}ms, dir: {output_dir}). Use stop_recording to end.",
        ))]))
    }
}

/// `stop_recording` — visible only while recording. Cancels the background task,
/// drains remaining frames, and fires `notify_tool_list_changed`.
pub struct StopRecording;

#[async_trait::async_trait]
impl ToolHandler for StopRecording {
    fn name(&self) -> &'static str {
        "stop_recording"
    }

    fn availability(&self) -> Availability {
        Availability::WhenRecording
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "stop_recording",
            "Stop screen recording and return all frame metadata as a JSON array.",
            Arc::new(json_to_object(serde_json::json!({
                "type": "object",
                "properties": {}
            }))),
        )
    }

    async fn call(
        &self,
        _args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<CallToolResult, McpError> {
        let recorder = ctx.screen_recorder.write().await.take();
        match recorder {
            Some(recorder) => {
                let frames = recorder.cancel_and_drain().await;
                let _ = ctx.peer.notify_tool_list_changed().await;
                Ok(CallToolResult::success(vec![Content::text(
                    to_json_pretty(&frames),
                )]))
            }
            None => Ok(CallToolResult::error(vec![Content::text(
                "No recording session is active.",
            )])),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recorded_frame_serialization() {
        let frame = RecordedFrame {
            timestamp_ms: 1710400000000,
            path: "/tmp/frame_1710400000000.jpg".to_string(),
            app_name: "Finder".to_string(),
            window_id: 42,
            origin_x: 100.0,
            origin_y: 200.0,
            scale: 2.0,
            pixel_width: 1920,
            pixel_height: 1080,
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains("\"timestamp_ms\":1710400000000"));
        assert!(json.contains("\"app_name\":\"Finder\""));
        assert!(json.contains("\"window_id\":42"));
    }

    #[test]
    fn test_drain_frames_clears_buffer() {
        let frames = Arc::new(Mutex::new(vec![RecordedFrame {
            timestamp_ms: 1000,
            path: "/tmp/f.jpg".to_string(),
            app_name: "Test".to_string(),
            window_id: 1,
            origin_x: 0.0,
            origin_y: 0.0,
            scale: 2.0,
            pixel_width: 100,
            pixel_height: 100,
        }]));
        let cancel = CancellationToken::new();
        let recorder = ScreenRecorder::new(
            frames.clone(),
            tokio::runtime::Runtime::new().unwrap().spawn(async {}),
            cancel,
        );

        let drained = recorder.drain_frames();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].timestamp_ms, 1000);

        let drained2 = recorder.drain_frames();
        assert!(drained2.is_empty());
    }

    #[tokio::test]
    async fn test_start_recording_cancellation() {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let cancel = CancellationToken::new();
        let dir = tempfile::tempdir().unwrap();

        let handle = start_recording(
            frames.clone(),
            cancel.clone(),
            dir.path().to_path_buf(),
            3,
            10000,
        );

        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_millis(500), handle)
            .await
            .expect("task should finish after cancel")
            .expect("task should not panic");
    }

    #[tokio::test]
    #[ignore] // requires a running desktop
    async fn test_recording_captures_frames() {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let cancel = CancellationToken::new();
        let dir = tempfile::tempdir().unwrap();

        let handle = start_recording(
            frames.clone(),
            cancel.clone(),
            dir.path().to_path_buf(),
            3,
            10000,
        );

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        cancel.cancel();
        let _ = handle.await;

        let captured = frames.lock().unwrap();
        eprintln!("Captured {} frames", captured.len());
        for f in captured.iter() {
            eprintln!(
                "  {} wid={} {}x{} {}",
                f.app_name, f.window_id, f.pixel_width, f.pixel_height, f.path
            );
        }

        // List files on disk
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let entry = entry.unwrap();
            eprintln!(
                "  file: {:?} ({} bytes)",
                entry.file_name(),
                entry.metadata().unwrap().len()
            );
        }

        assert!(
            !captured.is_empty(),
            "Should have captured at least one frame"
        );
    }
}
