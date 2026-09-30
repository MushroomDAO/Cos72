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

    // Kept alive for the rest of `main` (the `_` prefix only silences the
    // "never read again" lint — this binding is not dropped until the
    // process exits, so the pool stays open for the whole run) even though
    // no route touches it yet: T1.1.1's job is to prove a fresh install
    // already carries the full schema (docs/agent/tasks.md T1.1.1 目标).
    let _store = Cos72Store::open(&module.data_dir().join("cos72.db")).await?;

    let kernel = SdkKernelPort::wire(&module);
    // docs/agent/spec.md「事件」: "启动握手成功后发一次 module.ready（黑盒可
    // 观测的起点）" — after the handshake and the store are both up.
    kernel.emit("module.ready", serde_json::Map::new());

    let capabilities = Arc::new(module.offer().provides.clone());
    let app = router(Cos72State { capabilities });
    module.serve(app).await?;
    Ok(())
}
