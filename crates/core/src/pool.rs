//! Пул «токенов потоков» (раздел 6.3 ТЗ): задача берёт токены на старте и
//! возвращает по окончании; размер пула меняет регулятор нагрузки.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::sys::Priority;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolState {
    pub budget: usize,
    pub used: usize,
    pub hw_limit: usize,
    pub hw_used: usize,
    /// `false` — мало свободной памяти, новые задачи не запускаются.
    pub memory_ok: bool,
    pub priority: Priority,
}

pub struct TokenPool {
    state: Mutex<PoolState>,
    changed: Condvar,
}

impl TokenPool {
    pub fn new(budget: usize, hw_limit: usize, priority: Priority) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(PoolState {
                budget: budget.max(1),
                used: 0,
                hw_limit,
                hw_used: 0,
                memory_ok: true,
                priority,
            }),
            changed: Condvar::new(),
        })
    }

    pub fn snapshot(&self) -> PoolState {
        *self.state.lock().unwrap()
    }

    pub fn set_limits(&self, budget: usize, hw_limit: usize, memory_ok: bool, priority: Priority) {
        let mut s = self.state.lock().unwrap();
        s.budget = budget.max(1);
        s.hw_limit = hw_limit;
        s.memory_ok = memory_ok;
        s.priority = priority;
        drop(s);
        self.changed.notify_all();
    }

    pub fn try_acquire(self: &Arc<Self>, cpu: usize, hw: bool) -> Option<Permit> {
        let mut s = self.state.lock().unwrap();
        let cpu_fits = s.used + cpu <= s.budget;
        let hw_fits = !hw || s.hw_used < s.hw_limit;
        if !s.memory_ok || !cpu_fits || !hw_fits {
            return None;
        }
        s.used += cpu;
        s.hw_used += hw as usize;
        Some(Permit {
            pool: Arc::clone(self),
            cpu,
            hw,
        })
    }

    /// Ждёт освобождения токенов. `None` — отменено.
    pub fn acquire(self: &Arc<Self>, cpu: usize, cancel: &AtomicBool) -> Option<Permit> {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            if let Some(p) = self.try_acquire(cpu, false) {
                return Some(p);
            }
            let s = self.state.lock().unwrap();
            let _ = self.changed.wait_timeout(s, Duration::from_millis(200));
        }
    }

    /// Ждёт любого изменения пула (освобождения токенов или новых лимитов).
    pub fn wait_change(&self, timeout: Duration) {
        let s = self.state.lock().unwrap();
        let _ = self.changed.wait_timeout(s, timeout);
    }
}

pub struct Permit {
    pool: Arc<TokenPool>,
    cpu: usize,
    hw: bool,
}

impl Permit {
    pub fn cpu(&self) -> usize {
        self.cpu
    }

    pub fn priority(&self) -> Priority {
        self.pool.snapshot().priority
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut s = self.pool.state.lock().unwrap();
        s.used -= self.cpu;
        s.hw_used -= self.hw as usize;
        drop(s);
        self.pool.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_budget_hw_and_memory() {
        let pool = TokenPool::new(4, 1, Priority::Normal);
        let a = pool.try_acquire(3, false).unwrap();
        assert!(pool.try_acquire(2, false).is_none());
        let hw = pool.try_acquire(1, true).unwrap();
        assert!(pool.try_acquire(0, true).is_none(), "лимит HW-сессий");
        drop(a);
        assert_eq!(pool.snapshot().used, 1);

        pool.set_limits(4, 1, false, Priority::Normal);
        assert!(pool.try_acquire(1, false).is_none(), "мало памяти");
        drop(hw);
        assert_eq!(pool.snapshot().used, 0);
        assert_eq!(pool.snapshot().hw_used, 0);
    }

    #[test]
    fn shrinking_budget_blocks_new_permits_only() {
        let pool = TokenPool::new(8, 2, Priority::Normal);
        let held = pool.try_acquire(6, false).unwrap();
        pool.set_limits(2, 2, true, Priority::Low);
        assert!(pool.try_acquire(1, false).is_none());
        drop(held);
        assert!(pool.try_acquire(2, false).is_some());
    }

    #[test]
    fn acquire_returns_none_when_cancelled() {
        let pool = TokenPool::new(1, 0, Priority::Normal);
        let _held = pool.try_acquire(1, false).unwrap();
        let cancel = AtomicBool::new(true);
        assert!(pool.acquire(1, &cancel).is_none());
    }
}
