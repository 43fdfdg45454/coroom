//! Contabilidad y cuota del relay.
//!
//! Los bytes que el relay reenvía en una sala se cargan al **anfitrión** de la sala, por día UTC
//! (`relay_usage`). La carga es síncrona y en memoria ([`Quota::charge`], en el camino de cada
//! datagrama) y una tarea la vuelca a SQLite cada [`FLUSH_PERIOD`] ([`Quota::flush`]). Con
//! `COROOM_RELAY_QUOTA_GB_PER_MONTH > 0`, el total del mes en curso (lo de la base más lo
//! pendiente) se compara con la cuota en cada carga: al agotarla, [`Quota::charge`] devuelve
//! `false` y el relay corta la sala; las salas nuevas de ese anfitrión se crean sin relay
//! ([`Quota::exceeded`]). Nunca se guarda nada del contenido, solo bytes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use tracing::warn;

use crate::clock;
use crate::db::{Db, DbResult};

/// Cada cuánto se vuelca lo pendiente a la base.
pub const FLUSH_PERIOD: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
struct Inner {
    /// Primer día del mes al que se refieren los totales.
    month: i64,
    /// Total del mes por usuario (base al cargar + cargas desde entonces).
    month_total: HashMap<i64, u64>,
    /// Bytes sin volcar por (usuario, día).
    pending: HashMap<(i64, i64), u64>,
}

impl Inner {
    fn roll(&mut self, today: i64) {
        let (first, _) = clock::month_range(today);
        if first != self.month {
            self.month = first;
            self.month_total.clear();
        }
    }
}

/// Cuota y contabilidad del relay.
#[derive(Debug)]
pub struct Quota {
    db: Db,
    quota_bytes: Option<u64>,
    inner: Mutex<Inner>,
}

impl Quota {
    /// Contabilidad sobre `db` con la cuota mensual `quota_bytes` (`None` = sin límite).
    pub fn new(db: Db, quota_bytes: Option<u64>) -> Self {
        Self {
            db,
            quota_bytes,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Cuota mensual en bytes.
    pub fn quota_bytes(&self) -> Option<u64> {
        self.quota_bytes
    }

    /// Carga el total del mes de `user` desde la base si aún no estaba en memoria.
    pub async fn load(&self, user: i64) -> DbResult<()> {
        let today = clock::day_of(clock::now());
        let loaded = self
            .inner
            .lock()
            .map(|mut inner| {
                inner.roll(today);
                inner.month_total.contains_key(&user)
            })
            .unwrap_or(false);
        if loaded {
            return Ok(());
        }
        let (first, last) = clock::month_range(today);
        let stored = self.db.usage_sum(user, first, last).await?;
        if let Ok(mut inner) = self.inner.lock() {
            inner.roll(today);
            let pending: u64 = inner
                .pending
                .iter()
                .filter(|((u, day), _)| *u == user && *day >= first && *day <= last)
                .map(|(_, bytes)| *bytes)
                .sum();
            inner
                .month_total
                .entry(user)
                .or_insert(stored.saturating_add(pending));
        }
        Ok(())
    }

    /// Total del mes en curso de `user` (lo cargado y lo pendiente).
    pub fn month_total(&self, user: i64) -> u64 {
        let today = clock::day_of(clock::now());
        self.inner
            .lock()
            .map(|mut inner| {
                inner.roll(today);
                inner.month_total.get(&user).copied().unwrap_or(0)
            })
            .unwrap_or(0)
    }

    /// Si `user` ya agotó la cuota del mes (llamar antes a [`Quota::load`]).
    pub fn exceeded(&self, user: i64) -> bool {
        self.quota_bytes
            .is_some_and(|quota| self.month_total(user) >= quota)
    }

    /// Carga `bytes` a `user` hoy; `false` si con ellos agota la cuota.
    pub fn charge(&self, user: i64, bytes: u64) -> bool {
        let today = clock::day_of(clock::now());
        let Ok(mut inner) = self.inner.lock() else {
            return true;
        };
        inner.roll(today);
        *inner.pending.entry((user, today)).or_insert(0) += bytes;
        let total = inner.month_total.entry(user).or_insert(0);
        *total = total.saturating_add(bytes);
        self.quota_bytes.is_none_or(|quota| *total < quota)
    }

    /// Bytes de hoy sin volcar todavía (de todos los usuarios).
    pub fn pending_today(&self) -> u64 {
        let today = clock::day_of(clock::now());
        self.inner
            .lock()
            .map(|inner| {
                inner
                    .pending
                    .iter()
                    .filter(|((_, day), _)| *day == today)
                    .map(|(_, bytes)| *bytes)
                    .sum()
            })
            .unwrap_or(0)
    }

    /// Bytes sin volcar de `user` por día.
    pub fn pending_of(&self, user: i64) -> Vec<(i64, u64)> {
        self.inner
            .lock()
            .map(|inner| {
                inner
                    .pending
                    .iter()
                    .filter(|((u, _), _)| *u == user)
                    .map(|((_, day), bytes)| (*day, *bytes))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Bytes sin volcar de todos, por usuario, entre `first` y `last`.
    pub fn pending_between(&self, first: i64, last: i64) -> HashMap<i64, u64> {
        let mut out = HashMap::new();
        if let Ok(inner) = self.inner.lock() {
            for ((user, day), bytes) in &inner.pending {
                if *day >= first && *day <= last {
                    *out.entry(*user).or_insert(0) += *bytes;
                }
            }
        }
        out
    }

    /// Vuelca lo pendiente a la base (lo que falle se vuelve a intentar en el siguiente).
    pub async fn flush(&self) {
        let pending: Vec<((i64, i64), u64)> = match self.inner.lock() {
            Ok(mut inner) => inner.pending.drain().collect(),
            Err(_) => return,
        };
        for ((user, day), bytes) in pending {
            if let Err(err) = self.db.add_usage(user, day, bytes).await {
                warn!(error = %err, "no se pudo guardar el uso del relay; se reintenta");
                if let Ok(mut inner) = self.inner.lock() {
                    *inner.pending.entry((user, day)).or_insert(0) += bytes;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn charges_accumulate_and_the_quota_cuts() {
        let dir = tempfile::tempdir().expect("directorio temporal");
        let db = Db::open(&dir.path().join("coroom.db")).await.expect("base");
        let ana = db.create_user("ana", "Ana", "h", false).await.expect("ana");
        let today = clock::day_of(clock::now());
        db.add_usage(ana.id, today, 600).await.expect("uso previo");

        let quota = Quota::new(db.clone(), Some(1_000));
        quota.load(ana.id).await.expect("carga");
        assert_eq!(quota.month_total(ana.id), 600);
        assert!(!quota.exceeded(ana.id));
        assert!(quota.charge(ana.id, 300));
        assert!(!quota.charge(ana.id, 100), "llega a la cuota");
        assert!(quota.exceeded(ana.id));
        assert_eq!(quota.pending_today(), 400);
        quota.flush().await;
        assert_eq!(quota.pending_today(), 0);
        assert_eq!(
            db.usage_sum(ana.id, today, today).await.expect("suma"),
            1_000
        );
        // Una contabilidad nueva (reinicio) lee el total de la base.
        let again = Quota::new(db, Some(1_000));
        again.load(ana.id).await.expect("carga");
        assert!(again.exceeded(ana.id));
        let unlimited = Quota::new(again.db.clone(), None);
        assert!(unlimited.charge(ana.id, u64::MAX / 2));
        assert!(!unlimited.exceeded(ana.id));
    }
}
