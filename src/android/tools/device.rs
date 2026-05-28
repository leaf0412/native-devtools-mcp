//! Device connection, screenshot, and input tool handlers.

use super::with_android_device;
use crate::android::AndroidDevice;
use crate::tools::registry::{
    parse_string_field, parse_xy, to_json_pretty, Availability, ToolContext, ToolHandler,
};
use base64::Engine;
use rmcp::{
    model::{CallToolResult, Content, Tool},
    Error as McpError,
};
use serde_json::Value;
use std::sync::Arc;

/// `android_list_devices` — always visible so the user can discover devices.
pub struct AndroidListDevices;

#[async_trait::async_trait]
impl ToolHandler for AndroidListDevices {
    fn name(&self) -> &'static str {
        "android_list_devices"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "android_list_devices",
            "List all Android devices connected via ADB.",
            Arc::new(crate::tools::registry::json_to_object(serde_json::json!({
                "type": "object",
                "properties": {}
            }))),
        )
    }

    async fn call(&self, _args: Value, _ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        match crate::android::device::list_devices() {
            Ok(devices) => Ok(CallToolResult::success(vec![Content::text(
                to_json_pretty(&devices),
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
        }
    }
}

/// `android_connect` — always visible so a disconnected client can connect.
/// Fires `notify_tool_list_changed` so the connected device's tools appear.
pub struct AndroidConnect;

#[async_trait::async_trait]
impl ToolHandler for AndroidConnect {
    fn name(&self) -> &'static str {
        "android_connect"
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "android_connect",
            "Connect to an Android device by its serial number. Use android_list_devices to find available devices.",
            Arc::new(crate::tools::registry::json_to_object(serde_json::json!({
                "type": "object",
                "required": ["serial"],
                "properties": {
                    "serial": {
                        "type": "string",
                        "description": "Device serial number (e.g., 'emulator-5554' or a USB device serial)"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let serial = parse_string_field(&args, "serial")?;
        match AndroidDevice::connect(&serial) {
            Ok(device) => {
                let msg = format!(
                    "Connected to Android device '{}'. Android tools (android_*) are now available.",
                    device.serial
                );
                *ctx.android_device.write().await = Some(device);
                let _ = ctx.peer.notify_tool_list_changed().await;
                Ok(CallToolResult::success(vec![Content::text(msg)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
        }
    }
}

/// `android_disconnect` — visible only while connected. Fires
/// `notify_tool_list_changed` so the device tools disappear.
pub struct AndroidDisconnect;

#[async_trait::async_trait]
impl ToolHandler for AndroidDisconnect {
    fn name(&self) -> &'static str {
        "android_disconnect"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAndroidConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "android_disconnect",
            "Disconnect from the current Android device.",
            Arc::new(crate::tools::registry::json_to_object(serde_json::json!({
                "type": "object",
                "properties": {}
            }))),
        )
    }

    async fn call(&self, _args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        if ctx.android_device.write().await.take().is_some() {
            let _ = ctx.peer.notify_tool_list_changed().await;
            Ok(CallToolResult::success(vec![Content::text(
                "Disconnected from Android device. Android tools (android_*) are no longer available.",
            )]))
        } else {
            Ok(CallToolResult::error(vec![Content::text(
                "No Android device connected.",
            )]))
        }
    }
}

/// `android_screenshot` — visible only while connected.
pub struct AndroidScreenshot;

#[async_trait::async_trait]
impl ToolHandler for AndroidScreenshot {
    fn name(&self) -> &'static str {
        "android_screenshot"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAndroidConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "android_screenshot",
            "Take a screenshot of the Android device screen. Returns a base64-encoded JPEG image.",
            Arc::new(crate::tools::registry::json_to_object(serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Optional file path to save the screenshot PNG to instead of returning it inline."
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(String::from);
        Ok(with_android_device(ctx.android_device.clone(), |device| {
            let shot = match crate::android::screenshot::capture(device) {
                Ok(s) => s,
                Err(e) => return CallToolResult::error(vec![Content::text(e)]),
            };
            if let Some(ref path) = file_path {
                return match std::fs::write(path, &shot.png_data) {
                    Ok(()) => CallToolResult::success(vec![Content::text(format!(
                        "Screenshot saved to {} ({}x{})",
                        path, shot.width, shot.height
                    ))]),
                    Err(e) => CallToolResult::error(vec![Content::text(format!(
                        "Failed to save screenshot: {}",
                        e
                    ))]),
                };
            }
            let (image_data, mime_type) =
                match crate::tools::screenshot::png_to_jpeg(&shot.png_data) {
                    Ok(jpeg_data) => (jpeg_data, "image/jpeg"),
                    Err(e) => {
                        tracing::warn!("JPEG conversion failed, using PNG: {}", e);
                        (shot.png_data, "image/png")
                    }
                };
            let base64_data = base64::engine::general_purpose::STANDARD.encode(&image_data);
            let mut contents = vec![Content::image(base64_data, mime_type)];
            contents.push(Content::text(to_json_pretty(&serde_json::json!({
                "width": shot.width,
                "height": shot.height,
                "scale": 1.0,
                "device": device.serial,
            }))));
            CallToolResult::success(contents)
        })
        .await)
    }
}

/// `android_click` — visible only while connected.
pub struct AndroidClick;

#[async_trait::async_trait]
impl ToolHandler for AndroidClick {
    fn name(&self) -> &'static str {
        "android_click"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAndroidConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "android_click",
            "Tap at screen coordinates on the Android device.",
            Arc::new(crate::tools::registry::json_to_object(serde_json::json!({
                "type": "object",
                "required": ["x", "y"],
                "properties": {
                    "x": {
                        "type": "number",
                        "description": "Screen X coordinate"
                    },
                    "y": {
                        "type": "number",
                        "description": "Screen Y coordinate"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let (x, y) = parse_xy(&args)?;
        Ok(with_android_device(ctx.android_device.clone(), |device| {
            match crate::android::input::click(device, x, y) {
                Ok(()) => CallToolResult::success(vec![Content::text(format!(
                    "Tapped at ({:.0}, {:.0})",
                    x, y
                ))]),
                Err(e) => CallToolResult::error(vec![Content::text(e)]),
            }
        })
        .await)
    }
}

/// `android_type_text` — visible only while connected.
pub struct AndroidTypeText;

#[async_trait::async_trait]
impl ToolHandler for AndroidTypeText {
    fn name(&self) -> &'static str {
        "android_type_text"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAndroidConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "android_type_text",
            "Type text on the Android device. Special characters are automatically escaped.",
            Arc::new(crate::tools::registry::json_to_object(serde_json::json!({
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Text to type"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let text = parse_string_field(&args, "text")?;
        let len = text.len();
        Ok(with_android_device(ctx.android_device.clone(), |device| {
            match crate::android::input::type_text(device, &text) {
                Ok(()) => CallToolResult::success(vec![Content::text(format!(
                    "Typed {} characters",
                    len
                ))]),
                Err(e) => CallToolResult::error(vec![Content::text(e)]),
            }
        })
        .await)
    }
}

/// `android_press_key` — visible only while connected.
pub struct AndroidPressKey;

#[async_trait::async_trait]
impl ToolHandler for AndroidPressKey {
    fn name(&self) -> &'static str {
        "android_press_key"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAndroidConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "android_press_key",
            "Press a key on the Android device by keycode name (e.g., 'KEYCODE_HOME') or numeric code.",
            Arc::new(crate::tools::registry::json_to_object(serde_json::json!({
                "type": "object",
                "required": ["key"],
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Key to press (e.g., 'KEYCODE_HOME', 'KEYCODE_BACK', 'KEYCODE_ENTER', or a numeric keycode)"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let key = parse_string_field(&args, "key")?;
        Ok(with_android_device(ctx.android_device.clone(), |device| {
            match crate::android::input::press_key(device, &key) {
                Ok(()) => {
                    CallToolResult::success(vec![Content::text(format!("Pressed key: {}", key))])
                }
                Err(e) => CallToolResult::error(vec![Content::text(e)]),
            }
        })
        .await)
    }
}

/// `android_swipe` — visible only while connected.
pub struct AndroidSwipe;

#[async_trait::async_trait]
impl ToolHandler for AndroidSwipe {
    fn name(&self) -> &'static str {
        "android_swipe"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAndroidConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "android_swipe",
            "Swipe from one point to another on the Android device.",
            Arc::new(crate::tools::registry::json_to_object(serde_json::json!({
                "type": "object",
                "required": ["start_x", "start_y", "end_x", "end_y"],
                "properties": {
                    "start_x": {
                        "type": "number",
                        "description": "Start X coordinate"
                    },
                    "start_y": {
                        "type": "number",
                        "description": "Start Y coordinate"
                    },
                    "end_x": {
                        "type": "number",
                        "description": "End X coordinate"
                    },
                    "end_y": {
                        "type": "number",
                        "description": "End Y coordinate"
                    },
                    "duration_ms": {
                        "type": "integer",
                        "description": "Duration of the swipe in milliseconds (optional, default is instant)"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        #[derive(serde::Deserialize)]
        struct Params {
            start_x: f64,
            start_y: f64,
            end_x: f64,
            end_y: f64,
            duration_ms: Option<u32>,
        }
        let p: Params = serde_json::from_value(args)
            .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
        Ok(with_android_device(ctx.android_device.clone(), |device| {
            match crate::android::input::swipe(
                device,
                p.start_x,
                p.start_y,
                p.end_x,
                p.end_y,
                p.duration_ms,
            ) {
                Ok(()) => CallToolResult::success(vec![Content::text(format!(
                    "Swiped from ({:.0}, {:.0}) to ({:.0}, {:.0})",
                    p.start_x, p.start_y, p.end_x, p.end_y
                ))]),
                Err(e) => CallToolResult::error(vec![Content::text(e)]),
            }
        })
        .await)
    }
}
