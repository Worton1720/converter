//! Платформенный слой: приоритеты (FR-12.3), привязка дочерних процессов к приложению
//! (FR-6.3), питание (FR-12.5), тип диска (раздел 6.3), атрибуты файлов (FR-1.2, FR-7.4).

use std::fs::Metadata;
use std::path::Path;
use std::process::{Child, Command};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Priority {
    Normal,
    BelowNormal,
    /// Низкий приоритет CPU и I/O, EcoQoS на Windows 11.
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiskInfo {
    /// Идентификатор физического устройства (для лимита читателей на диск).
    pub key: String,
    /// `None` — определить не удалось (считается HDD).
    pub rotational: Option<bool>,
}

pub use imp::*;

#[cfg(unix)]
pub fn is_hidden(path: &Path, _meta: &Metadata) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

#[cfg(windows)]
pub fn is_hidden(_path: &Path, meta: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN_OR_SYSTEM: u32 = 0x2 | 0x4;
    meta.file_attributes() & HIDDEN_OR_SYSTEM != 0
}

/// Облачный файл, содержимого которого нет на диске (OneDrive Files On-Demand и т.п.).
#[cfg(windows)]
pub fn is_cloud_placeholder(meta: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x40000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x400000;
    meta.file_attributes() & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0
}

#[cfg(unix)]
pub fn is_cloud_placeholder(_meta: &Metadata) -> bool {
    false
}

/// Команда для программы системы (kioclient, busctl…). В AppImage окружение указывает внутрь
/// образа (`LD_LIBRARY_PATH`, `QT_PLUGIN_PATH`, `PATH`…): системная программа с чужими
/// библиотеками падает (`liblzma.so.5: version XZ_5.4 not found`). Части путей, ведущие
/// в образ, убираются; переменная, от которой ничего не осталось, удаляется.
pub fn host_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    if let Some(appdir) = std::env::var_os("APPDIR") {
        for (key, value) in host_env_fixes(&appdir.to_string_lossy(), std::env::vars_os()) {
            match value {
                Some(v) => cmd.env(key, v),
                None => cmd.env_remove(key),
            };
        }
    }
    cmd
}

/// Что поменять в окружении: `(имя, None)` — удалить, `(имя, Some)` — новое значение.
fn host_env_fixes(
    appdir: &str,
    vars: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(std::ffi::OsString, Option<String>)> {
    let appdir = appdir.trim_end_matches('/');
    if appdir.is_empty() {
        return Vec::new();
    }
    let inside = |p: &str| p == appdir || p.starts_with(&format!("{appdir}/"));
    vars.filter_map(|(key, value)| {
        let value = value.to_string_lossy().into_owned();
        if !value.contains(appdir) || key == "APPDIR" || key == "APPIMAGE" {
            return None;
        }
        let kept: Vec<&str> = value
            .split(':')
            .filter(|p| !p.is_empty() && !inside(p))
            .collect();
        Some((key, (!kept.is_empty()).then(|| kept.join(":"))))
    })
    .collect()
}

/// Привязывает ffmpeg к приложению и выставляет приоритет при запуске.
pub fn spawn(cmd: &mut Command, priority: Priority) -> std::io::Result<Child> {
    configure_child(cmd, priority);
    let child = cmd.spawn()?;
    after_spawn(&child, priority);
    Ok(child)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::os::unix::process::CommandExt;

    const IOPRIO_WHO_PROCESS: libc::c_int = 1;
    const IOPRIO_CLASS_BE: libc::c_int = 2;
    const IOPRIO_CLASS_IDLE: libc::c_int = 3;
    const IOPRIO_CLASS_SHIFT: libc::c_int = 13;

    fn nice(p: Priority) -> libc::c_int {
        match p {
            Priority::Normal => 0,
            Priority::BelowNormal => 10,
            Priority::Low => 19,
        }
    }

    fn ioprio(p: Priority) -> libc::c_int {
        match p {
            Priority::Low => IOPRIO_CLASS_IDLE << IOPRIO_CLASS_SHIFT,
            _ => (IOPRIO_CLASS_BE << IOPRIO_CLASS_SHIFT) | 4,
        }
    }

    /// `tid == 0` — текущий поток.
    fn apply_to_thread(tid: libc::pid_t, p: Priority) {
        // nice в Linux задаётся на поток. Без CAP_SYS_NICE приоритет можно только
        // понизить, поэтому ошибки при попытке повысить игнорируются.
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, nice(p));
            libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, tid, ioprio(p));
        }
    }

    pub fn set_current_thread_priority(p: Priority) {
        apply_to_thread(0, p);
    }

    pub(super) fn configure_child(cmd: &mut Command, p: Priority) {
        unsafe {
            cmd.pre_exec(move || {
                // ffmpeg умрёт вместе с потоком, который его запустил (он же ждёт его завершения).
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                // Своя группа процессов: Ctrl+C в терминале не должен убить ffmpeg раньше,
                // чем ядро корректно отменит задачу.
                libc::setpgid(0, 0);
                apply_to_thread(0, p);
                Ok(())
            });
        }
    }

    pub(super) fn after_spawn(_child: &Child, _p: Priority) {}

    /// Меняет приоритет всех потоков запущенного процесса.
    pub fn set_process_priority(pid: u32, p: Priority) {
        let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
            return;
        };
        for task in tasks.flatten() {
            if let Some(tid) = task.file_name().to_str().and_then(|s| s.parse().ok()) {
                apply_to_thread(tid, p);
            }
        }
    }

    pub fn on_battery() -> Option<bool> {
        let entries = std::fs::read_dir("/sys/class/power_supply").ok()?;
        let mut has_mains = false;
        for e in entries.flatten() {
            let dir = e.path();
            let typ = std::fs::read_to_string(dir.join("type")).unwrap_or_default();
            if typ.trim() == "Mains" {
                has_mains = true;
                if std::fs::read_to_string(dir.join("online")).is_ok_and(|s| s.trim() == "1") {
                    return Some(false);
                }
            }
        }
        has_mains.then_some(true)
    }

    pub fn disk_info(path: &Path) -> DiskInfo {
        use std::os::unix::fs::MetadataExt;
        let dev = std::fs::metadata(path).map(|m| m.dev()).unwrap_or(0);
        let (maj, min) = (libc::major(dev), libc::minor(dev));
        let by_dev = format!("/sys/dev/block/{maj}:{min}");
        if let Some(rot) = rotational_in(Path::new(&by_dev)) {
            return DiskInfo {
                key: by_dev,
                rotational: Some(rot),
            };
        }
        // btrfs и device-mapper дают анонимный st_dev — ищем источник монтирования.
        if let Some(name) = mount_source_block_name(path) {
            let sys = format!("/sys/class/block/{name}");
            if let Some(rot) = rotational_in(Path::new(&sys)) {
                return DiskInfo {
                    key: sys,
                    rotational: Some(rot),
                };
            }
        }
        DiskInfo {
            key: by_dev,
            rotational: None,
        }
    }

    fn rotational_in(dev_dir: &Path) -> Option<bool> {
        // У раздела очередь лежит у родительского устройства.
        [
            dev_dir.join("queue/rotational"),
            dev_dir.join("../queue/rotational"),
        ]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim() == "1")
    }

    fn mount_source_block_name(path: &Path) -> Option<String> {
        let path = std::fs::canonicalize(path).ok()?;
        let info = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
        let mut best: Option<(usize, String)> = None;
        for line in info.lines() {
            let Some((left, right)) = line.split_once(" - ") else {
                continue;
            };
            let (Some(mount_point), Some(source)) =
                (left.split(' ').nth(4), right.split(' ').nth(1))
            else {
                continue;
            };
            let mount_point = mount_point.replace("\\040", " ");
            if !source.starts_with("/dev/") || !path.starts_with(&mount_point) {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|(len, _)| mount_point.len() > *len)
            {
                best = Some((mount_point.len(), source.to_string()));
            }
        }
        let source = std::fs::canonicalize(best?.1).ok()?;
        source.file_name()?.to_str().map(str::to_string)
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
mod imp {
    use super::*;

    pub fn set_current_thread_priority(_p: Priority) {}
    pub(super) fn configure_child(_cmd: &mut Command, _p: Priority) {}
    pub(super) fn after_spawn(_child: &Child, _p: Priority) {}
    pub fn set_process_priority(_pid: u32, _p: Priority) {}
    pub fn on_battery() -> Option<bool> {
        None
    }
    pub fn disk_info(_path: &Path) -> DiskInfo {
        DiskInfo {
            key: String::new(),
            rotational: None,
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, GetVolumePathNameW, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery,
        STORAGE_PROPERTY_QUERY, StorageDeviceSeekPenaltyProperty,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    use windows_sys::Win32::System::Threading::{
        BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW, GetCurrentThread, IDLE_PRIORITY_CLASS,
        NORMAL_PRIORITY_CLASS, OpenProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
        PROCESS_SET_INFORMATION, ProcessPowerThrottling, SetPriorityClass, SetProcessInformation,
        SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN, THREAD_PRIORITY_BELOW_NORMAL,
        THREAD_PRIORITY_LOWEST, THREAD_PRIORITY_NORMAL,
    };

    fn priority_class(p: Priority) -> u32 {
        match p {
            Priority::Normal => NORMAL_PRIORITY_CLASS,
            Priority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
            Priority::Low => IDLE_PRIORITY_CLASS,
        }
    }

    pub fn set_current_thread_priority(p: Priority) {
        unsafe {
            let t = GetCurrentThread();
            match p {
                Priority::Normal => SetThreadPriority(t, THREAD_PRIORITY_NORMAL),
                Priority::BelowNormal => SetThreadPriority(t, THREAD_PRIORITY_BELOW_NORMAL),
                Priority::Low => {
                    // Фоновый режим снижает и приоритет I/O. Поток живёт ровно одну задачу.
                    SetThreadPriority(t, THREAD_MODE_BACKGROUND_BEGIN);
                    SetThreadPriority(t, THREAD_PRIORITY_LOWEST)
                }
            };
        }
    }

    pub(super) fn configure_child(cmd: &mut Command, p: Priority) {
        cmd.creation_flags(CREATE_NO_WINDOW | priority_class(p));
    }

    /// Job Object с KILL_ON_JOB_CLOSE: если приложение упадёт, ffmpeg завершится вместе с ним.
    fn job() -> Option<HANDLE> {
        struct Job(HANDLE);
        unsafe impl Send for Job {}
        unsafe impl Sync for Job {}
        static JOB: OnceLock<Option<Job>> = OnceLock::new();
        JOB.get_or_init(|| unsafe {
            let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if h.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            (ok != 0).then_some(Job(h))
        })
        .as_ref()
        .map(|j| j.0)
    }

    fn set_eco_qos(process: HANDLE, enabled: bool) {
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: if enabled {
                PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            } else {
                0
            },
        };
        unsafe {
            SetProcessInformation(
                process,
                ProcessPowerThrottling,
                &state as *const _ as *const _,
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            );
        }
    }

    pub(super) fn after_spawn(child: &Child, p: Priority) {
        let h = child.as_raw_handle() as HANDLE;
        if let Some(job) = job() {
            unsafe { AssignProcessToJobObject(job, h) };
        }
        if p == Priority::Low {
            set_eco_qos(h, true);
        }
    }

    pub fn set_process_priority(pid: u32, p: Priority) {
        unsafe {
            let h = OpenProcess(PROCESS_SET_INFORMATION, 0, pid);
            if h.is_null() {
                return;
            }
            SetPriorityClass(h, priority_class(p));
            set_eco_qos(h, p == Priority::Low);
            CloseHandle(h);
        }
    }

    pub fn on_battery() -> Option<bool> {
        let mut s: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        if unsafe { GetSystemPowerStatus(&mut s) } == 0 {
            return None;
        }
        const NO_BATTERY: u8 = 128;
        match s.ACLineStatus {
            _ if s.BatteryFlag & NO_BATTERY != 0 => Some(false),
            0 => Some(true),
            1 => Some(false),
            _ => None,
        }
    }

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    pub fn disk_info(path: &Path) -> DiskInfo {
        let mut volume = [0u16; 512];
        let ok = unsafe {
            GetVolumePathNameW(
                wide(path.as_os_str()).as_ptr(),
                volume.as_mut_ptr(),
                volume.len() as u32,
            )
        };
        if ok == 0 {
            return DiskInfo {
                key: String::new(),
                rotational: None,
            };
        }
        let len = volume.iter().position(|&c| c == 0).unwrap_or(0);
        let volume = String::from_utf16_lossy(&volume[..len]);
        let rotational = seek_penalty(&volume);
        DiskInfo {
            key: volume,
            rotational,
        }
    }

    /// `volume` вида `C:\` → устройство `\\.\C:`.
    fn seek_penalty(volume: &str) -> Option<bool> {
        let device = format!(r"\\.\{}", volume.trim_end_matches('\\'));
        let device = wide(std::ffi::OsStr::new(&device));
        unsafe {
            let h = CreateFileW(
                device.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if h == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut query: STORAGE_PROPERTY_QUERY = std::mem::zeroed();
            query.PropertyId = StorageDeviceSeekPenaltyProperty;
            query.QueryType = PropertyStandardQuery;
            let mut out: DEVICE_SEEK_PENALTY_DESCRIPTOR = std::mem::zeroed();
            let mut returned = 0u32;
            let ok = DeviceIoControl(
                h,
                IOCTL_STORAGE_QUERY_PROPERTY,
                &query as *const _ as *const _,
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                &mut out as *mut _ as *mut _,
                std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            );
            CloseHandle(h);
            (ok != 0).then_some(out.IncursSeekPenalty)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_env_drops_appimage_paths() {
        let v = |k: &str, v: &str| (std::ffi::OsString::from(k), std::ffi::OsString::from(v));
        let mut fixes = host_env_fixes(
            "/tmp/.mount_X/",
            [
                v(
                    "LD_LIBRARY_PATH",
                    "/tmp/.mount_X/usr/lib/:/tmp/.mount_X/usr/lib64",
                ),
                v("PATH", "/tmp/.mount_X/usr/bin/:/usr/bin:/bin"),
                v("HOME", "/home/u"),
                v("APPDIR", "/tmp/.mount_X"),
                v("OTHER", "/tmp/.mount_Xother"),
            ]
            .into_iter(),
        );
        fixes.sort();
        assert_eq!(
            fixes,
            [
                ("LD_LIBRARY_PATH".into(), None),
                ("OTHER".into(), Some("/tmp/.mount_Xother".into())),
                ("PATH".into(), Some("/usr/bin:/bin".into())),
            ]
        );
    }

    use super::*;

    #[test]
    fn disk_info_resolves_for_current_dir() {
        let info = disk_info(Path::new(env!("CARGO_MANIFEST_DIR")));
        assert!(!info.key.is_empty());
    }
}
