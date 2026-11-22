use super::*;

fn with_env(env: FakeEnv) -> RunStateStore<FakeEnv> {
    RunStateStore::new(env)
}

mod observations;
mod probing_and_mode;
