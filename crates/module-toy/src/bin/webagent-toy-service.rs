//! Toy module service process; the host passes its generation as the only argument.
use std::sync::Arc;
use webagent_module_service::{Limits, Service};
use webagent_module_toy::{Implementation, ToyHostClient, ToyServer};

fn main() {
    let generation = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(0);
    let result = webagent_module_service::run(
        Limits::default(),
        Arc::new(move |peer| {
            let implementation = Implementation {
                host: ToyHostClient(peer.clone()),
                own_process: true,
                generation,
            };
            vec![Arc::new(ToyServer(Arc::new(implementation))) as Arc<dyn Service>]
        }),
    );
    if result.is_err() {
        std::process::exit(1);
    }
}
