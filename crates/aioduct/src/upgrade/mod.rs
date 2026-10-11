//! HTTP upgrade (e.g., WebSocket) support.

mod observation;
mod upgrade_local;
mod upgrade_send;
pub(crate) use observation::{HandoffMetadata, HandoffRequest};

pub use upgrade_local::UpgradedLocal;
pub(crate) use upgrade_local::{UpgradeHandleLocal, on_upgrade_local};
pub(crate) use upgrade_send::on_upgrade;
pub use upgrade_send::{RawUpgraded, RawUpgradedParts, UpgradedSend};
