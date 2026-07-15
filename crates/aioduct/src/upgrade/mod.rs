//! HTTP upgrade (e.g., WebSocket) support.

mod upgrade_local;
mod upgrade_send;

pub use upgrade_local::UpgradedLocal;
pub(crate) use upgrade_local::{UpgradeHandleLocal, on_upgrade_local};
pub use upgrade_send::UpgradedSend;
pub(crate) use upgrade_send::on_upgrade;
