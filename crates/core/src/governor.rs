//! Уровень нагрузки на железо и алгоритм «Авто» (раздел 6 ТЗ).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::pool::TokenPool;
use crate::sys::{self, Priority};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LoadLevel {
    #[default]
    Auto,
    Low,
    Medium,
    High,
    Max,
    Custom {
        threads: usize,
        low_priority: bool,
        hw_sessions: usize,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct GovernorStatus {
    pub level: LoadLevel,
    pub budget: usize,
    pub threads_total: usize,
    /// Нагрузка от других программ, 0.0–1.0.
    pub external_load: f32,
    pub own_load: f32,
    pub on_battery: bool,
    pub memory_low: bool,
    pub priority: Priority,
}

/// Резерв для системы R = max(1, ⌈0.15·N⌉).
pub fn reserve(n: usize) -> usize {
    (n * 15).div_ceil(100).max(1)
}

pub fn static_budget(level: LoadLevel, n: usize) -> usize {
    let n = n.max(1);
    match level {
        LoadLevel::Auto => (n - reserve(n).min(n - 1)).max(1),
        LoadLevel::Low => n.div_ceil(4),
        LoadLevel::Medium => n.div_ceil(2),
        LoadLevel::High => (3 * n).div_ceil(4),
        LoadLevel::Max => n,
        LoadLevel::Custom { threads, .. } => threads.clamp(1, n),
    }
}

pub fn level_priority(level: LoadLevel) -> Priority {
    match level {
        LoadLevel::Low => Priority::Low,
        LoadLevel::Auto | LoadLevel::Medium => Priority::BelowNormal,
        LoadLevel::High | LoadLevel::Max => Priority::Normal,
        LoadLevel::Custom { low_priority, .. } => {
            if low_priority {
                Priority::Low
            } else {
                Priority::BelowNormal
            }
        }
    }
}

pub fn hw_sessions(level: LoadLevel, external_load: f32, hw_max: usize) -> usize {
    let wanted = match level {
        LoadLevel::Auto => {
            if external_load > 0.5 {
                1
            } else {
                2
            }
        }
        LoadLevel::Low | LoadLevel::Medium => 1,
        LoadLevel::High => 2,
        LoadLevel::Max => hw_max,
        LoadLevel::Custom { hw_sessions, .. } => hw_sessions,
    };
    wanted.min(hw_max)
}

/// Чистая часть алгоритма «Авто» (6.2, шаги 2–4), без замеров.
#[derive(Debug, Clone)]
pub struct AutoController {
    ema: Option<f32>,
    current: Option<usize>,
    below: u32,
    above: u32,
}

impl Default for AutoController {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoController {
    const ALPHA: f32 = 0.3;
    const DOWN_AFTER: u32 = 2;
    const UP_AFTER: u32 = 5;

    pub fn new() -> Self {
        Self {
            ema: None,
            current: None,
            below: 0,
            above: 0,
        }
    }

    pub fn external_load(&self) -> f32 {
        self.ema.unwrap_or(0.0)
    }

    /// `external` — доля всей машины, занятая другими программами (0.0–1.0).
    pub fn step(&mut self, n: usize, external: f32) -> usize {
        let n = n.max(1);
        let external = external.clamp(0.0, 1.0);
        let e = match self.ema {
            Some(prev) => prev + Self::ALPHA * (external - prev),
            None => external,
        };
        self.ema = Some(e);

        let r = reserve(n);
        let max = n.saturating_sub(r).max(1);
        let free = (n as f32 * (1.0 - e)).floor() as usize;
        let target = free.saturating_sub(r).clamp(1, max);

        let Some(current) = self.current else {
            self.current = Some(target);
            return target;
        };
        let next = if target < current {
            self.above = 0;
            self.below += 1;
            if self.below >= Self::DOWN_AFTER {
                self.below = 0;
                target
            } else {
                current
            }
        } else if target > current {
            self.below = 0;
            self.above += 1;
            if self.above >= Self::UP_AFTER {
                self.above = 0;
                let step = n.div_ceil(8).max(1);
                (current + step).min(target)
            } else {
                current
            }
        } else {
            self.below = 0;
            self.above = 0;
            current
        };
        self.current = Some(next);
        next
    }
}

struct Inner {
    level: LoadLevel,
    auto: AutoController,
    limit_on_battery: bool,
    last: Option<GovernorStatus>,
}

pub struct Governor {
    pool: Arc<TokenPool>,
    threads_total: usize,
    hw_max: AtomicUsize,
    inner: Mutex<Inner>,
    children: Mutex<HashSet<u32>>,
    subscribers: Mutex<Vec<Sender<GovernorStatus>>>,
    stop: AtomicBool,
}

impl Governor {
    /// Запускает фоновый замер раз в секунду. `hw_max` — предел HW-сессий из детекта.
    pub fn start(level: LoadLevel, hw_max: usize) -> Arc<Self> {
        let n = thread::available_parallelism().map_or(1, |n| n.get());
        let gov = Arc::new(Self {
            pool: TokenPool::new(
                static_budget(level, n),
                hw_sessions(level, 0.0, hw_max),
                level_priority(level),
            ),
            threads_total: n,
            hw_max: AtomicUsize::new(hw_max),
            inner: Mutex::new(Inner {
                level,
                auto: AutoController::new(),
                limit_on_battery: true,
                last: None,
            }),
            children: Mutex::new(HashSet::new()),
            subscribers: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&gov);
        thread::Builder::new()
            .name("governor".into())
            .spawn(move || sampler(weak))
            .expect("не удалось запустить поток регулятора");
        gov
    }

    pub fn pool(&self) -> &Arc<TokenPool> {
        &self.pool
    }

    pub fn threads_total(&self) -> usize {
        self.threads_total
    }

    pub fn level(&self) -> LoadLevel {
        self.inner.lock().unwrap().level
    }

    pub fn set_level(&self, level: LoadLevel) {
        let mut inner = self.inner.lock().unwrap();
        inner.level = level;
        inner.auto = AutoController::new();
        let snapshot = self.pool.snapshot();
        let budget = static_budget(level, self.threads_total);
        let priority = level_priority(level);
        self.pool.set_limits(
            budget,
            hw_sessions(
                level,
                inner.auto.external_load(),
                self.hw_max.load(Ordering::Relaxed),
            ),
            snapshot.memory_ok,
            priority,
        );
        drop(inner);
        for &pid in self.children.lock().unwrap().iter() {
            sys::set_process_priority(pid, priority);
        }
    }

    /// Предел HW-сессий меняется после детекта кодировщиков.
    pub fn set_hw_max(&self, hw_max: usize) {
        self.hw_max.store(hw_max, Ordering::Relaxed);
        let level = self.level();
        self.set_level(level);
    }

    pub fn set_limit_on_battery(&self, enabled: bool) {
        self.inner.lock().unwrap().limit_on_battery = enabled;
    }

    pub fn register_child(&self, pid: u32) {
        self.children.lock().unwrap().insert(pid);
    }

    pub fn unregister_child(&self, pid: u32) {
        self.children.lock().unwrap().remove(&pid);
    }

    pub fn subscribe(&self) -> Receiver<GovernorStatus> {
        let (tx, rx) = crossbeam_channel::unbounded();
        self.subscribers.lock().unwrap().push(tx);
        rx
    }

    pub fn last_status(&self) -> Option<GovernorStatus> {
        self.inner.lock().unwrap().last.clone()
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    fn tick(&self, total: f32, own: f32, on_battery: bool, memory_low: bool) {
        let mut inner = self.inner.lock().unwrap();
        let n = self.threads_total;
        let external = (total - own).clamp(0.0, 1.0);
        let level = inner.level;
        let mut budget = match level {
            LoadLevel::Auto => inner.auto.step(n, external),
            other => static_budget(other, n),
        };
        if on_battery && inner.limit_on_battery {
            budget = budget.min(static_budget(LoadLevel::Medium, n));
        }
        let external = match level {
            LoadLevel::Auto => inner.auto.external_load(),
            _ => external,
        };
        let priority = level_priority(level);
        self.pool.set_limits(
            budget,
            hw_sessions(level, external, self.hw_max.load(Ordering::Relaxed)),
            !memory_low,
            priority,
        );
        let status = GovernorStatus {
            level,
            budget,
            threads_total: n,
            external_load: external,
            own_load: own,
            on_battery,
            memory_low,
            priority,
        };
        inner.last = Some(status.clone());
        drop(inner);
        self.subscribers
            .lock()
            .unwrap()
            .retain(|tx| tx.send(status.clone()).is_ok());
    }
}

fn sampler(gov: std::sync::Weak<Governor>) {
    let mut system = System::new();
    let me = Pid::from_u32(std::process::id());
    let mut ticks = 0u32;
    let mut on_battery = false;
    loop {
        thread::sleep(Duration::from_secs(1));
        let Some(gov) = gov.upgrade() else { return };
        if gov.stop.load(Ordering::Relaxed) {
            return;
        }
        system.refresh_cpu_usage();
        system.refresh_memory();
        let mut pids: Vec<Pid> = gov
            .children
            .lock()
            .unwrap()
            .iter()
            .map(|&p| Pid::from_u32(p))
            .collect();
        pids.push(me);
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&pids),
            true,
            ProcessRefreshKind::nothing().with_cpu(),
        );
        let n = gov.threads_total as f32;
        let total = system.global_cpu_usage() / 100.0;
        // cpu_usage процесса — в процентах одного ядра, нормализуем к доле машины.
        let own: f32 = pids
            .iter()
            .filter_map(|p| system.process(*p))
            .map(|p| p.cpu_usage() / 100.0 / n)
            .sum();
        let total_mem = system.total_memory();
        let avail = system.available_memory();
        let memory_low =
            total_mem > 0 && (avail < total_mem * 15 / 100 || avail < 1536 * 1024 * 1024);
        // Питание меняется редко — проверяем раз в 10 с.
        if ticks.is_multiple_of(10) {
            on_battery = sys::on_battery().unwrap_or(false);
        }
        ticks = ticks.wrapping_add(1);
        gov.tick(total, own.min(total), on_battery, memory_low);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_levels_match_spec_table() {
        let n = 8;
        assert_eq!(reserve(8), 2);
        assert_eq!(static_budget(LoadLevel::Low, n), 2);
        assert_eq!(static_budget(LoadLevel::Medium, n), 4);
        assert_eq!(static_budget(LoadLevel::High, n), 6);
        assert_eq!(static_budget(LoadLevel::Max, n), 8);
        assert_eq!(static_budget(LoadLevel::Auto, n), 6);
        for level in [
            LoadLevel::Auto,
            LoadLevel::Low,
            LoadLevel::Medium,
            LoadLevel::Max,
        ] {
            assert_eq!(static_budget(level, 1), 1, "{level:?} при N = 1");
        }
        let custom = LoadLevel::Custom {
            threads: 99,
            low_priority: false,
            hw_sessions: 1,
        };
        assert_eq!(static_budget(custom, n), 8);
    }

    #[test]
    fn auto_idle_machine_takes_all_but_reserve() {
        assert_eq!(AutoController::new().step(8, 0.0), 6);
        assert_eq!(AutoController::new().step(16, 0.0), 13);
    }

    #[test]
    fn auto_scales_down_after_two_samples() {
        let mut c = AutoController::new();
        assert_eq!(c.step(8, 0.0), 6);
        // Скачок внешней нагрузки до 100 %: EMA растёт, снижение — со второго замера.
        let first = c.step(8, 1.0);
        assert_eq!(first, 6);
        let second = c.step(8, 1.0);
        assert!(second < 6, "{second}");
        for _ in 0..20 {
            c.step(8, 1.0);
        }
        assert_eq!(c.step(8, 1.0), 1);
    }

    #[test]
    fn auto_scales_up_gradually() {
        let mut c = AutoController::new();
        c.step(8, 1.0);
        let mut seen = vec![];
        for _ in 0..60 {
            seen.push(c.step(8, 0.0));
        }
        // Рост не больше чем на ⌈N/8⌉ = 1 за шаг и не чаще раза в 5 замеров.
        for w in seen.windows(2) {
            assert!(w[1] <= w[0] + 1);
        }
        assert_eq!(*seen.last().unwrap(), 6);
        let changes = seen.windows(2).filter(|w| w[1] != w[0]).count();
        assert!(changes <= 60 / 5);
    }

    #[test]
    fn auto_sawtooth_load_does_not_flap() {
        let mut c = AutoController::new();
        c.step(8, 0.3);
        let mut seen = vec![];
        for i in 0..40 {
            seen.push(c.step(8, if i % 2 == 0 { 0.9 } else { 0.1 }));
        }
        let changes = seen.windows(2).filter(|w| w[1] != w[0]).count();
        assert!(changes <= 4, "{seen:?}");
    }

    #[test]
    fn auto_single_core_never_zero() {
        let mut c = AutoController::new();
        for e in [0.0, 0.5, 1.0, 1.0, 1.0] {
            assert_eq!(c.step(1, e), 1);
        }
    }

    #[test]
    fn hw_sessions_follow_level_and_cap() {
        assert_eq!(hw_sessions(LoadLevel::Auto, 0.7, 4), 1);
        assert_eq!(hw_sessions(LoadLevel::Auto, 0.1, 4), 2);
        assert_eq!(hw_sessions(LoadLevel::Max, 0.0, 3), 3);
        assert_eq!(hw_sessions(LoadLevel::High, 0.0, 1), 1);
        assert_eq!(hw_sessions(LoadLevel::Auto, 0.0, 0), 0);
    }
}
