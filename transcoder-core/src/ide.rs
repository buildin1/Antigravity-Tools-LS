use std::path::{Path, PathBuf};
use std::fs;
use std::process::Command;
use std::thread;
use std::time::Duration;
use rusqlite::Connection;
use sysinfo::System;
use base64::{engine::general_purpose, Engine as _};
use ls_accounts::model::{Account, DeviceProfile};
use tracing::{info, warn, error, debug};
use chrono::Utc;
use rand::{distributions::Alphanumeric, Rng};
use serde::{Serialize, Deserialize};

/// Protobuf 编解码工具 (从 Manager 移植并补全)
mod protobuf {
    pub fn encode_varint(mut value: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        while value >= 0x80 {
            buf.push((value & 0x7F | 0x80) as u8);
            value >>= 7;
        }
        buf.push(value as u8);
        buf
    }

    pub fn read_varint(data: &[u8], offset: usize) -> Result<(u64, usize), String> {
        let mut result = 0u64;
        let mut shift = 0;
        let mut pos = offset;
        loop {
            if pos >= data.len() { return Err("Incomplete varint".into()); }
            let byte = data[pos];
            result |= ((byte & 0x7F) as u64) << shift;
            pos += 1;
            if byte & 0x80 == 0 { break; }
            shift += 7;
        }
        Ok((result, pos))
    }

    pub fn skip_field(data: &[u8], offset: usize, wire_type: u8) -> Result<usize, String> {
        match wire_type {
            0 => Ok(read_varint(data, offset)?.1),
            1 => Ok(offset + 8),
            2 => {
                let (len, content_offset) = read_varint(data, offset)?;
                Ok(content_offset + len as usize)
            }
            5 => Ok(offset + 4),
            _ => Err(format!("Unknown wire type: {}", wire_type)),
        }
    }

    pub fn remove_field(data: &[u8], field_num: u32) -> Result<Vec<u8>, String> {
        let mut result = Vec::new();
        let mut offset = 0;
        while offset < data.len() {
            let start = offset;
            let (tag, new_offset) = read_varint(data, offset)?;
            let wire_type = (tag & 7) as u8;
            let current_field = (tag >> 3) as u32;
            let next_offset = skip_field(data, new_offset, wire_type)?;
            if current_field != field_num {
                result.extend_from_slice(&data[start..next_offset]);
            }
            offset = next_offset;
        }
        Ok(result)
    }

    pub fn encode_len_delim_field(field_num: u32, data: &[u8]) -> Vec<u8> {
        let tag = (field_num << 3) | 2;
        let mut f = encode_varint(tag as u64);
        f.extend(encode_varint(data.len() as u64));
        f.extend_from_slice(data);
        f
    }

    pub fn encode_string_field(field_num: u32, value: &str) -> Vec<u8> {
        encode_len_delim_field(field_num, value.as_bytes())
    }

    pub fn create_oauth_info(access_token: &str, refresh_token: &str, expiry: i64) -> Vec<u8> {
        let field1 = encode_string_field(1, access_token);
        let field2 = encode_string_field(2, "Bearer");
        let field3 = encode_string_field(3, refresh_token);
        let timestamp_tag = (1 << 3) | 0;
        let mut timestamp_msg = encode_varint(timestamp_tag);
        timestamp_msg.extend(encode_varint(expiry as u64));
        let field4 = encode_len_delim_field(4, &timestamp_msg);
        [field1, field2, field3, field4].concat()
    }

    pub fn create_oauth_field(access_token: &str, refresh_token: &str, expiry: i64) -> Vec<u8> {
        let info = create_oauth_info(access_token, refresh_token, expiry);
        encode_len_delim_field(6, &info)
    }

    pub fn create_email_field(email: &str) -> Vec<u8> {
        encode_string_field(2, email)
    }
}

/// 获取 Antigravity 进程信息（路径及参数）
/// 返回 (可执行文件路径, 用户数据目录)
pub fn get_process_info_for_api() -> (Option<PathBuf>, Option<Vec<String>>) {
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, false);

    for (_pid, process) in system.processes() {
        let name = process.name().to_string_lossy().to_lowercase();
        let exe_path_buf = process.exe().map(|p| p.to_path_buf());
        let exe_path = exe_path_buf.as_ref().and_then(|p| p.to_str()).unwrap_or("").to_lowercase();

        // 识别特征：必须包含 antigravity 且不是 helper/tools/manager
        let is_antigravity = (name == "antigravity" || name == "antigravity.exe" || exe_path.contains("antigravity.app"))
            && !name.contains("helper") 
            && !exe_path.contains("tools") 
            && !exe_path.contains("manager");

        if is_antigravity {
            let args_raw = process.cmd();
            let mut args = Vec::new();
            for arg in args_raw {
                args.push(arg.to_string_lossy().to_string());
            }

            // 如果是 macOS，将路径修正为 .app 根目录
            #[cfg(target_os = "macos")]
            let final_exe = exe_path_buf.and_then(|p| {
                let p_str = p.to_string_lossy();
                if let Some(idx) = p_str.find(".app") {
                    Some(PathBuf::from(&p_str[..idx + 4]))
                } else {
                    Some(p)
                }
            });
            #[cfg(not(target_os = "macos"))]
            let final_exe = exe_path_buf;

            return (final_exe, Some(args));
        }
    }
    (None, None)
}

/// 获取 Antigravity 进程基础信息，供内部使用
fn get_process_info() -> (Option<PathBuf>, Option<PathBuf>) {
    let (exe, args) = get_process_info_for_api();
    let mut user_data_dir = None;
    if let Some(args) = args {
        for i in 0..args.len() {
            if args[i] == "--user-data-dir" && i + 1 < args.len() {
                user_data_dir = Some(PathBuf::from(args[i+1].clone()));
                break;
            } else if args[i].starts_with("--user-data-dir=") {
                if let Some(path_str) = args[i].split('=').nth(1) {
                    user_data_dir = Some(PathBuf::from(path_str.to_string()));
                    break;
                }
            }
        }
    }
    (exe, user_data_dir)
}

/// 获取 Antigravity 可执行文件路径
/// 优先级：1. 配置文件已保存路径 2. 手动环境变量指定 3. 运行进程探测 4. 标准路径轮询
pub fn get_antigravity_executable_path() -> Option<PathBuf> {
    // 1. 尝试从配置文件获取已保存的路径 (持久化设置)
    if let Some(saved) = crate::common::get_saved_antigravity_path() {
        if saved.exists() { 
            info!("🎯 使用持久化配置中的 IDE 路径: {:?}", saved);
            return Some(saved); 
        }
    }

    // 2. 尝试从环境变量获取手动指定路径 (由 cli-server 注入或临时覆盖)
    if let Ok(manual) = std::env::var("ANT_EXECUTABLE_PATH") {
        let path = PathBuf::from(manual);
        if path.exists() { 
            info!(" env: 使用环境变量指定的 IDE 路径: {:?}", path);
            return Some(path); 
        }
    }

    // 2. 尝试从运行中的进程探测
    let (process_path, _) = get_process_info();
    if let Some(path) = process_path {
        if path.exists() { 
            info!("🔍 通过运行中进程探测到 IDE 路径: {:?}", path);
            return Some(path); 
        }
    }

    // 3. 轮询标准安装路径
    #[cfg(target_os = "macos")]
    {
        let path = PathBuf::from("/Applications/Antigravity.app");
        if path.exists() { return Some(path); }
    }
    #[cfg(target_os = "windows")]
    {
        // 依次检查 LocalAppData, Program Files, Program Files (x86)
        let mut possible = Vec::new();
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            possible.push(PathBuf::from(local).join("Programs\\Antigravity\\Antigravity.exe"));
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            possible.push(PathBuf::from(pf).join("Antigravity\\Antigravity.exe"));
        }
        possible.push(PathBuf::from("C:\\Program Files (x86)\\Antigravity\\Antigravity.exe"));

        for p in possible {
            if p.exists() { return Some(p); }
        }
    }
    #[cfg(target_os = "linux")]
    {
        let possible = vec![
            PathBuf::from("/usr/bin/antigravity"),
            PathBuf::from("/opt/Antigravity/antigravity"),
        ];
        for p in possible {
            if p.exists() { return Some(p); }
        }
    }
    None
}

/// 获取 storage.json 路径
pub fn get_storage_path() -> Result<PathBuf, String> {
    // 1. 优先尝试从运行进程中探测数据目录 (最高优先级，支持 --user-data-dir)
    let (exe_path, process_user_data) = get_process_info();
    if let Some(user_data) = process_user_data {
        let storage = user_data.join("User").join("globalStorage").join("storage.json");
        if storage.exists() { return Ok(storage); }
    }

    // 2. 尝试检测便携模式 (基于可执行文件位置)
    if let Some(exe) = exe_path {
        if let Some(parent) = exe.parent() {
            // macOS: Antigravity.app/Contents/MacOS/Antigravity -> ../Resources/app/data/user-data
            #[cfg(target_os = "macos")]
            let portable = parent.parent().and_then(|p| p.join("Resources/app/data/user-data/User/globalStorage/storage.json").into());
            #[cfg(not(target_os = "macos"))]
            let portable = Some(parent.join("data").join("user-data").join("User").join("globalStorage").join("storage.json"));

            if let Some(ref p) = portable {
                if p.exists() { return Ok(p.clone()); }
            }
        }
    }

    // 3. 兜底：标准安装路径
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().ok_or("Failed to get home directory")?;
        Ok(home.join("Library/Application Support/Antigravity/User/globalStorage/storage.json"))
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").map_err(|_| "Failed to get APPDATA".to_string())?;
        Ok(PathBuf::from(appdata).join("Antigravity\\User\\globalStorage\\storage.json"))
    }
    #[cfg(target_os = "linux")]
    {
        let home = dirs::home_dir().ok_or("Failed to get home directory")?;
        Ok(home.join(".config/Antigravity/User/globalStorage/storage.json"))
    }
}

/// 获取 IDE 数据库路径
pub fn get_db_path() -> Result<PathBuf, String> {
    let storage_path = get_storage_path()?;
    Ok(storage_path.with_file_name("state.vscdb"))
}

/// 检查 IDE 是否正在运行
pub fn is_ide_running() -> bool {
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
    let current_pid = std::process::id();

    for process in system.processes().values() {
        let pid = process.pid().as_u32();
        if pid == current_pid {
            continue;
        }

        let name = process.name().to_string_lossy().to_lowercase();
        let exe_path = process.exe().and_then(|p| p.to_str()).unwrap_or("").to_lowercase();

        // 基础过滤：必须包含 antigravity 且不是 helper
        if (name.contains("antigravity") || exe_path.contains("antigravity")) && !name.contains("helper") {
            // 进一步精确匹配，避免识别到本工具 (Antigravity-Tools-LS)
            if name == "antigravity" || exe_path.contains("antigravity.app") || name == "antigravity.exe" {
                // 排除包含 tools 或 manager 的路径，这些通常是管理工具而非 IDE 本身
                if !exe_path.contains("tools") && !exe_path.contains("manager") {
                    return true;
                }
            }
        }
    }
    false
}

/// 关闭 IDE 进程
pub fn close_ide() -> Result<(), String> {
    info!("Closing Antigravity IDE...");
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
    let current_pid = std::process::id();
    let mut found = false;

    for process in system.processes().values() {
        let pid_val = process.pid().as_u32();
        if pid_val == current_pid {
            continue;
        }

        let name = process.name().to_string_lossy().to_lowercase();
        let exe_path = process.exe().and_then(|p| p.to_str()).unwrap_or("").to_lowercase();

        // 使用与 is_ide_running 一致的匹配逻辑
        if (name.contains("antigravity") || exe_path.contains("antigravity")) && !name.contains("helper") {
            if name == "antigravity" || exe_path.contains("antigravity.app") || name == "antigravity.exe" {
                if !exe_path.contains("tools") && !exe_path.contains("manager") {
                    found = true;
                    let pid = process.pid();
                    debug!("Killing IDE process: {} (PID: {})", name, pid);
                    #[cfg(unix)]
                    Command::new("kill").args(["-9", &pid.to_string()]).output().ok();
                    #[cfg(windows)]
                    Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).output().ok();
                }
            }
        }
    }
    if found { thread::sleep(Duration::from_millis(1000)); }
    Ok(())
}

/// 启动 IDE
pub fn start_ide() -> Result<(), String> {
    info!("Starting Antigravity IDE...");
    #[cfg(target_os = "macos")]
    {
        Command::new("open").args(["-a", "Antigravity"]).spawn()
            .map_err(|e| format!("Failed to start IDE: {}", e))?;
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(exe) = get_antigravity_executable_path() {
            Command::new(exe).spawn().map_err(|e| format!("Failed to start IDE: {}", e))?;
        } else {
            warn!("IDE executable not found, please start manually.");
        }
    }
    Ok(())
}

/// 生成随机设备指纹
pub fn generate_profile() -> DeviceProfile {
    let mut rng = rand::thread_rng();
    let mut random_hex = |len: usize| -> String {
        (&mut rng).sample_iter(&Alphanumeric).take(len).map(char::from).collect::<String>().to_lowercase()
    };
    DeviceProfile {
        machine_id: format!("auth0|user_{}", random_hex(32)),
        mac_machine_id: uuid::Uuid::new_v4().to_string(),
        dev_device_id: uuid::Uuid::new_v4().to_string(),
        sqm_id: format!("{{{}}}", uuid::Uuid::new_v4().to_string().to_uppercase()),
    }
}

/// 写入设备指纹到 storage.json
pub fn write_device_profile(profile: &DeviceProfile) -> Result<(), String> {
    let storage_path = get_storage_path()?;
    if !storage_path.exists() { return Ok(()); }
    let content = fs::read_to_string(&storage_path).map_err(|e| e.to_string())?;
    let mut json: serde_json::Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    
    if let Some(obj) = json.as_object_mut() {
        let telemetry = obj.entry("telemetry").or_insert(serde_json::json!({}));
        if let Some(t_obj) = telemetry.as_object_mut() {
            t_obj.insert("machineId".into(), profile.machine_id.clone().into());
            t_obj.insert("macMachineId".into(), profile.mac_machine_id.clone().into());
            t_obj.insert("devDeviceId".into(), profile.dev_device_id.clone().into());
            t_obj.insert("sqmId".into(), profile.sqm_id.clone().into());
        }
        obj.insert("telemetry.machineId".into(), profile.machine_id.clone().into());
        obj.insert("telemetry.macMachineId".into(), profile.mac_machine_id.clone().into());
        obj.insert("telemetry.devDeviceId".into(), profile.dev_device_id.clone().into());
        obj.insert("telemetry.sqmId".into(), profile.sqm_id.clone().into());
        obj.insert("storage.serviceMachineId".into(), profile.dev_device_id.clone().into());
    }

    let updated = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    fs::write(storage_path, updated).map_err(|e| e.to_string())?;
    Ok(())
}

/// 新版格式注入 (>= 1.16.5)
fn inject_new_format(conn: &Connection, account: &Account, expiry: i64) -> Result<(), String> {
    let oauth_info = protobuf::create_oauth_info(&account.token.access_token, &account.token.refresh_token, expiry);
    let oauth_info_b64 = general_purpose::STANDARD.encode(&oauth_info);
    let inner2 = protobuf::encode_string_field(1, &oauth_info_b64);
    let inner1 = [protobuf::encode_string_field(1, "oauthTokenInfoSentinelKey"), protobuf::encode_len_delim_field(2, &inner2)].concat();
    let outer = protobuf::encode_len_delim_field(1, &inner1);
    let outer_b64 = general_purpose::STANDARD.encode(&outer);

    conn.execute("INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?, ?)", ["antigravityUnifiedStateSync.oauthToken", &outer_b64])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 旧版格式注入 (< 1.16.5)
fn inject_old_format(conn: &Connection, account: &Account, expiry: i64) -> Result<(), String> {
    let key = "jetskiStateSync.agentManagerInitState";
    let current_data: Option<String> = conn.query_row("SELECT value FROM ItemTable WHERE key = ?", [key], |row| row.get(0)).ok();
    
    let mut blob = if let Some(data) = current_data {
        general_purpose::STANDARD.decode(&data).map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };

    // 清理旧字段
    blob = protobuf::remove_field(&blob, 1)?; // UserID
    blob = protobuf::remove_field(&blob, 2)?; // Email
    blob = protobuf::remove_field(&blob, 6)?; // OAuthTokenInfo

    // 插入新字段
    let email_field = protobuf::create_email_field(&account.email);
    let oauth_field = protobuf::create_oauth_field(&account.token.access_token, &account.token.refresh_token, expiry);
    let final_data = [blob, email_field, oauth_field].concat();
    let final_b64 = general_purpose::STANDARD.encode(&final_data);

    conn.execute("INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?, ?)", [key, &final_b64])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 注入 Token 到数据库
pub fn inject_token(account: &Account) -> Result<(), String> {
    let db_path = get_db_path()?;
    if !db_path.exists() { return Err("IDE database not found.".into()); }
    let conn = Connection::open(&db_path).map_err(|e| e.to_string())?;
    
    // 计算绝对过期时间：updated_at + expires_in
    let expiry = account.token.updated_at.timestamp() + (account.token.expires_in as i64);

    // 尝试注入两种格式以保证兼容性
    let res_new = inject_new_format(&conn, account, expiry);
    let res_old = inject_old_format(&conn, account, expiry);

    if res_new.is_err() && res_old.is_err() {
        return Err(format!("Injection failed: New({:?}), Old({:?})", res_new.err(), res_old.err()));
    }

    // 写入 Onboarding 标志
    conn.execute("INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?, ?)", ["antigravityOnboarding", "true"]).ok();
    
    Ok(())
}

/// 执行完整的账号切换逻辑
/// 如果成功切换并生成了新指纹，则返回该指纹；如果使用了现有指纹，则返回 None
pub async fn switch_account(account: &Account) -> Result<Option<DeviceProfile>, String> {
    // 1. 关闭 IDE
    if is_ide_running() { close_ide()?; }

    // 2. 指纹同步 (优先使用已有指纹)
    let (profile, is_new) = if let Some(ref p) = account.device_profile {
        info!("Using existing device profile for account: {}", account.email);
        (p.clone(), false)
    } else {
        info!("Generating new device profile for account: {}", account.email);
        (generate_profile(), true)
    };
    
    write_device_profile(&profile)?;

    // 3. 注入 Token
    inject_token(account)?;

    // 4. 同步 serviceMachineId 到数据库
    let db_path = get_db_path()?;
    if db_path.exists() {
        let conn = Connection::open(&db_path).map_err(|e| e.to_string())?;
        conn.execute("INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?, ?)", ["storage.serviceMachineId", &profile.dev_device_id]).ok();
    }

    // 5. 重启 IDE
    start_ide()?;

    if is_new {
        Ok(Some(profile))
    } else {
        Ok(None)
    }
}
