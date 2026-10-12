//! One exclusive codec boundary using the existing source/render frame slots.

use dmsg_opus_sys::live::{LiveDecoder, LiveEncoder, LiveProfile};
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

pub(super) enum Operation {
    Encode(Zeroizing<Vec<i16>>),
    Decode {
        opus: Option<Zeroizing<Vec<u8>>>,
        reset: bool,
    },
}

pub(super) enum Output {
    Encoded {
        bytes: Zeroizing<Vec<u8>>,
        in_dtx: bool,
    },
    Decoded(Zeroizing<Vec<i16>>),
}

pub(super) struct Completion {
    pub output: Output,
    pub duration: Duration,
    pub finished: Instant,
}

struct Work {
    operation: Operation,
    done: oneshot::Sender<Result<Completion, String>>,
}

pub(super) struct Owner {
    work: Option<mpsc::SyncSender<Work>>,
    pending: Option<oneshot::Receiver<Result<Completion, String>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Owner {
    pub async fn start(
        profile: LiveProfile,
        #[cfg(test)] hold: Option<Hold>,
    ) -> Result<Self, String> {
        let (work, incoming) = mpsc::sync_channel::<Work>(1);
        let (ready, startup) = oneshot::channel();
        let thread = thread::Builder::new()
            .name("m1v-codec".into())
            .spawn(move || {
                let states = LiveEncoder::new(profile).and_then(|encoder| {
                    LiveDecoder::new(profile).map(|decoder| (encoder, decoder))
                });
                let (mut encoder, mut decoder) = match states {
                    Ok(states) => {
                        let _ = ready.send(Ok(()));
                        states
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                while let Ok(Work { operation, done }) = incoming.recv() {
                    let result = (|| {
                        if matches!(&operation, Operation::Decode { reset: true, .. }) {
                            decoder = LiveDecoder::new(profile)?;
                        }
                        let started = Instant::now();
                        #[cfg(test)]
                        if let Some(hold) = &hold {
                            hold.wait(if matches!(&operation, Operation::Encode(_)) {
                                Kind::Encode
                            } else {
                                Kind::Decode
                            });
                        }
                        let output = match operation {
                            Operation::Encode(pcm) => {
                                let packet = encoder.encode(&pcm)?;
                                Output::Encoded {
                                    bytes: Zeroizing::new(packet.bytes),
                                    in_dtx: packet.in_dtx,
                                }
                            }
                            Operation::Decode { opus, .. } => {
                                Output::Decoded(Zeroizing::new(match opus {
                                    Some(opus) => decoder.decode(&opus)?,
                                    None => decoder.conceal()?,
                                }))
                            }
                        };
                        Ok(Completion {
                            output,
                            duration: started.elapsed(),
                            finished: Instant::now(),
                        })
                    })();
                    let failed = result.is_err();
                    let _ = done.send(result); // Unobserved audio is zeroized by Output.
                    if failed {
                        break;
                    } // Advanced codec input is never retried.
                }
            })
            .map_err(|_| "probe codec owner unavailable".to_string())?;
        let owner = Self {
            work: Some(work),
            pending: None,
            thread: Some(thread),
        };
        startup
            .await
            .map_err(|_| "probe codec startup closed".to_string())??;
        Ok(owner)
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn submit(&mut self, operation: Operation) -> Result<(), String> {
        if self.busy() {
            return Err("probe codec slot already occupied".into());
        }
        let (done, receipt) = oneshot::channel();
        self.work
            .as_ref()
            .ok_or("probe codec owner closed")?
            .try_send(Work { operation, done })
            .map_err(|_| "probe codec owner admission failed".to_string())?;
        self.pending = Some(receipt);
        Ok(())
    }

    pub async fn completed(&mut self) -> Result<Completion, String> {
        let result = self
            .pending
            .as_mut()
            .expect("one codec completion owner")
            .await
            .map_err(|_| "probe codec completion closed".to_string());
        self.pending = None;
        result?
    }

    // A ready result gets published before the actor chooses more IO/timer
    // work. This consumes the same one result, not another queue or receipt.
    pub fn try_completed(&mut self) -> Result<Option<Completion>, String> {
        let Some(pending) = &mut self.pending else {
            return Ok(None);
        };
        match pending.try_recv() {
            Ok(result) => {
                self.pending = None;
                result.map(Some)
            }
            Err(oneshot::error::TryRecvError::Empty) => Ok(None),
            Err(_) => {
                self.pending = None;
                Err("probe codec completion closed".into())
            }
        }
    }

    pub fn close(&mut self) -> Result<(), String> {
        self.pending.take();
        self.work.take();
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| "probe codec owner panicked".to_string())?;
        }
        Ok(())
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Encode,
    Decode,
}

#[cfg(test)]
#[derive(Clone)]
pub(super) struct Hold {
    pub kind: Kind,
    pub entered: std::sync::mpsc::SyncSender<()>,
    pub release: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    pub admitted: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub controlled: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

#[cfg(test)]
impl Hold {
    pub fn wait(&self, kind: Kind) {
        if self.kind == kind {
            let _ = self.entered.try_send(());
            let (lock, changed) = &*self.release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = changed.wait(released).unwrap();
            }
        }
    }

    pub fn release(&self) {
        let (lock, changed) = &*self.release;
        *lock.lock().unwrap() = true;
        changed.notify_all();
    }
}
