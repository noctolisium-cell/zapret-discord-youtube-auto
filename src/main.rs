#![windows_subsystem = "windows"]

use eframe::egui;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, File};
use std::io::{self, Cursor};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// Вшитый архив со сборкой Flowseal 1.10.4
const EMBEDDED_ZIP: &[u8] = include_bytes!("../payload.zip");
const INITIAL_VERSION: &str = "1.10.4";

const GITHUB_REPO: &str = "Flowseal/zapret-discord-youtube";
const APP_FOLDER_NAME: &str = "ZapretFlowseal";
const EXE_NAME: &str = "zapret-gui.exe";
const TASK_NAME: &str = "ZapretGuiAutoStart";
const CONFIG_FILE: &str = "config.json";
const VERSION_FILE: &str = "version.txt";
const CREATE_NO_WINDOW: u32 = 0x08000000;

// Настройки пользователя, которые сохраняются на диск
#[derive(Serialize, Deserialize, Clone)]
struct AppConfig {
    pinned_profile: String,
    auto_connect: bool,
    auto_start_windows: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            pinned_profile: "general.bat".to_string(),
            auto_connect: true,
            auto_start_windows: false,
        }
    }
}

#[derive(Deserialize, Debug)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize, Debug)]
struct Asset {
    name: String,
    browser_download_url: String,
}

// Состояние, доступное и окну интерфейса, и фоновому потоку обновлений
struct SharedState {
    is_running: bool,
    current_version: String,
    status_text: String,
    available_profiles: Vec<String>,
    config: AppConfig,
}

struct ZapretApp {
    app_dir: PathBuf,
    installed_exe: PathBuf,
    state: Arc<Mutex<SharedState>>,
}

fn main() -> eframe::Result<()> {
    let app_dir = get_appdata_dir();
    let _ = fs::create_dir_all(&app_dir);

    let installed_exe = install_self_to_appdata(&app_dir);
    let _ = extract_initial_payload(&app_dir);

    let config = load_config(&app_dir);
    let profiles = scan_bat_files(&app_dir);
    let current_version = fs::read_to_string(app_dir.join(VERSION_FILE))
        .unwrap_or_else(|_| INITIAL_VERSION.to_string());

    let is_running = check_winws_running();

    let state = Arc::new(Mutex::new(SharedState {
        is_running,
        current_version: current_version.trim().to_string(),
        status_text: if is_running {
            "Запрет активен".to_string()
        } else {
            "Остановлен".to_string()
        },
        available_profiles: profiles,
        config: config.clone(),
    }));

    // Если включено автоподключение — сразу запускаем закрепленный профиль
    if config.auto_connect && !is_running {
        start_zapret_process(&app_dir, &config.pinned_profile);
        if let Ok(mut st) = state.lock() {
            st.is_running = check_winws_running();
            st.status_text = format!("Активен профиль: {}", config.pinned_profile);
        }
    }

    // Запускаем постоянный фоновый поток автообновления (проверка при старте и каждые 2 часа)
    let bg_state = Arc::clone(&state);
    let bg_dir = app_dir.clone();
    thread::spawn(move || {
        loop {
            if let Ok(mut st) = bg_state.lock() {
                st.status_text = "Проверка обновлений на GitHub...".to_string();
            }

            match check_and_update(&bg_dir) {
                Ok(Some(new_ver)) => {
                    let profiles = scan_bat_files(&bg_dir);
                    if let Ok(mut st) = bg_state.lock() {
                        st.current_version = new_ver.clone();
                        st.available_profiles = profiles;
                        st.status_text = format!("Обновлено до {}", new_ver);
                        // Перезапускаем запрет после обновления, если он работал или включен автоконнект
                        if st.is_running || st.config.auto_connect {
                            start_zapret_process(&bg_dir, &st.config.pinned_profile);
                            st.is_running = true;
                        }
                    }
                }
                Ok(None) => {
                    if let Ok(mut st) = bg_state.lock() {
                        st.is_running = check_winws_running();
                        st.status_text = if st.is_running {
                            format!("Работает ({})", st.config.pinned_profile)
                        } else {
                            "Остановлен (обновлений нет)".to_string()
                        };
                    }
                }
                Err(_) => {
                    if let Ok(mut st) = bg_state.lock() {
                        st.is_running = check_winws_running();
                        st.status_text = "Работает (GitHub недоступен)".to_string();
                    }
                }
            }

            // Спим 2 часа до следующей проверки
            thread::sleep(Duration::from_secs(7200));
        }
    });

    // Настройки окна программы
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 320.0])
            .with_resizable(false)
            .with_maximize_button(false),
        ..Default::default()
    };

    eframe::run_native(
        "Zapret Control Center",
        options,
        Box::new(|_cc| {
            Ok(Box::new(ZapretApp {
                app_dir,
                installed_exe,
                state,
            }))
        }),
    )
}

impl eframe::App for ZapretApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut st = self.state.lock().unwrap();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Управление Zapret (Flowseal)");
            ui.add_space(5.0);

            // Блок статуса и версии
            ui.horizontal(|ui| {
                ui.label("Версия сборки:");
                ui.strong(&st.current_version);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let color = if st.is_running {
                        egui::Color32::from_rgb(50, 205, 50)
                    } else {
                        egui::Color32::from_rgb(220, 50, 50)
                    };
                    ui.colored_label(color, if st.is_running { "● РАБОТАЕТ" } else { "● ОСТАНОВЛЕН" });
                });
            });

            ui.label(egui::RichText::new(&st.status_text).small().weak());
            ui.separator();
            ui.add_space(8.0);

            // Выбор и закрепление ситуации (батника)
            ui.label("Закрепленная ситуация (профиль):");
            let prev_profile = st.config.pinned_profile.clone();
            egui::ComboBox::from_id_salt("profile_selector")
                .width(ui.available_width())
                .selected_text(&st.config.pinned_profile)
                .show_ui(ui, |ui| {
                    for profile in &st.available_profiles.clone() {
                        ui.selectable_value(&mut st.config.pinned_profile, profile.clone(), profile);
                    }
                });

            // Если пользователь сменил профиль в списке — сохраняем и сразу перезапускаем
            if prev_profile != st.config.pinned_profile {
                save_config(&self.app_dir, &st.config);
                if st.is_running {
                    start_zapret_process(&self.app_dir, &st.config.pinned_profile);
                    st.status_text = format!("Переключено на {}", st.config.pinned_profile);
                }
            }

            ui.add_space(12.0);

            // Кнопки Запуска / Остановки
            ui.horizontal(|ui| {
                let btn_text = if st.is_running {
                    "🔄 Перезапустить профиль"
                } else {
                    "▶ Запустить обход"
                };

                if ui.button(btn_text).clicked() {
                    start_zapret_process(&self.app_dir, &st.config.pinned_profile);
                    st.is_running = true;
                    st.status_text = format!("Запущен {}", st.config.pinned_profile);
                }

                if st.is_running {
                    if ui.button("⏹ Остановить").clicked() {
                        stop_winws();
                        st.is_running = false;
                        st.status_text = "Остановлен пользователем".to_string();
                    }
                }
            });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            // Переключатели Автоподключения и Автозапуска
            if ui
                .checkbox(&mut st.config.auto_connect, "Автоподключение при открытии программы")
                .changed()
            {
                save_config(&self.app_dir, &st.config);
            }

            if ui
                .checkbox(&mut st.config.auto_start_windows, "Автозапуск вместе с Windows (без UAC)")
                .changed()
            {
                set_autostart(&self.installed_exe, st.config.auto_start_windows);
                save_config(&self.app_dir, &st.config);
            }

            ui.add_space(15.0);

            // Кнопки открытия папок
            ui.horizontal(|ui| {
                if ui.button("📂 Открыть папку со списками (lists)").clicked() {
                    let lists_dir = self.app_dir.join("lists");
                    let _ = fs::create_dir_all(&lists_dir);
                    let _ = Command::new("explorer").arg(lists_dir).spawn();
                }

                if ui.button("📁 Вся папка").clicked() {
                    let _ = Command::new("explorer").arg(&self.app_dir).spawn();
                }
            });
        });

        // Обновляем интерфейс раз в секунду для отображения фоновых статусов
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

// ================= ВСПОМОГАТЕЛЬНЫЕ ФУНКЦИИ =================

fn get_appdata_dir() -> PathBuf {
    let local = env::var("LOCALAPPDATA").unwrap_or_else(|_| "C:\\ProgramData".to_string());
    Path::new(&local).join(APP_FOLDER_NAME)
}

fn install_self_to_appdata(app_dir: &Path) -> PathBuf {
    let target_exe = app_dir.join(EXE_NAME);
    if let Ok(current_exe) = env::current_exe() {
        if current_exe != target_exe {
            let _ = fs::copy(&current_exe, &target_exe);
        }
    }
    target_exe
}

fn load_config(app_dir: &Path) -> AppConfig {
    let path = app_dir.join(CONFIG_FILE);
    if let Ok(data) = fs::read_to_string(path) {
        if let Ok(cfg) = serde_json::from_str(&data) {
            return cfg;
        }
    }
    AppConfig::default()
}

fn save_config(app_dir: &Path, config: &AppConfig) {
    let path = app_dir.join(CONFIG_FILE);
    if let Ok(data) = serde_json::to_string_pretty(config) {
        let _ = fs::write(path, data);
    }
}

/// Ищет все .bat файлы в папке (кроме сервисных), чтобы показать их в меню выбора
fn scan_bat_files(app_dir: &Path) -> Vec<String> {
    let mut bats = Vec::new();
    if let Ok(entries) = fs::read_dir(app_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("bat") {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    // Исключаем служебные батники установки служб, оставляем только рабочие профили
                    if !name.starts_with("service_") {
                        bats.push(name.to_string());
                    }
                }
            }
        }
    }
    bats.sort();
    if bats.is_empty() {
        bats.push("general.bat".to_string());
    }
    bats
}

fn extract_initial_payload(app_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let version_path = app_dir.join(VERSION_FILE);
    if !version_path.exists() {
        stop_winws();
        extract_zip_bytes(EMBEDDED_ZIP, app_dir, false)?;
        fs::write(version_path, INITIAL_VERSION)?;
    }
    Ok(())
}

/// Распаковывает архив. Если preserve_user_lists = true, не затирает существующие списки при автообновлении
fn extract_zip_bytes(
    bytes: &[u8],
    target_dir: &Path,
    preserve_user_lists: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let reader = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let enclosed = match file.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => continue,
        };

        // Если в архиве с GitHub все лежит внутри одной общей папки — срезаем её
        let relative_path = if enclosed.components().count() > 1
            && !enclosed.starts_with("bin")
            && !enclosed.starts_with("lists")
        {
            enclosed.iter().skip(1).collect::<PathBuf>()
        } else {
            enclosed
        };

        if relative_path.as_os_str().is_empty() {
            continue;
        }

        let outpath = target_dir.join(&relative_path);

        if file.name().ends_with('/') {
            fs::create_dir_all(&outpath)?;
        } else {
            // Защита пользовательских файлов при автообновлении
            if preserve_user_lists && relative_path.starts_with("lists") && outpath.exists() {
                continue;
            }
            if outpath.file_name().and_then(|n| n.to_str()) == Some(EXE_NAME) {
                continue;
            }

            if let Some(p) = outpath.parent() {
                fs::create_dir_all(p)?;
            }
            let mut outfile = File::create(&outpath)?;
            io::copy(&mut file, &mut outfile)?;
        }
    }
    Ok(())
}

fn check_and_update(app_dir: &Path) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let client = Client::builder()
        .user_agent("Zapret-GUI-Updater/1.0")
        .timeout(Duration::from_secs(15))
        .build()?;

    let url = format!("https://api.github.com/repos/{}/releases/latest", GITHUB_REPO);
    let release: Release = client.get(&url).send()?.json()?;

    let version_path = app_dir.join(VERSION_FILE);
    let current_version = fs::read_to_string(&version_path).unwrap_or_default();

    if current_version.trim() != release.tag_name {
        if let Some(asset) = release.assets.iter().find(|a| a.name.ends_with(".zip")) {
            stop_winws();
            let response = client.get(&asset.browser_download_url).send()?;
            let bytes = response.bytes()?;

            // При обновлении сохраняем пользовательские файлы в папке lists (preserve_user_lists = true)
            extract_zip_bytes(&bytes, app_dir, true)?;
            fs::write(version_path, &release.tag_name)?;
            return Ok(Some(release.tag_name));
        }
    }
    Ok(None)
}

fn start_zapret_process(app_dir: &Path, bat_name: &str) {
    stop_winws();
    let bat_path = app_dir.join(bat_name);
    if bat_path.exists() {
        let _ = Command::new("cmd")
            .args(["/C", bat_name])
            .current_dir(app_dir)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
}

fn stop_winws() {
    let _ = Command::new("taskkill")
        .args(["/F", "/IM", "winws.exe"])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

fn check_winws_running() -> bool {
    if let Ok(output) = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq winws.exe"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        return stdout.contains("winws.exe");
    }
    false
}

/// Включает или выключает задачу в Планировщике Windows по нажатию галочки в меню
fn set_autostart(exe_path: &Path, enable: bool) {
    if enable {
        if let Some(path_str) = exe_path.to_str() {
            let _ = Command::new("schtasks")
                .args([
                    "/Create",
                    "/TN",
                    TASK_NAME,
                    "/TR",
                    &format!("\"{}\"", path_str),
                    "/SC",
                    "ONLOGON",
                    "/RL",
                    "HIGHEST",
                    "/F",
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .status();
        }
    } else {
        let _ = Command::new("schtasks")
            .args(["/Delete", "/TN", TASK_NAME, "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
}
