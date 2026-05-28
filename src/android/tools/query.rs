//! UI query, app listing, and activity inspection tool handlers.

use super::with_android_device;
use crate::tools::registry::{
    parse_string_field, to_json_pretty, Availability, ToolContext, ToolHandler,
};
use rmcp::{
    model::{CallToolResult, Content, Tool},
    Error as McpError,
};
use serde_json::Value;
use std::sync::Arc;

/// `android_find_text` — visible only while connected.
pub struct AndroidFindText;

#[async_trait::async_trait]
impl ToolHandler for AndroidFindText {
    fn name(&self) -> &'static str {
        "android_find_text"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAndroidConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "android_find_text",
            "Find UI elements on the Android device screen that match the given text. Uses uiautomator to dump the view hierarchy and search for matching elements. Returns coordinates for clicking. When no matches are found, the response includes an available_elements array listing all UI element names on screen — use this to find the correct name and retry.",
            Arc::new(crate::tools::registry::json_to_object(serde_json::json!({
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Text to search for (case-insensitive substring match against text and content-desc attributes)"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let text = parse_string_field(&args, "text")?;
        Ok(with_android_device(ctx.android_device.clone(), |device| {
            match crate::android::ui_automator::find_text(device, &text) {
                Ok(result) => {
                    let mut content = vec![Content::text(to_json_pretty(&result.matches))];
                    if result.matches.is_empty() {
                        content.push(Content::text(crate::tools::input::build_no_matches_hint(
                            &text,
                            &result.available_elements,
                        )));
                    }
                    CallToolResult::success(content)
                }
                Err(e) => CallToolResult::error(vec![Content::text(e)]),
            }
        })
        .await)
    }
}

/// `android_list_apps` — visible only while connected.
pub struct AndroidListApps;

#[async_trait::async_trait]
impl ToolHandler for AndroidListApps {
    fn name(&self) -> &'static str {
        "android_list_apps"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAndroidConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "android_list_apps",
            "List installed apps on the Android device.",
            Arc::new(crate::tools::registry::json_to_object(serde_json::json!({
                "type": "object",
                "properties": {
                    "user_apps_only": {
                        "type": "boolean",
                        "description": "Only return user-installed (third-party) apps. Default is false (all packages)."
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let user_apps_only = args
            .get("user_apps_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(with_android_device(ctx.android_device.clone(), |device| {
            match crate::android::navigation::list_apps(device, user_apps_only) {
                Ok(apps) => CallToolResult::success(vec![Content::text(to_json_pretty(&apps))]),
                Err(e) => CallToolResult::error(vec![Content::text(e)]),
            }
        })
        .await)
    }
}

/// `android_launch_app` — visible only while connected.
pub struct AndroidLaunchApp;

#[async_trait::async_trait]
impl ToolHandler for AndroidLaunchApp {
    fn name(&self) -> &'static str {
        "android_launch_app"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAndroidConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "android_launch_app",
            "Launch an app on the Android device by its package name.",
            Arc::new(crate::tools::registry::json_to_object(serde_json::json!({
                "type": "object",
                "required": ["package_name"],
                "properties": {
                    "package_name": {
                        "type": "string",
                        "description": "Package name to launch (e.g., 'com.android.settings')"
                    }
                }
            }))),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        let package_name = parse_string_field(&args, "package_name")?;
        Ok(with_android_device(ctx.android_device.clone(), |device| {
            match crate::android::navigation::launch_app(device, &package_name) {
                Ok(()) => CallToolResult::success(vec![Content::text(format!(
                    "Launched {}",
                    package_name
                ))]),
                Err(e) => CallToolResult::error(vec![Content::text(e)]),
            }
        })
        .await)
    }
}

/// `android_get_display_info` — visible only while connected.
pub struct AndroidGetDisplayInfo;

#[async_trait::async_trait]
impl ToolHandler for AndroidGetDisplayInfo {
    fn name(&self) -> &'static str {
        "android_get_display_info"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAndroidConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "android_get_display_info",
            "Get display information (size and density) from the Android device.",
            Arc::new(crate::tools::registry::json_to_object(serde_json::json!({
                "type": "object",
                "properties": {}
            }))),
        )
    }

    async fn call(&self, _args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        Ok(with_android_device(ctx.android_device.clone(), |device| {
            match crate::android::navigation::get_display_info(device) {
                Ok(info) => CallToolResult::success(vec![Content::text(to_json_pretty(&info))]),
                Err(e) => CallToolResult::error(vec![Content::text(e)]),
            }
        })
        .await)
    }
}

/// `android_get_current_activity` — visible only while connected.
pub struct AndroidGetCurrentActivity;

#[async_trait::async_trait]
impl ToolHandler for AndroidGetCurrentActivity {
    fn name(&self) -> &'static str {
        "android_get_current_activity"
    }

    fn availability(&self) -> Availability {
        Availability::WhenAndroidConnected
    }

    fn schema(&self) -> Tool {
        Tool::new(
            "android_get_current_activity",
            "Get the currently resumed activity on the Android device.",
            Arc::new(crate::tools::registry::json_to_object(serde_json::json!({
                "type": "object",
                "properties": {}
            }))),
        )
    }

    async fn call(&self, _args: Value, ctx: &ToolContext) -> Result<CallToolResult, McpError> {
        Ok(with_android_device(ctx.android_device.clone(), |device| {
            match crate::android::navigation::get_current_activity(device) {
                Ok(activity) => {
                    CallToolResult::success(vec![Content::text(to_json_pretty(&activity))])
                }
                Err(e) => CallToolResult::error(vec![Content::text(e)]),
            }
        })
        .await)
    }
}
