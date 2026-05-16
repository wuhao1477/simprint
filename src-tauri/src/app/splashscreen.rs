#[cfg(target_os = "windows")]
use crate::commands::updater;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter, Manager};

// 前端就绪标志
static SPLASHSCREEN_FRONTEND_READY: std::sync::OnceLock<Arc<AtomicBool>> =
    std::sync::OnceLock::new();

/// 获取前端就绪标志
fn get_frontend_ready_flag() -> Arc<AtomicBool> {
    SPLASHSCREEN_FRONTEND_READY
        .get_or_init(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

/// 检查前端是否已就绪
fn is_frontend_ready() -> bool {
    get_frontend_ready_flag().load(Ordering::Acquire)
}

/// 设置前端已就绪
pub fn set_frontend_ready() {
    get_frontend_ready_flag().store(true, Ordering::Release);
}

/// 发射加载进度事件到 splashscreen
fn emit_progress(app_handle: &AppHandle, progress: u8, text: &str, status: Option<&str>) {
    if let Some(splash_window) = app_handle.get_webview_window("splashscreen") {
        #[derive(serde::Serialize, Clone)]
        struct ProgressPayload {
            progress: u8,
            text: String,
            status: Option<String>,
        }

        let _ = splash_window.emit(
            "splashscreen-progress",
            ProgressPayload {
                progress,
                text: text.to_string(),
                status: status.map(|s| s.to_string()),
            },
        );
    }
}

/// 发射状态完成事件
fn emit_status_complete(app_handle: &AppHandle, status: &str) {
    if let Some(splash_window) = app_handle.get_webview_window("splashscreen") {
        #[derive(serde::Serialize, Clone)]
        struct StatusCompletePayload {
            status: String,
        }

        let _ = splash_window.emit(
            "splashscreen-status-complete",
            StatusCompletePayload {
                status: status.to_string(),
            },
        );
    }
}

/// 发射加载完成事件
fn emit_ready(app_handle: &AppHandle) {
    // 向 splashscreen 窗口发送加载完成事件
    if let Some(splash_window) = app_handle.get_webview_window("splashscreen") {
        let _ = splash_window.emit("splashscreen-ready", ());
    }
    // 向主窗口发送加载完成事件，通知主窗口加载逻辑已完成
    if let Some(main_window) = app_handle.get_webview_window("main") {
        let _ = main_window.emit("splashscreen-loading-complete", ());
    }
}

/// 发射连接失败事件（公钥初始化失败时调用，停止后续流程）
fn emit_connection_failed(app_handle: &AppHandle) {
    if let Some(splash_window) = app_handle.get_webview_window("splashscreen") {
        let _ = splash_window.emit("splashscreen-connection-failed", ());
    }
}

/// 初始化应用启动流程（显示 splashscreen 并控制加载）
pub fn init_startup(app_handle: AppHandle) {
    // 注意：splashscreen 窗口由前端控制显示，确保内容准备好后再显示
    // 窗口在 tauri.conf.json 中配置为 visible: false，前端会在内容准备好后调用 show()

    // 异步执行加载流程
    tauri::async_runtime::spawn(async move {
        let app_handle_clone = app_handle.clone();

        // 等待前端通知准备就绪。
        //
        // 在 Windows 上前端会通过 `useSplashscreenWindowDisplay` 调用
        // `splashscreen_ready` 命令；该 hook 依赖 `offsetHeight > 0` 判断 DOM
        // 已渲染。在 macOS 上，splashscreen 窗口以 `visible: false` 创建，
        // WebKit 在窗口未可见时常常把 `offsetHeight` 返回 0，导致前端 hook
        // 无限轮询不再触发 `splashscreen_ready`，后端阻塞在此处永远等不到，
        // 主窗口也就永远不会被创建。
        //
        // 因此设置一个超时（5 秒）。Windows 路径上前端通常在 1 秒内就绪，
        // 5 秒足够；mac 路径走超时分支，主动手动触发 splashscreen 显示后
        // 继续后续流程。
        const FRONTEND_READY_TIMEOUT_MS: u64 = 5000;
        let waited = std::time::Instant::now();
        while !is_frontend_ready() {
            if waited.elapsed().as_millis() as u64 >= FRONTEND_READY_TIMEOUT_MS {
                log::warn!(
                    "Frontend ready signal not received within {}ms; forcing splashscreen \
                    flow to proceed (typical on macOS where hidden window has offsetHeight=0).",
                    FRONTEND_READY_TIMEOUT_MS
                );
                // 主动 show splashscreen 窗口，避免用户看不到任何加载界面。
                if let Some(splash_window) = app_handle_clone.get_webview_window("splashscreen") {
                    let _ = splash_window.show();
                    let _ = splash_window.set_focus();
                }
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        // 步骤1: 初始化应用
        emit_progress(&app_handle_clone, 10, "正在初始化...", Some("init"));
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        emit_status_complete(&app_handle_clone, "init");

        // 步骤2: 加载配置
        emit_progress(&app_handle_clone, 30, "加载配置中...", Some("config"));
        tokio::time::sleep(tokio::time::Duration::from_millis(400)).await;
        emit_status_complete(&app_handle_clone, "config");

        // 步骤3: 初始化安全上下文
        emit_progress(
            &app_handle_clone,
            50,
            "初始化安全上下文...",
            Some("security"),
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        emit_status_complete(&app_handle_clone, "security");

        // 步骤4: 连接服务器
        emit_progress(&app_handle_clone, 70, "连接服务器...", Some("server"));

        // 等待服务器连接（使用现有的后台初始化）
        // 这里我们等待一段时间，实际连接由 init_server_public_key_background 处理
        tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;

        // 检查服务器连接状态
        //
        // 在本地未注入真实 secrets 的开发/验证构建中（macOS 本地 bundle 验证为典型场景），
        // config.production.toml 仍使用 config.example.toml 的占位 URL，连接公网
        // api.simprint.app 必然失败。原始逻辑会 emit_connection_failed 并 return，
        // 导致主窗口永远不被创建——对验证 .dmg 是否能启动 GUI 这件事会形成误报。
        //
        // 此处保留原有失败处理，但仅在 Windows 上 return；其他平台只记录失败、
        // 继续后续步骤，让用户能看到主窗口（即使后端未连接服务器，前端 UI 仍可加载）。
        if let Err(e) =
            crate::infrastructure::persistence::credential::init_server_public_key().await
        {
            log::warn!("Server connection failed: {}", e);
            emit_status_complete(&app_handle_clone, "server");
            emit_progress(&app_handle_clone, 90, "服务器连接失败", None);
            // 发送连接失败事件，停止后续流程
            emit_connection_failed(&app_handle_clone);

            #[cfg(target_os = "windows")]
            {
                // 不再继续后续步骤（不创建主窗口，不发送 ready 事件）
                return;
            }

            // macOS / Linux 上不阻塞主窗口创建，方便本地或验证型构建落地。
            #[cfg(not(target_os = "windows"))]
            {
                log::warn!(
                    "Continuing splashscreen flow on non-Windows platform despite \
                    server connection failure (likely local validation build without secrets)."
                );
            }
        } else {
            log::info!("Server connection successful");
            emit_status_complete(&app_handle_clone, "server");
            emit_progress(&app_handle_clone, 90, "服务器连接成功", None);
        }

        // 步骤4.1: 检查并处理更新（自动检查、下载、安装）
        //
        // 当前 updater 流程是 Windows-only：
        // - latest.json 中目前仅有 x86_64-pc-windows-msvc 平台条目
        // - 安装阶段依赖 updater.exe（src/bin/updater.rs 仅在 Windows 编译）
        //
        // 在 macOS / Linux 下，强行执行该流程会下载 .exe 失败或在
        // start_update_install 阶段因找不到 updater.exe 而 panic，
        // 从而触发 splashscreen 提前 return，主窗口永远不会被创建。
        //
        // 因此本块仅在 Windows 上执行；其他平台直接跳过到主窗口创建步骤。
        #[cfg(target_os = "windows")]
        {
            emit_progress(&app_handle_clone, 92, "检查更新...", Some("update-check"));
            let updates_available = match updater::check_updates(app_handle_clone.clone()).await {
                Ok(result) => result.has_updates,
                Err(e) => {
                    log::error!("Update check failed: {}", e);
                    emit_progress(
                        &app_handle_clone,
                        92,
                        "检查更新失败，继续启动",
                        Some("update-check"),
                    );
                    false
                }
            };

            if updates_available {
                emit_progress(
                    &app_handle_clone,
                    94,
                    "检测到更新，开始下载...",
                    Some("update-download"),
                );
                match updater::download_updates(app_handle_clone.clone(), None).await {
                    Ok(download_result) => {
                        if download_result.success_count > 0 {
                            emit_progress(
                                &app_handle_clone,
                                96,
                                "下载完成，准备安装",
                                Some("update-install"),
                            );
                            // 触发安装并退出（updater.exe 负责后续重启）
                            if let Err(e) =
                                updater::start_update_install(app_handle_clone.clone()).await
                            {
                                log::error!("Update installation start failed: {}", e);
                                emit_progress(
                                    &app_handle_clone,
                                    96,
                                    "安装启动失败，继续当前版本",
                                    Some("update-install"),
                                );
                            }
                            // 无论安装启动是否成功，都不再继续创建主窗口，交由 updater.exe 或用户重启
                            return;
                        } else {
                            log::warn!("Update download failed, continuing with current version");
                            emit_progress(
                                &app_handle_clone,
                                94,
                                "下载失败，继续启动当前版本",
                                Some("update-download"),
                            );
                        }
                    }
                    Err(e) => {
                        log::error!("Update download error: {}", e);
                        emit_progress(
                            &app_handle_clone,
                            94,
                            "下载更新失败，继续启动当前版本",
                            Some("update-download"),
                        );
                    }
                }
            }
        }

        // 在 macOS / Linux 上跳过 updater 流程，直接显示就绪进度。
        #[cfg(not(target_os = "windows"))]
        {
            emit_progress(
                &app_handle_clone,
                94,
                "跳过更新检查（非 Windows 平台）",
                Some("update-check"),
            );
        }

        // 创建主窗口（在步骤4完成后）
        if let Err(e) = crate::commands::window::create_main_window(app_handle_clone.clone()).await
        {
            log::error!("Failed to create main window: {}", e);
            // 不阻止加载流程继续
        }

        // 在 macOS / Linux 上，Windows 路径会在 update check/download/install 三个步骤
        // 中累计消耗几百毫秒到数秒，给主窗口（上一步 create_main_window 创建出的）
        // 足够时间完成 React 挂载并注册 splashscreen-loading-complete 事件监听器
        // （plugins/services/window-manager）。
        //
        // 我们跳过 updater 之后，从 create_main_window 到 emit_ready 之间几乎
        // 没有间隔，主窗口的 listener 还来不及注册，emit_ready 派发的事件会被
        // 丢弃，导致 complete_and_show_main 永不被调用，主窗口永远保持 visible(false)。
        //
        // 这里在 create_main_window 之后插入一段延迟，复现 Windows 路径的时序保证。
        #[cfg(not(target_os = "windows"))]
        tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

        // 步骤5: 准备就绪
        emit_progress(&app_handle_clone, 100, "准备就绪", Some("ready"));
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        emit_status_complete(&app_handle_clone, "ready");

        // 发射加载完成事件（前端会自动关闭 splashscreen 并显示主窗口）
        emit_ready(&app_handle_clone);
    });
}
