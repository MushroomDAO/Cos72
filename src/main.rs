//! Cos72 binary entry point — spawned by `agent24d` as an out-of-process
//! module (`domain-os.yml`'s `spawn: {command: bin/cos72, args: ["module"]}`).
//!
//! T1.1.1 scope (docs/agent/tasks.md「开发范围」): connect the handshake,
//! open + migrate the store, wire `emit`, serve `GET /health`. No business
//! routes yet (T1.2.1), no standalone mode (docs/agent/architecture.md「运行
//! 形态」: "不提供 standalone 模式").

use std::sync::Arc;

use agent24_os_sdk::Module;
use cos72::http::{router, Cos72State};
use cos72::kernel::{KernelPort, SdkKernelPort};
use cos72::store::Cos72Store;
use cos72::MANIFEST;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let module = Module::builder(MANIFEST).connect().await?;
    tracing::info!(offer = ?module.offer().provides, "cos72: handshake accepted");

    let store = Cos72Store::open(&module.data_dir().join("cos72.db")).await?;

    let kernel = Arc::new(SdkKernelPort::wire(&module));
    // docs/agent/spec.md「事件」: "启动握手成功后发一次 module.ready（黑盒可
    // 观测的起点）" — after the handshake and the store are both up.
    kernel.emit("module.ready", serde_json::Map::new());

    // docs/agent/architecture.md 不可动摇的边界 #6: only spawn the poller
    // when the kernel actually granted `approval` — a generation without it
    // has nothing for the poller to do (every submit already answers 503
    // and writes no `awaiting` row for it to find).
    if kernel.approval_available() {
        // The `JoinHandle` outlives nothing this process cares about — the
        // task itself only stops via `TickControl::Stop`/a store error loop
        // exit, at which point the SDK's own fatal hook has already (or is
        // about to) `exit(70)` this generation.
        drop(cos72::workers::award_poller::spawn_loop(
            store.clone(),
            kernel.clone(),
        ));
    }

    // docs/agent/spec.md「记忆泵」: only spawn the pump when the kernel
    // actually granted `memory` — with no memory capability there is
    // nothing for it to drain into, and any `outbox` rows a future award
    // credits simply stay `pending` (spec.md: "不报错").
    if kernel.memory_available() {
        drop(cos72::workers::memory_pump::spawn_loop(
            store.clone(),
            kernel.clone(),
        ));
    }

    let capabilities = Arc::new(module.offer().provides.clone());
    let app = router(Cos72State {
        capabilities,
        store,
        kernel,
    });
    module.serve(app).await?;
    Ok(())
}
