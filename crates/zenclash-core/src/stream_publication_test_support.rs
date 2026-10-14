//! Per-thread scheduling pauses for real stream-publication regressions.

use std::{cell::RefCell, sync::mpsc, time::Duration};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicationPhase {
    BeforeStateLock,
    BeforeBufferAppend,
}

pub(crate) struct PublicationPause {
    phase: PublicationPhase,
    reached: mpsc::SyncSender<()>,
    resume: mpsc::Receiver<()>,
}

pub(crate) struct PublicationPauseController {
    reached: mpsc::Receiver<()>,
    resume: mpsc::SyncSender<()>,
}

thread_local! {
    static PAUSE: RefCell<Option<PublicationPause>> = const { RefCell::new(None) };
}

pub(crate) fn publication_pause(
    phase: PublicationPhase,
) -> (PublicationPause, PublicationPauseController) {
    let (reached, reached_receiver) = mpsc::sync_channel(1);
    let (resume_sender, resume) = mpsc::sync_channel(1);
    (
        PublicationPause {
            phase,
            reached,
            resume,
        },
        PublicationPauseController {
            reached: reached_receiver,
            resume: resume_sender,
        },
    )
}

impl PublicationPause {
    pub(crate) fn install(self) {
        PAUSE.with(|pause| {
            assert!(pause.borrow().is_none());
            *pause.borrow_mut() = Some(self);
        });
    }
}

impl PublicationPauseController {
    pub(crate) fn wait_until_reached(&self) {
        self.reached.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    pub(crate) fn resume(self) {
        drop(self);
    }
}

impl Drop for PublicationPauseController {
    fn drop(&mut self) {
        // Also release the writer if its test fails while it is paused.
        let _ = self.resume.try_send(());
    }
}

pub(crate) fn pause_at(phase: PublicationPhase) {
    let pause = PAUSE.with(|pause| {
        let mut pause = pause.borrow_mut();
        if pause.as_ref().is_some_and(|pause| pause.phase == phase) {
            pause.take()
        } else {
            None
        }
    });
    if let Some(pause) = pause {
        pause.reached.send(()).unwrap();
        pause.resume.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}
