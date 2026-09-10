//! Thin launcher. All application code lives in `tod-ui` (GUI), `tod-core`
//! (policy and orchestration), `tod-agent` (agent transport), and `tod-store`
//! (persistence).

fn main() {
    tod_ui::run();
}
