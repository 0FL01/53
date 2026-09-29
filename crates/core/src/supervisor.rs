//! Supervisor K1: один инстанс, start/stop/status, reconnect с backoff.
//!
//! Переподключением владеет Rust: фоновая tokio-задача держит цикл
//! connect→hold→reconnect, наружу — только [`Status`] через watch-канал.
//! K1 держит только AUTH (без ENROL — это K2), поэтому сервер закрывает
//! pre-enrol сессию по idle-timeout и супервизор переподключается: это
//! норма скелета и заодно живое доказательство reconnect.

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

use crate::transport::Transport;

/// Параметры подключения супервизора.
#[derive(Clone, Debug)]
pub struct Config {
    /// `127.0.0.1:port` msgd (direct-TCP; DNS-путь — позже за тем же швом).
    pub addr: String,
    /// Ожидаемый домен (AUTH_DOMAIN→WELCOME).
    pub domain: Vec<u8>,
    /// Noise static-публичник сервера (из invite, не секрет).
    pub server_pub: [u8; 32],
}

/// Состояние супервизора (снапшот; подписка — [`Supervisor::subscribe`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Не запущен (или остановлен).
    Stopped,
    /// Пробуем установить канал.
    Connecting,
    /// Канал AUTH→WELCOME установлен и удерживается.
    Connected,
    /// Последняя попытка провалилась; следующая — после backoff.
    Backoff {
        /// Номер подряд идущей неудачной попытки (с 1).
        attempt: u32,
    },
}

/// Один инстанс на процесс: второй `start` без `stop` — Err.
/// Владение Rust: после `start` задача сама переподключается, звонков
/// переподключения снаружи нет — только `stop`.
pub struct Supervisor {
    cfg: Config,
    status_tx: watch::Sender<Status>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Supervisor {
    /// Создать остановленный инстанс.
    pub fn new(cfg: Config) -> Arc<Self> {
        let (status_tx, _) = watch::channel(Status::Stopped);
        Arc::new(Self { cfg, status_tx, task: Mutex::new(None) })
    }

    /// Запустить фоновую задачу. Второй запуск без `stop` — Err.
    pub fn start(&self) -> Result<(), String> {
        let mut slot = self.task.lock().map_err(|_| "supervisor lock".to_string())?;
        if slot.is_some() {
            return Err("supervisor already running".into());
        }
        let cfg = self.cfg.clone();
        let tx = self.status_tx.clone();
        *slot = Some(tokio::spawn(async move { run_loop(cfg, tx).await }));
        Ok(())
    }

    /// Остановить задачу (идемпотентно) и зафиксировать Stopped.
    pub fn stop(&self) {
        if let Ok(mut slot) = self.task.lock() {
            if let Some(h) = slot.take() {
                h.abort();
            }
        }
        let _ = self.status_tx.send_replace(Status::Stopped);
    }

    /// Текущий снапшот состояния.
    pub fn status(&self) -> Status {
        self.status_tx.borrow().clone()
    }

    /// Подписка на изменения статуса (watch-канал).
    pub fn subscribe(&self) -> watch::Receiver<Status> {
        self.status_tx.subscribe()
    }
}

/// Цикл задачи: connect → hold до закрытия → reconnect. Выход — только через
/// abort из `stop` (задача ничем другим не завершается).
async fn run_loop(cfg: Config, tx: watch::Sender<Status>) {
    let mut attempt: u32 = 0;
    loop {
        let _ = tx.send_replace(Status::Connecting);
        match crate::transport::initiate(&cfg.addr, &cfg.server_pub, &cfg.domain).await {
            Ok(mut ch) => {
                attempt = 0;
                let _ = tx.send_replace(Status::Connected);
                // Hold: сервер pre-enrol ничего не шлёт и закроет по idle —
                // recv вернёт Closed, и уйдём на reconnect. Таймаут тика —
                // только чтобы не висеть вечно на мёртвом сокете.
                loop {
                    match tokio::time::timeout(Duration::from_secs(2), ch.recv_frame()).await {
                        Ok(Ok(_)) => break, // pre-enrol серверу отвечать нечем — чужой кадр, переподключиться
                        Ok(Err(_)) => break, // закрыто/бито — переподключиться
                        Err(_) => continue, // idle-тик, канал держим
                    }
                }
                // Пауза перед переподключением живого-канала, чтобы не спиновать
                // по мгновенным закрытиям.
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(_) => {
                attempt = attempt.saturating_add(1);
                let _ = tx.send_replace(Status::Backoff { attempt });
                tokio::time::sleep(Duration::from_millis(backoff_ms(attempt))).await;
            }
        }
    }
}

/// Backoff: 100ms × 2^(attempt-1), кап 5s. Без джиттера (KISS, один инстанс).
fn backoff_ms(attempt: u32) -> u64 {
    let shift = attempt.saturating_sub(1).min(5);
    100u64.saturating_mul(1u64 << shift).min(5000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed_port_cfg() -> Config {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        Config { addr: format!("127.0.0.1:{port}"), domain: b"k1.test".to_vec(), server_pub: [7u8; 32] }
    }

    #[test]
    fn backoff_shape() {
        assert_eq!((backoff_ms(1), backoff_ms(2), backoff_ms(3)), (100, 200, 400));
        assert_eq!(backoff_ms(6), 3200);
        assert_eq!(backoff_ms(100), 3200);
    }

    #[tokio::test]
    async fn single_instance_and_stop() {
        let sv = Supervisor::new(closed_port_cfg());
        assert_eq!(sv.status(), Status::Stopped);
        sv.start().expect("first start");
        assert!(sv.start().is_err(), "second start without stop must fail");
        // Задача крутит connect→backoff на закрытом порту, но не Stopped.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(sv.status() != Status::Stopped, "must be active, got {:?}", sv.status());
        sv.stop();
        assert_eq!(sv.status(), Status::Stopped);
        // После stop можно стартовать заново (тот же инстанс).
        sv.start().expect("restart after stop");
        sv.stop();
        assert_eq!(sv.status(), Status::Stopped);
    }
}
