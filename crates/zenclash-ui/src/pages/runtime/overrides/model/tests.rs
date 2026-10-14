use super::*;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use zenclash_core::{ControlledConfigStore, YamlOverrideCatalog};

struct PreviewFixture {
    root: PathBuf,
    profile: PathBuf,
    controlled: ControlledConfigStore,
}

impl PreviewFixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = loop {
            let path = std::env::temp_dir().join(format!(
                "zenclash-preview-model-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("preview fixture: {error}"),
            }
        };
        let profile = root.join("base.yaml");
        std::fs::write(
            &profile,
            "mixed-port: 7890\nmode: rule\nrules: [MATCH,DIRECT]\n",
        )
        .unwrap();
        let controlled = ControlledConfigStore::new(root.join("controlled"));
        Self {
            root,
            profile,
            controlled,
        }
    }

    fn read(&self, state: &mut OverridesState) -> PreviewRead {
        state
            .start_preview(
                &tokio::runtime::Handle::current(),
                self.profile.clone(),
                self.controlled.clone(),
            )
            .unwrap()
    }
}

impl Drop for PreviewFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[tokio::test]
async fn equal_preview_generations_do_not_accept_a_foreign_models_result() {
    let fixture = PreviewFixture::new();
    let mut first = OverridesState::new(None, YamlOverrideCatalog::default());
    let mut second = OverridesState::new(None, YamlOverrideCatalog::default());
    let first_read = fixture.read(&mut first);
    let second_read = fixture.read(&mut second);
    assert_eq!(first.preview_generation, second.preview_generation);
    assert!(second.complete_preview(first_read.finish().await).is_none());
    assert!(second.is_preview_loading());
    assert!(second.preview().is_none());
    assert!(
        second
            .complete_preview(second_read.finish().await)
            .unwrap()
            .is_ok()
    );
    assert!(!second.is_preview_loading());
    assert!(second.preview().is_some());
}

#[tokio::test]
async fn an_invalidated_read_cannot_clear_a_new_preview_and_blur_keeps_completed_data() {
    let fixture = PreviewFixture::new();
    let mut state = OverridesState::new(None, YamlOverrideCatalog::default());
    let held = state.preview_gate.clone().lock_owned().await;
    let old = fixture.read(&mut state);
    state.invalidate_preview();
    let current = fixture.read(&mut state);
    assert!(state.complete_preview(old.finish().await).is_none());
    assert!(state.is_preview_loading());
    drop(held);
    assert!(
        state
            .complete_preview(current.finish().await)
            .unwrap()
            .is_ok()
    );
    state.cancel_preview();
    assert!(!state.is_preview_loading());
    assert!(state.preview().is_some());
    state.invalidate_preview();
    assert!(state.preview().is_none());
}

#[tokio::test]
async fn dropping_the_preview_owner_cancels_its_gate_waiter_without_retaining_the_owner() {
    let fixture = PreviewFixture::new();
    let mut state = OverridesState::new(None, YamlOverrideCatalog::default());
    let held = state.preview_gate.clone().lock_owned().await;
    let read = fixture.read(&mut state);
    drop(state);
    let completion = read.finish().await;
    assert!(completion.result.is_err());
    assert!(completion.owner.upgrade().is_none());
    drop(held);
}
